use shuttle_core::internal::{ExecutionState, StorageKey, VectorClock};
use crate::sync::{Mutex, ResourceSignature, ResourceType};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize as StdAtomicUsize, Ordering};
use std::sync::LazyLock;
use tracing::trace;

/// A synchronization primitive which can be used to run a one-time global initialization.
#[derive(Debug)]
pub struct Once {
    id: LazyLock<usize>,
    signature: ResourceSignature,
}

enum OnceInitState {
    Running(Rc<Mutex<bool>>),
    Complete(VectorClock),
}

impl std::fmt::Debug for OnceInitState {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Running(_) => write!(f, "Running"),
            Self::Complete(_) => write!(f, "Complete"),
        }
    }
}

impl Once {
    /// Creates a new `Once` value.
    #[must_use]
    #[allow(clippy::new_without_default)]
    #[track_caller]
    pub const fn new() -> Self {
        static NEXT_ID: StdAtomicUsize = StdAtomicUsize::new(1);

        Self {
            id: LazyLock::new(|| {
                let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
                assert_ne!(id, 0, "id overflow");
                id
            }),
            signature: ResourceSignature::new_const(ResourceType::Once),
        }
    }

    /// Performs an initialization routine once and only once.
    pub fn call_once<F>(&self, f: F)
    where
        F: FnOnce(),
    {
        self.call_once_inner(|_state| f(), false);
    }

    /// Performs the same function as [`Once::call_once()`] except ignores poisoning.
    pub fn call_once_force<F>(&self, f: F)
    where
        F: FnOnce(&OnceState),
    {
        self.call_once_inner(f, true);
    }

    /// Returns `true` if some [`Once::call_once()`] call has completed successfully.
    pub fn is_completed(&self) -> bool {
        ExecutionState::with(|state| {
            let init = match self.get_state(state) {
                Some(init) => init,
                None => return false,
            };
            let init_state = init.borrow();
            match &*init_state {
                OnceInitState::Complete(clock) => {
                    let clock = clock.clone();
                    drop(init_state);
                    state.update_clock(&clock);
                    true
                }
                _ => false,
            }
        })
    }

    fn call_once_inner<F>(&self, f: F, ignore_poisoning: bool)
    where
        F: FnOnce(&OnceState),
    {
        let lock = ExecutionState::with(|state| {
            if self.get_state(state).is_none() {
                self.init_state(
                    state,
                    OnceInitState::Running(Rc::new(Mutex::new_internal(false, self.signature.clone()))),
                );
            }

            let init = self.get_state(state).expect("must be initialized by this point");
            let init_state = init.borrow();
            trace!(state=?init_state, "call_once on cell {:p}", self);
            match &*init_state {
                OnceInitState::Complete(clock) => {
                    let clock = clock.clone();
                    drop(init_state);
                    state.update_clock(&clock);
                    None
                }
                OnceInitState::Running(lock) => Some(Rc::clone(lock)),
            }
        });

        if let Some(lock) = lock {
            let (mut flag, is_poisoned) = match lock.lock() {
                Ok(flag) => (flag, false),
                Err(_) if !ignore_poisoning => panic!("Once instance has previously been poisoned"),
                Err(err) => (err.into_inner(), true),
            };
            if *flag {
                return;
            }

            trace!("won the call_once race for cell {:p}", self);
            f(&OnceState(is_poisoned));

            *flag = true;
            ExecutionState::with(|state| {
                let clock = state.increment_clock().clone();
                *self
                    .get_state(state)
                    .expect("must be initialized by this point")
                    .borrow_mut() = OnceInitState::Complete(clock);
            });
        }
    }

    fn id(&self) -> usize {
        *self.id
    }

    fn get_state<'a>(&self, from: &'a ExecutionState) -> Option<&'a RefCell<OnceInitState>> {
        from.get_storage::<_, RefCell<OnceInitState>>(self)
    }

    fn init_state(&self, into: &mut ExecutionState, new_state: OnceInitState) {
        into.init_storage::<_, RefCell<OnceInitState>>(self, RefCell::new(new_state));
    }
}

/// State yielded to [`Once::call_once_force()`]'s closure parameter.
#[derive(Debug)]
#[non_exhaustive]
pub struct OnceState(bool);

impl OnceState {
    /// Returns `true` if the associated [`Once`] was poisoned prior to the invocation of the
    /// closure passed to [`Once::call_once_force()`].
    pub fn is_poisoned(&self) -> bool {
        self.0
    }
}

impl From<&Once> for StorageKey {
    fn from(once: &Once) -> Self {
        StorageKey(once.id(), 0x2)
    }
}
