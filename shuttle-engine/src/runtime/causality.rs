use crate::runtime::task::clock::VectorClock;
use crate::runtime::task::labels::Labels;
use std::fmt;

#[cfg(feature = "causal-state")]
use std::any::{Any, TypeId};
#[cfg(feature = "causal-state")]
use std::sync::Arc;

/// A snapshot of the causal information associated with a task.
///
/// This type is public for use by Shuttle's synchronization implementations.
/// Applications should use the typed APIs in [`crate::current`] instead.
#[doc(hidden)]
#[derive(Clone)]
pub struct CausalSnapshot {
    clock: VectorClock,
    #[cfg(feature = "causal-state")]
    custom: Option<ErasedSnapshot>,
}

impl CausalSnapshot {
    /// Construct an empty snapshot.
    #[inline]
    pub const fn new() -> Self {
        Self {
            clock: VectorClock::new(),
            #[cfg(feature = "causal-state")]
            custom: None,
        }
    }

    #[inline]
    pub(crate) fn from_clock(clock: VectorClock) -> Self {
        Self {
            clock,
            #[cfg(feature = "causal-state")]
            custom: None,
        }
    }

    /// Join another causal snapshot into this one.
    #[inline]
    pub fn update(&mut self, other: &Self) {
        self.clock.update(&other.clock);

        #[cfg(feature = "causal-state")]
        match (&mut self.custom, &other.custom) {
            (_, None) => {}
            (slot @ None, Some(incoming)) => *slot = Some(incoming.clone()),
            (Some(current), Some(incoming)) => current.update(incoming),
        }
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
    pub(crate) fn set<T, J>(&mut self, initial: T, join: J)
    where
        T: Clone + Send + Sync + 'static,
        J: Fn(&mut T, &T) + Send + Sync + 'static,
    {
        assert!(self.ops.is_none(), "only one causal state type may be configured");
        self.ops = Some(Arc::new(TypedCausalState { initial, join }));
    }

    pub(crate) fn initialize(&self, labels: &mut Labels) {
        #[cfg(feature = "causal-state")]
        if let Some(ops) = &self.ops {
            ops.initialize(labels);
        }

        #[cfg(not(feature = "causal-state"))]
        let _ = labels;
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

    pub(crate) fn capture(&self, clock: VectorClock, labels: Option<&Labels>) -> CausalSnapshot {
        #[cfg(feature = "causal-state")]
        let custom = match (&self.ops, labels) {
            (Some(ops), Some(labels)) => ops.snapshot(labels).map(|value| ErasedSnapshot {
                ops: Arc::clone(ops),
                value,
            }),
            _ => None,
        };

        #[cfg(not(feature = "causal-state"))]
        let _ = labels;

        CausalSnapshot {
            clock,
            #[cfg(feature = "causal-state")]
            custom,
        }
    }

    pub(crate) fn merge_into_labels(&self, labels: &mut Labels, snapshot: &CausalSnapshot) {
        #[cfg(feature = "causal-state")]
        if let (Some(ops), Some(incoming)) = (&self.ops, &snapshot.custom) {
            assert_eq!(
                ops.state_type_id(),
                incoming.ops.state_type_id(),
                "causal state type changed while a synchronization object was live"
            );
            ops.join_into_labels(labels, incoming.value.as_ref());
        }

        #[cfg(not(feature = "causal-state"))]
        let _ = (labels, snapshot);
    }

    #[cfg(feature = "causal-state")]
    pub(crate) fn is_type<T: 'static>(&self) -> bool {
        self.ops
            .as_ref()
            .is_some_and(|ops| ops.state_type_id() == TypeId::of::<T>())
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
trait AnyCausalValue: Any + Send + Sync {
    fn clone_box(&self) -> Box<dyn AnyCausalValue>;
    fn as_any(&self) -> &dyn Any;
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

    fn as_any(&self) -> &dyn Any {
        self
    }

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
    fn initialize(&self, labels: &mut Labels);
    fn snapshot(&self, labels: &Labels) -> Option<Box<dyn AnyCausalValue>>;
    fn join_values(&self, current: &mut dyn AnyCausalValue, incoming: &dyn AnyCausalValue);
    fn join_into_labels(&self, labels: &mut Labels, incoming: &dyn AnyCausalValue);
}

#[cfg(feature = "causal-state")]
struct TypedCausalState<T, J> {
    initial: T,
    join: J,
}

#[cfg(feature = "causal-state")]
impl<T, J> ErasedCausalState for TypedCausalState<T, J>
where
    T: Clone + Send + Sync + 'static,
    J: Fn(&mut T, &T) + Send + Sync + 'static,
{
    fn state_type_id(&self) -> TypeId {
        TypeId::of::<T>()
    }

    fn state_type_name(&self) -> &'static str {
        std::any::type_name::<T>()
    }

    fn initialize(&self, labels: &mut Labels) {
        labels.insert(self.initial.clone());
    }

    fn snapshot(&self, labels: &Labels) -> Option<Box<dyn AnyCausalValue>> {
        labels
            .get::<T>()
            .cloned()
            .map(|value| Box::new(value) as Box<dyn AnyCausalValue>)
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

    fn join_into_labels(&self, labels: &mut Labels, incoming: &dyn AnyCausalValue) {
        let incoming = incoming
            .as_any()
            .downcast_ref::<T>()
            .expect("causal snapshot had an unexpected label type");

        if let Some(current) = labels.get_mut::<T>() {
            (self.join)(current, incoming);
        } else {
            labels.insert(incoming.clone());
        }
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
        assert_eq!(
            self.ops.state_type_id(),
            incoming.ops.state_type_id(),
            "cannot join causal snapshots with different types"
        );
        self.ops.join_values(self.value.as_mut(), incoming.value.as_ref());
    }
}
