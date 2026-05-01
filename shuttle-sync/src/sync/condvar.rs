use shuttle_core::current;
use shuttle_core::internal::{ExecutionState, TaskId, VectorClock, switch};
use crate::sync::{MutexGuard, ResourceSignature, ResourceType};
use assoc::AssocExt;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::{LockResult, PoisonError};
use std::time::Duration;
use tracing::trace;

/// A `Condvar` represents the ability to block a thread such that it consumes no CPU time while
/// waiting for an event to occur.
#[derive(Debug)]
pub struct Condvar {
    state: RefCell<CondvarState>,
    #[allow(unused)]
    signature: ResourceSignature,
}

#[derive(Debug)]
struct CondvarState {
    // TODO: this should be a HashMap but [HashMap::new] is not const
    waiters: Vec<(TaskId, CondvarWaitStatus)>,
    next_epoch: usize,
}

// For tracking causal dependencies, we record the clock C of the thread that does the notify.
// When a thread is unblocked, its clock is updated by C.
#[derive(PartialEq, Eq, Debug)]
enum CondvarWaitStatus {
    Waiting,
    // invariant: VecDeque is non-empty (if it's empty, we should be Waiting instead)
    Signal(VecDeque<(usize, VectorClock)>),
    Broadcast(VectorClock),
}

impl Condvar {
    /// Creates a new condition variable which is ready to be waited on and notified.
    #[track_caller]
    pub const fn new() -> Self {
        let state = CondvarState {
            waiters: Vec::new(),
            next_epoch: 0,
        };

        Self {
            state: RefCell::new(state),
            signature: ResourceSignature::new_const(ResourceType::Condvar),
        }
    }

    /// Blocks the current thread until this condition variable receives a notification.
    pub fn wait<'a, T>(&self, guard: MutexGuard<'a, T>) -> LockResult<MutexGuard<'a, T>> {
        let me = ExecutionState::me();

        let mutex = guard.unlock();
        let mut state = self.state.borrow_mut();

        trace!(waiters=?state.waiters, next_epoch=state.next_epoch, "waiting on condvar {:p}", self);

        debug_assert!(<_ as AssocExt<_, _>>::get(&state.waiters, &me).is_none());
        state.waiters.push((me, CondvarWaitStatus::Waiting));
        drop(state);

        ExecutionState::with(|s| s.current_mut().block(false));
        switch();

        let mut state = self.state.borrow_mut();
        trace!(waiters=?state.waiters, next_epoch=state.next_epoch, "woken from condvar {:p}", self);
        let my_status = <_ as AssocExt<_, _>>::remove(&mut state.waiters, &me).expect("should be waiting");
        match my_status {
            CondvarWaitStatus::Broadcast(clock) => {
                ExecutionState::with(|s| s.update_clock(&clock));
            }
            CondvarWaitStatus::Signal(mut epochs) => {
                let (epoch, clock) = epochs.pop_front().expect("should be a pending signal");
                for (tid, status) in state.waiters.iter_mut() {
                    if let CondvarWaitStatus::Signal(epochs) = status {
                        if let Some(i) = epochs.iter().position(|e| epoch == e.0) {
                            epochs.remove(i);
                            if epochs.is_empty() {
                                *status = CondvarWaitStatus::Waiting;
                                ExecutionState::with(|s| s.get_mut(*tid).block(false));
                            }
                        }
                    }
                }
                ExecutionState::with(|s| s.update_clock(&clock));
            }
            CondvarWaitStatus::Waiting => panic!("should not have been woken while in Waiting status"),
        }
        drop(state);

        mutex.lock()
    }

    /// Blocks the current thread until this condition variable receives a notification and the
    /// provided condition is false.
    pub fn wait_while<'a, T, F>(&self, mut guard: MutexGuard<'a, T>, mut condition: F) -> LockResult<MutexGuard<'a, T>>
    where
        F: FnMut(&mut T) -> bool,
    {
        while condition(&mut *guard) {
            guard = self.wait(guard)?;
        }
        Ok(guard)
    }

    /// Waits on this condition variable for a notification, timing out after a specified duration.
    pub fn wait_timeout<'a, T>(
        &self,
        guard: MutexGuard<'a, T>,
        _dur: Duration,
    ) -> LockResult<(MutexGuard<'a, T>, WaitTimeoutResult)> {
        self.wait(guard)
            .map(|guard| (guard, WaitTimeoutResult(false)))
            .map_err(|e| PoisonError::new((e.into_inner(), WaitTimeoutResult(false))))
    }

    /// Waits on this condition variable for a notification, timing out after a specified duration.
    pub fn wait_timeout_while<'a, T, F>(
        &self,
        guard: MutexGuard<'a, T>,
        _dur: Duration,
        condition: F,
    ) -> LockResult<(MutexGuard<'a, T>, WaitTimeoutResult)>
    where
        F: FnMut(&mut T) -> bool,
    {
        self.wait_while(guard, condition)
            .map(|guard| (guard, WaitTimeoutResult(false)))
            .map_err(|e| PoisonError::new((e.into_inner(), WaitTimeoutResult(false))))
    }

    /// Wakes up one blocked thread on this condvar.
    pub fn notify_one(&self) {
        switch();

        let me = ExecutionState::me();

        let mut state = self.state.borrow_mut();

        trace!(waiters=?state.waiters, next_epoch=state.next_epoch, "notifying one on condvar {:p}", self);

        let epoch = state.next_epoch;
        for (tid, status) in state.waiters.iter_mut() {
            assert_ne!(*tid, me);

            let clock = current::clock();
            match status {
                CondvarWaitStatus::Waiting => {
                    let mut epochs = VecDeque::new();
                    epochs.push_back((epoch, clock));
                    *status = CondvarWaitStatus::Signal(epochs);
                }
                CondvarWaitStatus::Signal(epochs) => {
                    epochs.push_back((epoch, clock));
                }
                CondvarWaitStatus::Broadcast(_) => {
                    // no-op, broadcast will already unblock this task
                }
            }

            ExecutionState::with(|s| s.get_mut(*tid).unblock());
        }
        state.next_epoch += 1;

        drop(state);
    }

    /// Wakes up all blocked threads on this condvar.
    pub fn notify_all(&self) {
        switch();

        let me = ExecutionState::me();

        let mut state = self.state.borrow_mut();

        trace!(waiters=?state.waiters, next_epoch=state.next_epoch, "notifying all on condvar {:p}", self);

        for (tid, status) in state.waiters.iter_mut() {
            assert_ne!(*tid, me);
            *status = CondvarWaitStatus::Broadcast(current::clock());
            ExecutionState::with(|s| s.get_mut(*tid).unblock());
        }

        drop(state);
    }
}

// Safety: Condvar is never actually passed across true threads, only across continuations. The
// Rc<RefCell<_>> type therefore can't be preempted mid-bookkeeping-operation.
// TODO we shouldn't need to do this, but RefCell is not Send
unsafe impl Send for Condvar {}
unsafe impl Sync for Condvar {}

impl Default for Condvar {
    fn default() -> Self {
        Self::new()
    }
}

/// A type indicating whether a timed wait on a condition variable returned due to a time out or not.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct WaitTimeoutResult(bool);

impl WaitTimeoutResult {
    /// Returns `true` if the wait was known to have timed out.
    pub fn timed_out(&self) -> bool {
        self.0
    }
}
