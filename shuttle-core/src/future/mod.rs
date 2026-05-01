//! Core async support for Shuttle.
//!
//! This module provides `block_on` and `yield_now`, which are needed by
//! `batch_semaphore` and other core primitives. Higher-level async APIs
//! (spawn, JoinHandle, etc.) live in `shuttle-sync`.

pub mod batch_semaphore;

use crate::runtime::execution::ExecutionState;
use crate::runtime::thread;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Run a future to completion on the current thread.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = Box::pin(future);
    let waker = ExecutionState::with(|state| state.current_mut().waker());
    let cx = &mut Context::from_waker(&waker);

    // Note: we only switch on poll pending, since this blocks the current task. This means that *internal*
    // Shuttle futures which do not use other Shuttle primitives such as `batch_semaphore::Acquire` must
    // have a scheduling point prior to first poll if that poll will be successful and can affect other tasks.
    // For example, an uncontested acquire makes other threads block or fail try-acquires, so there must be
    // a scheduling point for scheduling completeness. For *external* futures, this is a non-issue because they
    // should use other Shuttle primitives inside of `poll` if polling can affect other threads.
    loop {
        match future.as_mut().poll(cx) {
            Poll::Ready(result) => break result,
            Poll::Pending => {
                ExecutionState::with(|state| state.current_mut().sleep_unless_woken());
                thread::switch();
            }
        }
    }
}

/// Yields execution back to the scheduler.
///
/// Borrowed from the Tokio implementation.
pub async fn yield_now() {
    /// Yield implementation
    struct YieldNow {
        yielded: bool,
    }

    impl Future for YieldNow {
        type Output = ();

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            if self.yielded {
                return Poll::Ready(());
            }

            self.yielded = true;
            cx.waker().wake_by_ref();
            ExecutionState::request_yield();
            Poll::Pending
        }
    }

    YieldNow { yielded: false }.await
}
