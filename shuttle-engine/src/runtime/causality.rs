use crate::runtime::task::clock::VectorClock;
use crate::runtime::task::TaskId;
use std::any::{Any, TypeId};
use std::fmt;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

static NEXT_EXECUTION_ID: AtomicU64 = AtomicU64::new(1);
static CAUSAL_STATE_ENABLED: OnceLock<bool> = OnceLock::new();

pub(crate) fn initialize_global_causal_state(enabled: bool) {
    let installed = *CAUSAL_STATE_ENABLED.get_or_init(|| enabled);
    assert_eq!(
        installed, enabled,
        "causal state was globally initialized as {installed}, but this runtime requested {enabled}"
    );
}

#[inline]
pub(crate) fn causal_state_enabled() -> bool {
    CAUSAL_STATE_ENABLED.get().copied().unwrap_or(false)
}

/// An identifier that distinguishes one Shuttle execution from another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ExecutionId(u64);

impl ExecutionId {
    pub(crate) fn fresh() -> Self {
        let id = NEXT_EXECUTION_ID.fetch_add(1, Ordering::Relaxed);
        assert_ne!(id, 0, "execution ID overflow");
        Self(id)
    }
}

/// A snapshot of the causal information associated with a task.
///
/// The fields are private so synchronization implementations cannot update the
/// vector clock and application-defined state independently.
#[derive(Clone)]
pub struct CausalSnapshot {
    execution_id: Option<ExecutionId>,
    clock: VectorClock,
    custom: Option<Arc<ErasedSnapshot>>,
}

impl CausalSnapshot {
    /// Construct an empty snapshot with no associated execution.
    #[inline]
    pub const fn new() -> Self {
        Self {
            execution_id: None,
            clock: VectorClock::new(),
            custom: None,
        }
    }

    #[inline]
    pub(crate) fn from_clock(execution_id: ExecutionId, clock: VectorClock) -> Self {
        Self {
            execution_id: Some(execution_id),
            clock,
            custom: None,
        }
    }

    /// Add another snapshot to a snapshot retained by a synchronization object.
    ///
    /// An empty snapshot adopts the incoming execution. A snapshot from a newer
    /// execution replaces retained state from an older execution. A snapshot
    /// from an older execution is ignored.
    #[inline]
    pub fn update(&mut self, other: &Self) {
        let Some(other_execution_id) = other.execution_id else {
            return;
        };

        if self.execution_id != Some(other_execution_id) {
            if self
                .execution_id
                .map(|execution_id| execution_id < other_execution_id)
                .unwrap_or(true)
            {
                *self = other.clone();
            }
            return;
        }

        self.clock.update(&other.clock);

        match (&mut self.custom, &other.custom) {
            (_, None) => {}
            (slot @ None, Some(incoming)) => *slot = Some(Arc::clone(incoming)),
            (Some(current), Some(incoming)) => Arc::make_mut(current).update(incoming),
        }
    }

    pub(crate) fn belongs_to(&self, execution_id: ExecutionId) -> bool {
        self.execution_id == Some(execution_id)
    }

    pub(crate) fn clock(&self) -> &VectorClock {
        &self.clock
    }
}

impl Default for CausalSnapshot {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for CausalSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CausalSnapshot").finish_non_exhaustive()
    }
}

/// A snapshot of application-defined causal state.
///
/// Unlike [`CausalSnapshot`], this snapshot does not contain a vector clock.
#[derive(Clone)]
pub struct ApplicationStateSnapshot {
    custom: Option<Arc<ErasedSnapshot>>,
}

impl ApplicationStateSnapshot {
    /// Construct an empty snapshot with no associated execution.
    #[inline]
    pub const fn new() -> Self {
        Self { custom: None }
    }

    /// Add another snapshot to a snapshot retained by a synchronization object.
    ///
    /// An empty snapshot adopts the incoming execution. A snapshot from a newer
    /// execution replaces retained state from an older execution. A snapshot
    /// from an older execution is ignored.
    #[inline]
    pub fn update(&mut self, other: &Self) {
        let Some(incoming) = &other.custom else {
            return;
        };

        match &mut self.custom {
            None => self.custom = Some(Arc::clone(incoming)),
            Some(current) if current.execution_id != incoming.execution_id => {
                if current.execution_id < incoming.execution_id {
                    self.custom = Some(Arc::clone(incoming));
                }
            }
            Some(current) => Arc::make_mut(current).update(incoming),
        }
    }

    pub(crate) fn belongs_to(&self, execution_id: ExecutionId) -> bool {
        self.custom
            .as_ref()
            .is_some_and(|snapshot| snapshot.execution_id == execution_id)
    }
}

impl Default for ApplicationStateSnapshot {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ApplicationStateSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApplicationStateSnapshot").finish_non_exhaustive()
    }
}

#[derive(Clone, Default)]
pub(crate) struct CausalStateConfig {
    ops: Option<Arc<dyn ErasedCausalState>>,
}

impl CausalStateConfig {
    pub(crate) fn set<T, I, J>(&mut self, initial: I, join: J)
    where
        T: Clone + Send + Sync + 'static,
        I: Fn() -> T + Send + Sync + 'static,
        J: Fn(&mut T, &T) + Send + Sync + 'static,
    {
        assert!(self.ops.is_none(), "only one causal state type may be configured");
        self.ops = Some(Arc::new(TypedCausalState {
            initial,
            join,
            state: PhantomData,
        }));
    }

    pub(crate) fn execution_state(&self) -> Option<Box<ExecutionCausalState>> {
        self.ops.as_ref().map(|ops| {
            Box::new(ExecutionCausalState {
                ops: Arc::clone(ops),
                task_states: Vec::new(),
            })
        })
    }

    #[inline]
    pub(crate) fn is_enabled(&self) -> bool {
        self.ops.is_some()
    }
}

pub(crate) struct ExecutionCausalState {
    ops: Arc<dyn ErasedCausalState>,
    task_states: Vec<CausalStateValue>,
}

impl ExecutionCausalState {
    pub(crate) fn add_task(&mut self, task_id: TaskId, parent: Option<TaskId>) {
        assert_eq!(self.task_states.len(), task_id.0);
        let state = parent
            .map(|parent| self.task_states[parent.0].clone())
            .unwrap_or_else(|| CausalStateValue(self.ops.initialize()));
        self.task_states.push(state);
    }

    pub(crate) fn capture(&self, execution_id: ExecutionId, task_id: TaskId, clock: VectorClock) -> CausalSnapshot {
        CausalSnapshot {
            execution_id: Some(execution_id),
            clock,
            custom: Some(Arc::new(ErasedSnapshot {
                execution_id,
                ops: Arc::clone(&self.ops),
                value: self.task_states[task_id.0].0.clone(),
            })),
        }
    }

    pub(crate) fn capture_application_state(
        &self,
        execution_id: ExecutionId,
        task_id: TaskId,
    ) -> ApplicationStateSnapshot {
        ApplicationStateSnapshot {
            custom: Some(Arc::new(ErasedSnapshot {
                execution_id,
                ops: Arc::clone(&self.ops),
                value: self.task_states[task_id.0].0.clone(),
            })),
        }
    }

    pub(crate) fn merge_causal_snapshot(&mut self, task_id: TaskId, snapshot: &CausalSnapshot) {
        if let Some(incoming) = &snapshot.custom {
            self.merge(task_id, incoming);
        }
    }

    pub(crate) fn merge_application_snapshot(&mut self, task_id: TaskId, snapshot: &ApplicationStateSnapshot) {
        if let Some(incoming) = &snapshot.custom {
            self.merge(task_id, incoming);
        }
    }

    fn merge(&mut self, task_id: TaskId, incoming: &ErasedSnapshot) {
        assert!(
            Arc::ptr_eq(&self.ops, &incoming.ops),
            "causal state configuration changed within an execution"
        );
        self.ops
            .join_values(self.task_states[task_id.0].0.as_mut(), incoming.value.as_ref());
    }

    pub(crate) fn state<T>(&self, task_id: TaskId) -> T
    where
        T: Clone + Send + Sync + 'static,
    {
        assert_eq!(
            self.ops.state_type_id(),
            TypeId::of::<T>(),
            "requested type is not the configured causal state"
        );
        self.task_states[task_id.0]
            .0
            .as_ref()
            .as_any()
            .downcast_ref::<T>()
            .expect("current task has an unexpected causal state type")
            .clone()
    }

    pub(crate) fn join_state<T>(&mut self, task_id: TaskId, incoming: T)
    where
        T: Clone + Send + Sync + 'static,
    {
        assert_eq!(
            self.ops.state_type_id(),
            TypeId::of::<T>(),
            "joined type is not the configured causal state"
        );
        self.ops.join_values(self.task_states[task_id.0].0.as_mut(), &incoming);
    }
}

impl fmt::Debug for CausalStateConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = f.debug_struct("CausalStateConfig");
        debug.field("type", &self.ops.as_ref().map(|ops| ops.state_type_name()));
        debug.finish()
    }
}

#[derive(Clone)]
pub(crate) struct CausalStateValue(Box<dyn AnyCausalValue>);

impl fmt::Debug for CausalStateValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CausalStateValue").finish_non_exhaustive()
    }
}

trait AnyCausalValue: Send + Sync {
    fn clone_box(&self) -> Box<dyn AnyCausalValue>;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

impl<T> AnyCausalValue for T
where
    T: Clone + Send + Sync + 'static,
{
    fn clone_box(&self) -> Box<dyn AnyCausalValue> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl Clone for Box<dyn AnyCausalValue> {
    fn clone(&self) -> Self {
        (**self).clone_box()
    }
}

trait ErasedCausalState: Send + Sync {
    fn state_type_id(&self) -> TypeId;
    fn state_type_name(&self) -> &'static str;
    fn initialize(&self) -> Box<dyn AnyCausalValue>;
    fn join_values(&self, current: &mut dyn AnyCausalValue, incoming: &dyn AnyCausalValue);
}

struct TypedCausalState<T, I, J> {
    initial: I,
    join: J,
    state: PhantomData<fn() -> T>,
}

impl<T, I, J> ErasedCausalState for TypedCausalState<T, I, J>
where
    T: Clone + Send + Sync + 'static,
    I: Fn() -> T + Send + Sync + 'static,
    J: Fn(&mut T, &T) + Send + Sync + 'static,
{
    fn state_type_id(&self) -> TypeId {
        TypeId::of::<T>()
    }

    fn state_type_name(&self) -> &'static str {
        std::any::type_name::<T>()
    }

    fn initialize(&self) -> Box<dyn AnyCausalValue> {
        Box::new((self.initial)())
    }

    fn join_values(&self, current: &mut dyn AnyCausalValue, incoming: &dyn AnyCausalValue) {
        let current = current
            .as_any_mut()
            .downcast_mut::<T>()
            .expect("causal snapshot had an unexpected current type");
        let incoming = incoming
            .as_any()
            .downcast_ref::<T>()
            .expect("causal snapshot had an unexpected incoming type");
        (self.join)(current, incoming);
    }
}

#[derive(Clone)]
struct ErasedSnapshot {
    execution_id: ExecutionId,
    ops: Arc<dyn ErasedCausalState>,
    value: Box<dyn AnyCausalValue>,
}

impl ErasedSnapshot {
    fn update(&mut self, incoming: &Self) {
        assert!(
            Arc::ptr_eq(&self.ops, &incoming.ops),
            "cannot join causal snapshots from different configurations"
        );
        self.ops.join_values(self.value.as_mut(), incoming.value.as_ref());
    }
}
