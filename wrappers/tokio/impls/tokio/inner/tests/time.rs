use shuttle_tokio_impl_inner::time::{clear_triggers, sleep, sleep_until, Duration, Instant};
use test_log::test;

fn sleep_test(duration: Duration) {
    shuttle::check_dfs(
        move || {
            shuttle::future::block_on(async move {
                let _jh = shuttle::future::spawn(async move {
                    sleep(duration).await;
                    panic!();
                });
            });
        },
        None,
    );
}

fn sleep_until_test(deadline: Instant) {
    shuttle::check_dfs(
        move || {
            shuttle::future::block_on(async move {
                let _jh = shuttle::future::spawn(async move {
                    sleep_until(deadline).await;
                    panic!();
                });
            });
        },
        None,
    );
}

#[test]
#[should_panic(expected = "explicit panic")]
fn sleep_panic() {
    sleep_test(Duration::from_secs(10_0000));
}

#[test]
fn sleep_forever() {
    sleep_test(Duration::MAX);
}

#[test]
#[should_panic(expected = "explicit panic")]
fn sleep_until_panic() {
    sleep_until_test(Instant::now() + Duration::from_micros(500));
}

// Test that `deadline`, `is_elapsed` and `reset` work as expected.
#[test]
fn sleep_reset() {
    let old_deadline = Instant::now() - Duration::from_secs(5);
    let mut sleep = sleep_until(old_deadline);
    assert_eq!(sleep.deadline(), old_deadline);
    assert!(sleep.is_elapsed());

    let new_deadline = Instant::now() + Duration::from_secs(100);
    let pinned = std::pin::Pin::new(&mut sleep);
    pinned.reset(new_deadline);
    assert_eq!(sleep.deadline(), new_deadline);
    assert!(!sleep.is_elapsed());
}

mod timeout_tests {
    use super::*;
    use futures::future::join_all;
    use shuttle::current::{me, set_label_for_task, Labels};
    use shuttle::future;
    use shuttle_tokio_impl_inner::sync::Mutex;
    use shuttle_tokio_impl_inner::time::{timeout, trigger_timeouts};
    use std::sync::Arc;
    use test_log::test;

    #[derive(Clone, Debug)]
    struct Label(usize);

    // Create an instance of the Dining Philosophers problem with `count` philosophers.
    // The i'th philosopher grabs forks i and i+1 (modulo count).
    async fn dining_philosophers(count: usize, trigger: bool) {
        clear_triggers();
        let forks = (0..count).map(|_| Arc::new(Mutex::new(0))).collect::<Vec<_>>();

        let mut handles = Vec::new();
        for i in 0..count {
            let left_fork = forks[i].clone();
            let right_fork = forks[(i + 1) % count].clone();
            let h = future::spawn(async move {
                _ = set_label_for_task(me(), Label(i));
                let l = timeout(Duration::from_secs(1), left_fork.lock()).await;
                let r = timeout(Duration::from_secs(1), right_fork.lock()).await;
                l.is_err() || r.is_err()
            });
            handles.push(h);
        }

        if trigger {
            // Trigger timeout on the middle philosopher
            trigger_timeouts(move |labels: &Labels| labels.get::<Label>().expect("task label not set").0 == count / 2);
        }

        let _ = join_all(handles).await;
    }

    #[test]
    fn dining_philosophers_no_deadlock() {
        shuttle::check_random(
            || {
                shuttle::future::block_on(async move {
                    dining_philosophers(5, true).await;
                });
            },
            10_000,
        );
    }

    // Check that if we don't enable timeouts, the philosophers can deadlock.
    #[test]
    #[should_panic(expected = "deadlock")]
    fn dining_philosophers_with_deadlock() {
        shuttle::check_random(
            || {
                shuttle::future::block_on(async move {
                    dining_philosophers(5, false).await;
                });
            },
            10_000,
        );
    }

    // Do an exhaustive check for 2 philosophers.  Since this test takes a while, we mark it ignored.
    #[ignore]
    #[test]
    fn dining_philosophers_no_deadlock_exhaustive() {
        shuttle::check_dfs(
            || {
                shuttle::future::block_on(async move {
                    dining_philosophers(2, true).await;
                });
            },
            None,
        );
    }
}

mod scheduling_point_tests {
    use shuttle::current::{me, set_label_for_task, Labels};
    use shuttle::sync::atomic::AtomicBool;
    use shuttle::{check_dfs, future};
    use shuttle_tokio_impl_inner::time::{clear_triggers, timeout, trigger_timeouts, Duration};
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use test_log::test;

    #[derive(Clone, Debug)]
    struct Waiter;

    fn is_waiter(labels: &Labels) -> bool {
        labels.get::<Waiter>().is_some()
    }

    /// Regression test: Shuttle must be able to schedule another task between a task's previous
    /// operation and its `trigger_timeouts`.
    ///
    /// The test program wrongly assumes that once the waiter sees `ready`, the trigger is in place,
    /// so that the waiter's timeout expires. That fails when the waiter runs between the store and
    /// `trigger_timeouts`, which Shuttle only explores if `trigger_timeouts` takes a scheduling point
    /// first.
    #[test]
    #[should_panic(expected = "trigger not added yet")]
    fn trigger_timeouts_after_store_is_a_separate_step() {
        check_dfs(
            || {
                future::block_on(async {
                    clear_triggers();
                    let ready = Arc::new(AtomicBool::new(false));
                    let waiter = future::spawn({
                        let ready = ready.clone();
                        async move {
                            set_label_for_task(me(), Waiter);
                            if ready.load(Ordering::SeqCst) {
                                let result = timeout(Duration::from_secs(1), async {}).await;
                                // BUG (in this test program): assumes the trigger is in place.
                                assert!(result.is_err(), "trigger not added yet");
                            }
                        }
                    });
                    ready.store(true, Ordering::SeqCst);
                    trigger_timeouts(is_waiter);
                    waiter.await.unwrap();
                });
            },
            None,
        );
    }

    /// Regression test: Shuttle must be able to schedule another task between a task's previous
    /// operation and the first poll of its `Timeout`.
    ///
    /// The test program wrongly assumes that a trigger added after the waiter's store comes too late
    /// for the waiter's timeout. That fails when the main task triggers between the store and the
    /// poll, which Shuttle only explores if the poll takes a scheduling point first.
    #[test]
    #[should_panic(expected = "timeout expired after the store")]
    fn timeout_poll_after_store_is_a_separate_step() {
        check_dfs(
            || {
                future::block_on(async {
                    clear_triggers();
                    let ready = Arc::new(AtomicBool::new(false));
                    let waiter = future::spawn({
                        let ready = ready.clone();
                        async move {
                            set_label_for_task(me(), Waiter);
                            let timeout = timeout(Duration::from_secs(1), async {});
                            ready.store(true, Ordering::SeqCst);
                            timeout.await.is_ok()
                        }
                    });
                    let seen = ready.load(Ordering::SeqCst);
                    if seen {
                        trigger_timeouts(is_waiter);
                    }
                    let completed = waiter.await.unwrap();
                    // BUG (in this test program): assumes the poll ran in the step of the store.
                    assert!(!seen || completed, "timeout expired after the store");
                });
            },
            None,
        );
    }

    /// Regression test: Shuttle must be able to schedule another task between a task's previous
    /// operation and its `clear_triggers`.
    ///
    /// The test program wrongly assumes that once the waiter sees `ready`, the triggers are gone, so
    /// that the waiter's timeout completes. That fails when the waiter runs between the store and
    /// `clear_triggers`, which Shuttle only explores if `clear_triggers` takes a scheduling point
    /// first.
    #[test]
    #[should_panic(expected = "triggers not cleared yet")]
    fn clear_triggers_after_store_is_a_separate_step() {
        check_dfs(
            || {
                future::block_on(async {
                    clear_triggers();
                    trigger_timeouts(is_waiter);
                    let ready = Arc::new(AtomicBool::new(false));
                    let waiter = future::spawn({
                        let ready = ready.clone();
                        async move {
                            set_label_for_task(me(), Waiter);
                            if ready.load(Ordering::SeqCst) {
                                let result = timeout(Duration::from_secs(1), async {}).await;
                                // BUG (in this test program): assumes the triggers are gone.
                                assert!(result.is_ok(), "triggers not cleared yet");
                            }
                        }
                    });
                    ready.store(true, Ordering::SeqCst);
                    clear_triggers();
                    waiter.await.unwrap();
                });
            },
            None,
        );
    }
}
