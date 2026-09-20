use crate::runtime::task::clock::VectorClock;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "causal-state")]
use std::any::{Any, TypeId};
#[cfg(feature = "causal-state")]
use std::marker::PhantomData;
#[cfg(feature = "causal-state")]
use std::sync::Arc;

static NEXT_EXECUTION_ID: AtomicU64 = AtomicU64::new(1);

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
    #[cfg(feature = "causal-state")]
    custom: Option<ErasedSnapshot>,
}

impl CausalSnapshot {
    /// Construct an empty snapshot with no associated execution.
    #[inline]
    pub const fn new() -> Self {
        Self {
            execution_id: None,
            clock: VectorClock::new(),
            #[cfg(feature = "causal-state")]
            custom: None,
        }
    }

    #[inline]
    pub(crate) fn from_clock(execution_id: ExecutionId, clock: VectorClock) -> Self {
        Self {
            execution_id: Some(execution_id),
            clock,
            #[cfg(feature = "causal-state")]
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

        #[cfg(feature = "causal-state")]
        match (&mut self.custom, &other.custom) {
            (_, None) => {}
            (slot @ None, Some(incoming)) => *slot = Some(incoming.clone()),
            (Some(current), Some(incoming)) => current.update(incoming),
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

#[derive(Clone, Default)]
pub(crate) struct CausalStateConfig {
    #[cfg(feature = "causal-state")]
    ops: Option<Arc<dyn ErasedCausalState>>,
}

impl CausalStateConfig {
    #[cfg(feature = "causal-state")]
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

    pub(crate) fn initialize(&self) -> Option<CausalStateValue> {
        #[cfg(feature = "causal-state")]
        {
            self.ops.as_ref().map(|ops| CausalStateValue(ops.initialize()))
        }

        #[cfg(not(feature = "causal-state"))]
        {
            None
        }
    }

    #[inline]
    pub(crate) fn is_enabled(&self) -> bool {
        #[cfg(feature = "causal-state")]
        {
            self.ops.is_some()
        }

        #[cfg(not(feature = "causal-state"))]
        {
            false
        }
    }

    pub(crate) fn capture(
        &self,
        execution_id: ExecutionId,
        clock: VectorClock,
        state: Option<&CausalStateValue>,
    ) -> CausalSnapshot {
        #[cfg(feature = "causal-state")]
        let custom = match (&self.ops, state) {
            (Some(ops), Some(state)) => Some(ErasedSnapshot {
                ops: Arc::clone(ops),
                value: state.0.clone(),
            }),
            _ => None,
        };

        #[cfg(not(feature = "causal-state"))]
        let _ = state;

        CausalSnapshot {
            execution_id: Some(execution_id),
            clock,
            #[cfg(feature = "causal-state")]
            custom,
        }
    }

    pub(crate) fn merge_into(&self, state: &mut Option<CausalStateValue>, snapshot: &CausalSnapshot) {
        #[cfg(feature = "causal-state")]
        if let (Some(ops), Some(incoming)) = (&self.ops, &snapshot.custom) {
            assert!(
                Arc::ptr_eq(ops, &incoming.ops),
                "causal state configuration changed within an execution"
            );
            let current = state.as_mut().expect("configured causal state is missing from a task");
            ops.join_values(current.0.as_mut(), incoming.value.as_ref());
        }

        #[cfg(not(feature = "causal-state"))]
        let _ = (state, snapshot);
    }

    #[cfg(feature = "causal-state")]
    pub(crate) fn state<T>(&self, state: Option<&CausalStateValue>) -> T
    where
        T: Clone + Send + Sync + 'static,
    {
        let ops = self.ops.as_ref().expect("no causal state is configured");
        assert_eq!(
            ops.state_type_id(),
            TypeId::of::<T>(),
            "requested type is not the configured causal state"
        );
        state
            .expect("current task has no configured causal state")
            .0
            .as_ref()
            .as_any()
            .downcast_ref::<T>()
            .expect("current task has an unexpected causal state type")
            .clone()
    }

    #[cfg(feature = "causal-state")]
    pub(crate) fn join_state<T>(&self, state: &mut Option<CausalStateValue>, incoming: T)
    where
        T: Clone + Send + Sync + 'static,
    {
        let ops = self.ops.as_ref().expect("no causal state is configured");
        assert_eq!(
            ops.state_type_id(),
            TypeId::of::<T>(),
            "joined type is not the configured causal state"
        );
        let current = state.as_mut().expect("current task has no configured causal state");
        ops.join_values(current.0.as_mut(), &incoming);
    }
}

impl fmt::Debug for CausalStateConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = f.debug_struct("CausalStateConfig");

        #[cfg(feature = "causal-state")]
        debug.field("type", &self.ops.as_ref().map(|ops| ops.state_type_name()));

        debug.finish()
    }
}

#[cfg(feature = "causal-state")]
#[derive(Clone)]
pub(crate) struct CausalStateValue(Box<dyn AnyCausalValue>);

#[cfg(not(feature = "causal-state"))]
#[derive(Clone, Debug)]
pub(crate) struct CausalStateValue;

#[cfg(feature = "causal-state")]
impl fmt::Debug for CausalStateValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CausalStateValue").finish_non_exhaustive()
    }
}

#[cfg(feature = "causal-state")]
trait AnyCausalValue: Send + Sync {
    fn clone_box(&self) -> Box<dyn AnyCausalValue>;

    #[cfg(feature = "causal-state")]
    fn as_any(&self) -> &dyn Any;

    #[cfg(feature = "causal-state")]
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

#[cfg(feature = "causal-state")]
impl<T> AnyCausalValue for T
where
    T: Clone + Send + Sync + 'static,
{
    fn clone_box(&self) -> Box<dyn AnyCausalValue> {
        Box::new(self.clone())
    }

    #[cfg(feature = "causal-state")]
    fn as_any(&self) -> &dyn Any {
        self
    }

    #[cfg(feature = "causal-state")]
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(feature = "causal-state")]
impl Clone for Box<dyn AnyCausalValue> {
    fn clone(&self) -> Self {
        (**self).clone_box()
    }
}

#[cfg(feature = "causal-state")]
trait ErasedCausalState: Send + Sync {
    fn state_type_id(&self) -> TypeId;
    fn state_type_name(&self) -> &'static str;
    fn initialize(&self) -> Box<dyn AnyCausalValue>;
    fn join_values(&self, current: &mut dyn AnyCausalValue, incoming: &dyn AnyCausalValue);
}

#[cfg(feature = "causal-state")]
struct TypedCausalState<T, I, J> {
    initial: I,
    join: J,
    state: PhantomData<fn() -> T>,
}

#[cfg(feature = "causal-state")]
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

#[cfg(feature = "causal-state")]
#[derive(Clone)]
struct ErasedSnapshot {
    ops: Arc<dyn ErasedCausalState>,
    value: Box<dyn AnyCausalValue>,
}

#[cfg(feature = "causal-state")]
impl ErasedSnapshot {
    fn update(&mut self, incoming: &Self) {
        assert!(
            Arc::ptr_eq(&self.ops, &incoming.ops),
            "cannot join causal snapshots from different configurations"
        );
        self.ops.join_values(self.value.as_mut(), incoming.value.as_ref());
    }
}
