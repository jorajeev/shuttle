//! A counting semaphore supporting both async and sync operations.
use crate::runtime::execution::ExecutionState;
use crate::runtime::task::TaskId;
use crate::runtime::thread;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};
use tracing::trace;

#[derive(Clone, Debug)]
struct Waiter {
    task_id: TaskId,
    num_permits: usize,
}

#[derive(Debug)]
/// A counting semaphore which permits waiting on multiple permits at once,
/// and supports both asychronous and synchronous blocking operations.
///
/// The semaphore is strictly fair, so earlier requesters always get priority
/// over later ones.
///
/// TODO: Provide an option to support weaker models for fairness.
struct BatchSemaphoreState {
    // Key invariant: if `waiters` is nonempty and the head waiter is `H`,
    // then `H.num_permits > permits_available`.  (In other words, we are
    // never in a state where there are enough permits available for the
    // first waiter.  This invariant is ensured by the `drop` handler below.)
    waiters: VecDeque<Waiter>,
    permits_available: usize,
    closed: bool,
}

/// Counting semaphore
#[derive(Debug)]
pub struct BatchSemaphore {
    state: Rc<RefCell<BatchSemaphoreState>>,
}

/// Error returned from the [`Semaphore::try_acquire`] function.
#[derive(Debug, PartialEq)]
pub enum TryAcquireError {
    /// The semaphore has been closed and cannot issue new permits.
    Closed,

    /// The semaphore has no available permits.
    NoPermits,
}

impl fmt::Display for TryAcquireError {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TryAcquireError::Closed => write!(fmt, "semaphore closed"),
            TryAcquireError::NoPermits => write!(fmt, "no permits available"),
        }
    }
}

impl std::error::Error for TryAcquireError {}

/// Error returned from the [`Semaphore::acquire`] function.
///
/// An `acquire*` operation can only fail if the semaphore has been
/// closed.
#[derive(Debug)]
pub struct AcquireError(());

impl AcquireError {
    fn closed() -> AcquireError {
        AcquireError(())
    }
}

impl fmt::Display for AcquireError {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(fmt, "semaphore closed")
    }
}

impl std::error::Error for AcquireError {}

impl BatchSemaphore {
    /// Creates a new semaphore with the initial number of permits.
    pub fn new(num_permits: usize) -> Self {
        let state = Rc::new(RefCell::new(BatchSemaphoreState {
            waiters: VecDeque::new(),
            permits_available: num_permits,
            closed: false,
        }));
        Self { state: state }
    }

    /// Returns the current number of available permits.
    pub fn available_permits(&self) -> usize {
        let state = self.state.borrow();
        state.permits_available
    }

    /// Closes the semaphore. This prevents the semaphore from issuing new
    /// permits and notifies all pending waiters.    
    pub fn close(&self) {
        let mut state = self.state.borrow_mut();
        state.closed = true;
        // Wake up all the waiters.  Since we've marked the state as closed, they
        // will all return `AcquireError::closed` from their acquire calls.
        ExecutionState::with(|s| {
            for waiter in state.waiters.drain(..) {
                let waker = s.get(waiter.task_id).waker();
                waker.wake();
            }
        });
    }

    /// Returns true iff the semaphore is closed.
    pub fn is_closed(&self) -> bool {
        let state = self.state.borrow();
        state.closed
    }

    /// Try to acquire the specified number of permits from the Semaphore.
    /// If the permits are available, returns Ok(())
    /// If the semaphore is closed, returns `Err(TryAcquireError::Closed)`
    /// If there aren't enough permits, returns `Err(TryAcquireError::NoPermits)`
    pub fn try_acquire(&self, num_permits: usize) -> Result<(), TryAcquireError> {
        let me = ExecutionState::me();
        let waiter = Waiter {
            task_id: me,
            num_permits,
        };
        self.attempt_acquire(&waiter, false)
    }

    // Attempt to acquire the permits for the given waiter, adding it to the wait list if
    // `add_waiter` is true.
    fn attempt_acquire(&self, waiter: &Waiter, add_waiter: bool) -> Result<(), TryAcquireError> {
        let mut state = self.state.borrow_mut();

        trace!(waiters = ?state.waiters, avail = ?state.permits_available, ask = ?waiter.num_permits, "task {:?} requesting semaphore {:p}", waiter.task_id, self);
        if state.closed {
            Err(TryAcquireError::Closed)
        } else if state.waiters.is_empty() && waiter.num_permits <= state.permits_available {
            state.permits_available -= waiter.num_permits;
            drop(state);
            thread::switch();
            Ok(())
        } else {
            if add_waiter {
                state.waiters.push_back(waiter.clone());
                drop(state);
                thread::switch();
            }
            Err(TryAcquireError::NoPermits)
        }
    }

    /// Acquire the specified number of permits (async API)
    pub fn acquire(&self, num_permits: usize) -> Acquire<'_> {
        Acquire::new(self, num_permits)
    }

    /// Acquire the specified number of permits (blocking API)
    pub fn acquire_blocking(&self, num_permits: usize) -> Result<(), AcquireError> {
        crate::future::block_on(self.acquire(num_permits))
    }

    /// Release `num_permits` back to the Semaphore
    /// The maximum number of permits is `usize::MAX >> 3`, and this function will panic if the limit is exceeded.
    pub fn release(&self, num_permits: usize) {
        if num_permits == 0 {
            return;
        }

        let mut state = self.state.borrow_mut();

        assert!(
            num_permits <= (usize::MAX >> 3) - state.permits_available,
            "release of {} permits for semaphore {:p} with {} available permits would exceed bound of {}",
            num_permits,
            self,
            state.permits_available,
            usize::MAX >> 3
        );

        state.permits_available += num_permits;

        // Bail out early if we're panicking so we don't try to touch `ExecutionState`
        if ExecutionState::should_stop() {
            return;
        }

        let me = ExecutionState::me();
        trace!(task = ?me, avail = ?state.permits_available, waiters = ?state.waiters, "released {} permits for semaphore {:p}", num_permits, self);
        while let Some(front) = state.waiters.front() {
            if front.num_permits <= state.permits_available {
                drop(front);
                state.permits_available -= front.num_permits;
                let waiter = state.waiters.pop_front().unwrap();
                trace!("waking up waiter {:?} for semaphore {:p}", waiter, self);
                let waker = ExecutionState::with(|s| s.get(waiter.task_id).waker());
                waker.wake();
            } else {
                break;
            }
        }
    }
}

// Safety: Semaphore is never actually passed across true threads, only across continuations. The
// Rc<RefCell<_>> type therefore can't be preempted mid-bookkeeping-operation.
// TODO we shouldn't need to do this, but RefCell is not Send, and anything we put within a Semaphore
// TODO needs to be Send.
unsafe impl Send for BatchSemaphore {}
unsafe impl Sync for BatchSemaphore {}

impl Default for BatchSemaphore {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

/// The future that results from async calls to `acquire*`.
/// Callers must `await` on this future to obtain the necessary permits.
#[derive(Debug)]
pub struct Acquire<'a> {
    waiter: Waiter,
    semaphore: &'a BatchSemaphore,
    should_wait: bool,
}

impl<'a> Acquire<'a> {
    fn new(semaphore: &'a BatchSemaphore, num_permits: usize) -> Self {
        let me = ExecutionState::me();
        Self {
            waiter: Waiter {
                task_id: me,
                num_permits,
            },
            should_wait: true,
            semaphore,
        }
    }
}

impl Future for Acquire<'_> {
    type Output = Result<(), AcquireError>;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Add this task to the waiter list only once
        let should_wait = std::mem::replace(&mut self.should_wait, false);
        let result = if should_wait {
            self.semaphore.attempt_acquire(&self.waiter, true)
        } else if self.semaphore.is_closed() {
            Err(TryAcquireError::Closed)
        } else {
            Ok(())
        };
        if let Err(e) = result {
            match e {
                TryAcquireError::Closed => Poll::Ready(Err(AcquireError::closed())),
                TryAcquireError::NoPermits => Poll::Pending,
            }
        } else {
            Poll::Ready(Ok(()))
        }
    }
}
