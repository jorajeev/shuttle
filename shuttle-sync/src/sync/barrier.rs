use shuttle_core::internal::{ExecutionState, TaskId, VectorClock, switch};
use crate::sync::{ResourceSignature, ResourceType};
use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt;
use std::rc::Rc;
use tracing::trace;

#[derive(Clone, Copy, Debug)]
/// A `BarrierWaitResult` is returned by `Barrier::wait()` when all threads in the `Barrier` have rendezvoused.
pub struct BarrierWaitResult {
    is_leader: bool,
}

impl BarrierWaitResult {
    /// Returns true if this thread is the "leader thread" for the call to `Barrier::wait()`.
    pub fn is_leader(&self) -> bool {
        self.is_leader
    }
}

struct BarrierState {
    bound: usize,
    epoch: u64,
    waiters: HashSet<TaskId>,
    leader_tokens: HashSet<u64>,
    clock: VectorClock,
}

impl fmt::Debug for BarrierState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BarrierState")
            .field("bound", &self.bound)
            .field("epoch", &self.epoch)
            .field("waiters", &self.waiters)
            .field("leader_tokens", &self.leader_tokens)
            .finish()
    }
}

#[derive(Debug)]
/// A barrier enables multiple threads to synchronize the beginning of some computation.
pub struct Barrier {
    state: Rc<RefCell<BarrierState>>,
    #[allow(unused)]
    signature: ResourceSignature,
}

impl Barrier {
    /// Creates a new barrier that can block a given number of threads.
    #[track_caller]
    pub fn new(n: usize) -> Self {
        let state = BarrierState {
            bound: n,
            epoch: 0,
            waiters: HashSet::new(),
            leader_tokens: HashSet::new(),
            clock: VectorClock::new(),
        };

        Self {
            state: Rc::new(RefCell::new(state)),
            signature: ExecutionState::new_resource_signature(ResourceType::Barrier),
        }
    }

    /// Blocks the current thread until all threads have rendezvoused here.
    pub fn wait(&self) -> BarrierWaitResult {
        let state = self.state.borrow_mut();
        let will_block = state.waiters.len() + 1 < state.bound;
        drop(state);

        if !will_block {
            switch();
        }
        let mut state = self.state.borrow_mut();
        let my_epoch = state.epoch;

        trace!(waiters=?state.waiters, epoch=my_epoch, "waiting on barrier {:p}", self);

        ExecutionState::with(|s| {
            let clock = s.increment_clock();
            state.clock.update(clock);
        });

        assert!(state.waiters.insert(ExecutionState::me()));

        if state.waiters.len() < state.bound {
            trace!(waiters=?state.waiters, epoch=my_epoch, "blocked on barrier {:?}", self);
            drop(state);
            ExecutionState::with(|s| s.current_mut().block(false));
            switch();
        } else {
            trace!(waiters=?state.waiters, epoch=my_epoch, "releasing waiters on barrier {:?}", self);

            debug_assert!(state.waiters.len() == state.bound || state.bound == 0);

            assert!(state.leader_tokens.insert(my_epoch));

            let waiters = state.waiters.drain().collect::<Vec<_>>();
            state.epoch += 1;

            trace!(
                waiters=?state.waiters,
                epoch=state.epoch,
                "releasing waiters on barrier {:?}",
                self,
            );

            let clock = state.clock.clone();
            ExecutionState::with(|s| {
                for tid in waiters {
                    let t = s.get_mut(tid);
                    t.clock.increment(tid);
                    t.clock.update(&clock);
                    t.unblock();
                }
            });
            drop(state);
        };

        let is_leader = self.state.borrow_mut().leader_tokens.remove(&my_epoch);

        trace!(epoch=?my_epoch, is_leader, "returning from barrier {:?}", self);

        BarrierWaitResult { is_leader }
    }
}

// Safety: Barrier is never actually passed across threads, only across continuations.
unsafe impl Send for Barrier {}
unsafe impl Sync for Barrier {}
