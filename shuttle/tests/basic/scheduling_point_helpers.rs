//! Tests for the helpers that put scheduling points in `Drop` implementations and in `poll`
//! functions: `drop_scheduling_point` and `FirstPollSwitch`.

use shuttle::future::batch_semaphore::{BatchSemaphore, Fairness};
use shuttle::future::{self, block_on};
use shuttle::sync::atomic::{AtomicUsize as ShuttleAtomicUsize, Ordering as ShuttleOrdering};
use shuttle::sync::mpsc;
use shuttle::{check_dfs, check_random, current, thread};
use shuttle_engine::runtime::thread::{drop_scheduling_point, FirstPollSwitch};
use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

/// Takes a scheduling point when dropped, and then counts the drop.
struct SwitchOnDrop(Arc<AtomicUsize>);

impl Drop for SwitchOnDrop {
    fn drop(&mut self) {
        drop_scheduling_point();
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn drop_scheduling_point_switches() {
    check_dfs(
        || {
            let before = current::context_switches();
            drop(SwitchOnDrop(Arc::new(AtomicUsize::new(0))));
            assert_eq!(current::context_switches(), before + 1);
        },
        None,
    );
}

/// A destructor that runs while its task unwinds a caught panic switches, and runs to completion.
#[test]
fn drop_scheduling_point_while_unwinding_caught_panic() {
    let drops = Arc::new(AtomicUsize::new(0));
    let executions = Arc::new(AtomicUsize::new(0));
    let (drops_clone, executions_clone) = (Arc::clone(&drops), Arc::clone(&executions));
    check_dfs(
        move || {
            executions_clone.fetch_add(1, Ordering::SeqCst);
            let drops = Arc::clone(&drops_clone);
            let handle = thread::spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    let _guard = SwitchOnDrop(drops);
                    panic!("expected");
                }));
                assert!(result.is_err());
            });
            thread::yield_now();
            handle.join().unwrap();
        },
        None,
    );
    let executions = executions.load(Ordering::SeqCst);
    assert_eq!(drops.load(Ordering::SeqCst), executions);
    // DFS runs the other task at the destructor's scheduling point in some of the executions.
    assert_eq!(executions, 3);
}

/// A destructor that switches while an uncaught panic unwinds doesn't hide the panic.
#[test]
#[should_panic(expected = "expected")]
fn drop_scheduling_point_while_unwinding_uncaught_panic() {
    check_dfs(
        || {
            let _handle = thread::spawn(thread::yield_now);
            let _guard = SwitchOnDrop(Arc::new(AtomicUsize::new(0)));
            panic!("expected");
        },
        None,
    );
}

/// Destructors that switch run to completion when execution teardown drops a parked task.
#[test]
fn drop_scheduling_point_during_teardown_of_parked_task() {
    let drops = Arc::new(AtomicUsize::new(0));
    let drops_clone = Arc::clone(&drops);
    check_random(
        move || {
            let guard = SwitchOnDrop(Arc::clone(&drops_clone));
            super::teardown::park_until_teardown(move || drop(guard));
        },
        10,
    );
    assert_eq!(drops.load(Ordering::SeqCst), 10);
}

/// The same, when teardown has to unwind the task's stack.
#[test]
fn drop_scheduling_point_during_teardown_of_unwinding_task() {
    let drops = Arc::new(AtomicUsize::new(0));
    let drops_clone = Arc::clone(&drops);
    check_random(
        move || {
            let drops = Arc::clone(&drops_clone);
            let (started_tx, started_rx) = mpsc::channel();
            drop(future::spawn_local(async move {
                let _guard = SwitchOnDrop(drops);
                started_tx.send(()).unwrap();
                let spins = ShuttleAtomicUsize::new(0);
                loop {
                    spins.fetch_add(1, ShuttleOrdering::SeqCst);
                }
            }));
            started_rx.recv().unwrap();
        },
        10,
    );
    assert_eq!(drops.load(Ordering::SeqCst), 10);
}

/// Outside of an execution there's nothing to switch, and the destructor still runs.
#[test]
fn drop_scheduling_point_outside_execution() {
    let drops = Arc::new(AtomicUsize::new(0));
    drop(SwitchOnDrop(Arc::clone(&drops)));
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}

/// A future that is ready on its `polls`th poll, and records which of its polls switched.
struct PollsUntilReady {
    first_poll: FirstPollSwitch,
    polls: usize,
    switched: Vec<bool>,
}

impl Future for PollsUntilReady {
    type Output = Vec<bool>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Vec<bool>> {
        let before = current::context_switches();
        let first = self.first_poll.switch();
        let switched = current::context_switches() > before;
        assert_eq!(first, switched);
        self.switched.push(switched);
        if self.switched.len() == self.polls {
            Poll::Ready(std::mem::take(&mut self.switched))
        } else {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

#[test]
fn first_poll_switch_switches_once() {
    check_dfs(
        || {
            let switched = block_on(PollsUntilReady {
                first_poll: FirstPollSwitch::new(),
                polls: 3,
                switched: Vec::new(),
            });
            assert_eq!(switched, [true, false, false]);
        },
        None,
    );
}

#[test]
fn release_no_scheduling_point_takes_no_scheduling_point() {
    check_dfs(
        || {
            let semaphore = BatchSemaphore::new(0, Fairness::StrictlyFair);
            let before = current::context_switches();
            semaphore.release_no_scheduling_point(1);
            assert_eq!(current::context_switches(), before);
            semaphore.try_acquire(1).unwrap();
        },
        None,
    );
}
