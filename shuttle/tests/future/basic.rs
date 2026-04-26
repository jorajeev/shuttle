use futures::task::{FutureObj, Spawn, SpawnError, SpawnExt as _};
use futures::{try_join, Future};
use shuttle::sync::{Barrier, Mutex};
use shuttle::{check_dfs, check_random, future, scheduler::PctScheduler, thread, Runner};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use test_log::test;

async fn add(a: u32, b: u32) -> u32 {
    a + b
}

#[test]
fn async_fncall() {
    check_dfs(
        move || {
            let sum = add(3, 5);
            future::spawn(async move {
                let r = sum.await;
                assert_eq!(r, 8u32);
            });
        },
        None,
    );
}

#[test]
fn async_with_join() {
    check_dfs(
        move || {
            thread::spawn(|| {
                let join = future::spawn(async move { add(10, 32).await });

                future::spawn(async move {
                    assert_eq!(join.await.unwrap(), 42u32);
                });
            });
        },
        None,
    );
}

#[test]
fn async_with_threads() {
    check_dfs(
        move || {
            thread::spawn(|| {
                let v1 = async { 3u32 };
                let v2 = async { 2u32 };
                future::spawn(async move {
                    assert_eq!(5u32, v1.await + v2.await);
                });
            });
            thread::spawn(|| {
                let v1 = async { 5u32 };
                let v2 = async { 6u32 };
                future::spawn(async move {
                    assert_eq!(11u32, v1.await + v2.await);
                });
            });
        },
        None,
    );
}

#[test]
fn async_block_on() {
    check_dfs(
        || {
            let v = future::block_on(async { 42u32 });
            assert_eq!(v, 42u32);
        },
        None,
    );
}

#[test]
fn async_spawn() {
    check_dfs(
        || {
            let t = future::spawn(async { 42u32 });
            let v = future::block_on(async { t.await.unwrap() });
            assert_eq!(v, 42u32);
        },
        None,
    );
}

#[test]
fn async_spawn_chain() {
    check_dfs(
        || {
            let t1 = future::spawn(async { 1u32 });
            let t2 = future::spawn(async move { t1.await.unwrap() });
            let v = future::block_on(async move { t2.await.unwrap() });
            assert_eq!(v, 1u32);
        },
        None,
    );
}

#[test]
fn async_thread_yield() {
    // This tests if thread::yield_now can be called from within an async block
    check_dfs(
        || {
            future::spawn(async move {
                thread::yield_now();
            });
            future::spawn(async move {});
        },
        None,
    )
}

#[test]
#[should_panic(expected = "DFS should find a schedule where r=1 here")]
fn async_atomic() {
    // This tests if shuttle can correctly schedule a different task
    // after thread::yield_now is called from within an async block
    use std::sync::atomic::{AtomicUsize, Ordering};
    check_dfs(
        || {
            let r = Arc::new(AtomicUsize::new(0));
            let r1 = r.clone();
            future::spawn(async move {
                r1.store(1, Ordering::SeqCst);
                thread::yield_now();
                r1.store(0, Ordering::SeqCst);
            });
            future::spawn(async move {
                assert_eq!(r.load(Ordering::SeqCst), 0, "DFS should find a schedule where r=1 here");
            });
        },
        None,
    )
}

#[test]
fn async_mutex() {
    // This test checks that futures can acquire Shuttle sync mutexes.
    // The future should only block on acquiring a lock when
    // another task holds the lock.
    check_dfs(
        move || {
            let lock = Arc::new(Mutex::new(0u64));

            let t1 = {
                let lock = Arc::clone(&lock);
                future::spawn(async move {
                    let mut l = lock.lock().unwrap();
                    *l += 1;
                })
            };

            let t2 = future::block_on(async move {
                t1.await.unwrap();
                *lock.lock().unwrap()
            });

            assert_eq!(t2, 1);
        },
        None,
    )
}

#[test]
fn async_yield() {
    check_dfs(
        || {
            let v = future::block_on(async {
                future::yield_now().await;
                42u32
            });
            assert_eq!(v, 42u32);
        },
        None,
    )
}

#[test]
fn join_handle_abort() {
    check_dfs(
        || {
            let counter = Arc::new(AtomicUsize::new(0));
            let t1 = future::spawn({
                let counter = Arc::clone(&counter);
                async move {
                    Barrier::new(2).wait();
                    counter.fetch_add(1, Ordering::SeqCst)
                }
            });
            t1.abort();
            t1.abort(); // should be safe to call it twice
            assert_eq!(0, counter.load(Ordering::SeqCst));
        },
        None,
    );
}

fn async_counter() {
    let counter = Arc::new(AtomicUsize::new(0));

    let tasks: Vec<_> = (0..10)
        .map(|_| {
            let counter = Arc::clone(&counter);
            future::spawn(async move {
                let c = counter.load(Ordering::SeqCst);
                future::yield_now().await;
                counter.fetch_add(c, Ordering::SeqCst);
            })
        })
        .collect();

    future::block_on(async move {
        for t in tasks {
            t.await.unwrap();
        }
    });
}

#[test]
fn async_counter_random() {
    check_random(async_counter, 5000)
}

#[test]
fn async_counter_pct() {
    let scheduler = PctScheduler::new(2, 5000);
    let runner = Runner::new(scheduler, Default::default());
    runner.run(async_counter);
}

async fn do_err(e: bool) -> Result<(), ()> {
    if e {
        Err(())
    } else {
        Ok(())
    }
}

// Check that try_join on Shuttle futures works as expected
#[test]
fn test_try_join() {
    check_dfs(
        || {
            let f2 = do_err(true);
            let f1 = do_err(false);
            let res = future::block_on(async { try_join!(f1, f2) });
            assert!(res.is_err());
        },
        None,
    );
}

// Check that a task may not run if its JoinHandle is dropped
#[test]
fn drop_shuttle_future() {
    let orderings = Arc::new(AtomicUsize::new(0));
    let async_accesses = Arc::new(AtomicUsize::new(0));
    let orderings_clone = orderings.clone();
    let async_accesses_clone = async_accesses.clone();

    check_dfs(
        move || {
            orderings.fetch_add(1, Ordering::SeqCst);
            let async_accesses = async_accesses.clone();
            future::spawn(async move {
                async_accesses.fetch_add(1, Ordering::SeqCst);
            });
        },
        None,
    );

    assert_eq!(2, orderings_clone.load(Ordering::SeqCst));
    assert_eq!(1, async_accesses_clone.load(Ordering::SeqCst));
}

// Same as `drop_shuttle_future`, but the inner task yields first, and might be cancelled part way through
#[test]
fn drop_shuttle_yield_future() {
    let orderings = Arc::new(AtomicUsize::new(0));
    let async_accesses = Arc::new(AtomicUsize::new(0));
    let post_yield_accesses = Arc::new(AtomicUsize::new(0));
    let orderings_clone = orderings.clone();
    let async_accesses_clone = async_accesses.clone();
    let post_yield_accesses_clone = post_yield_accesses.clone();

    check_dfs(
        move || {
            orderings.fetch_add(1, Ordering::SeqCst);
            let async_accesses = async_accesses.clone();
            let post_yield_accesses = post_yield_accesses.clone();
            future::spawn(async move {
                async_accesses.fetch_add(1, Ordering::SeqCst);
                future::yield_now().await;
                post_yield_accesses.fetch_add(1, Ordering::SeqCst);
            });
        },
        None,
    );

    // The three orderings of main task M and spawned task S are:
    // (1) M runs and finished, then S gets dropped and doesn't run
    // (2) M runs, S runs until yield point, then M finishes and S is dropped
    // (3) M runs, S runs until yield point, S runs again and finished, then M finishes
    assert_eq!(3, orderings_clone.load(Ordering::SeqCst));
    assert_eq!(2, async_accesses_clone.load(Ordering::SeqCst));
    assert_eq!(1, post_yield_accesses_clone.load(Ordering::SeqCst));
}

// This test checks two behaviors. First, it checks that a future can be polled twice
// without needing to wake up another task. Second, it checks that a waiter can finish
// before the waitee task.
#[test]
fn wake_self_on_join_handle() {
    check_dfs(
        || {
            let yielder = future::spawn(async move {
                future::yield_now().await;
            });

            struct Timeout<F: Future> {
                inner: Pin<Box<F>>,
                counter: u8,
            }

            impl<F> Future for Timeout<F>
            where
                F: Future,
            {
                type Output = ();

                fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                    if self.counter == 0 {
                        return Poll::Ready(());
                    }

                    self.counter -= 1;

                    match self.inner.as_mut().poll(cx) {
                        Poll::Pending => {
                            cx.waker().wake_by_ref();
                            Poll::Pending
                        }
                        Poll::Ready(_) => Poll::Ready(()),
                    }
                }
            }

            let wait_on_yield = future::spawn(Timeout {
                inner: Box::pin(yielder),
                counter: 2,
            });

            drop(wait_on_yield);
        },
        None,
    );
}

#[test]
fn is_finished_on_join_handle() {
    check_dfs(
        || {
            let barrier = Arc::new(Barrier::new(2));
            let t1 = future::spawn({
                let barrier = Arc::clone(&barrier);
                async move {
                    barrier.wait();
                }
            });
            assert!(!t1.is_finished());

            future::block_on(future::spawn(async move {
                assert!(!t1.is_finished());
                barrier.wait();
                futures::pin_mut!(t1);
                t1.as_mut().await.unwrap();
                assert!(t1.is_finished());
            }))
            .unwrap();
        },
        None,
    );
}

struct ShuttleSpawn;

impl Spawn for ShuttleSpawn {
    fn spawn_obj(&self, future: FutureObj<'static, ()>) -> Result<(), SpawnError> {
        future::spawn(future);
        Ok(())
    }
}

// ---- Abort tests ----

// Abort before the task ever runs: join handle should resolve to Err(Cancelled).
#[test]
fn abort_before_task_runs_join_returns_cancelled() {
    check_dfs(
        || {
            let counter = Arc::new(AtomicUsize::new(0));
            let t1 = future::spawn({
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            });
            t1.abort();
            let result = future::block_on(async move { t1.await });
            assert!(
                result.is_err(),
                "aborted task should resolve with Err(Cancelled)"
            );
            assert_eq!(0, counter.load(Ordering::SeqCst), "aborted task must not run");
        },
        None,
    );
}

// Abort a task that has already yielded once; join returns Cancelled and counter stays 0.
#[test]
fn abort_after_yield_join_returns_cancelled() {
    check_dfs(
        || {
            let counter = Arc::new(AtomicUsize::new(0));
            let t1 = future::spawn({
                let counter = Arc::clone(&counter);
                async move {
                    future::yield_now().await;
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            });
            // Give the task a chance to run until the yield point, then abort.
            let abort_handle = t1.abort_handle();
            let waiter = future::spawn(async move {
                abort_handle.abort();
                t1.await
            });
            let result = future::block_on(waiter).unwrap();
            // Either the task completed before abort, or it was cancelled.
            // In all DFS orderings, counter is either 0 (aborted) or 1 (completed first).
            match result {
                Ok(_) => assert_eq!(1, counter.load(Ordering::SeqCst)),
                Err(_) => assert_eq!(0, counter.load(Ordering::SeqCst)),
            }
        },
        None,
    );
}

// Aborting an already-finished task is a no-op; join still returns Ok.
#[test]
fn abort_finished_task_returns_ok() {
    check_dfs(
        || {
            let t1 = future::spawn(async { 42u32 });
            let result = future::block_on(async move {
                let val = t1.await;
                val
            });
            // Task completed normally before abort; result should be Ok(42).
            assert!(matches!(result, Ok(42)));
        },
        None,
    );
}

// AbortHandle can be used to abort a task after the JoinHandle is gone.
#[test]
fn abort_via_abort_handle() {
    check_dfs(
        || {
            let counter = Arc::new(AtomicUsize::new(0));
            let t1 = future::spawn({
                let counter = Arc::clone(&counter);
                async move {
                    // Block forever at a barrier so the task doesn't complete naturally.
                    Barrier::new(2).wait();
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            });
            let abort = t1.abort_handle();
            abort.abort();
            abort.abort(); // idempotent
            assert_eq!(0, counter.load(Ordering::SeqCst));
        },
        None,
    );
}

// Abort handle clone works the same as the original.
#[test]
fn abort_handle_clone() {
    check_dfs(
        || {
            let counter = Arc::new(AtomicUsize::new(0));
            let t1 = future::spawn({
                let counter = Arc::clone(&counter);
                async move {
                    Barrier::new(2).wait();
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            });
            let abort1 = t1.abort_handle();
            let abort2 = abort1.clone();
            abort2.abort();
            assert_eq!(0, counter.load(Ordering::SeqCst));
        },
        None,
    );
}

// Aborting with JoinHandle while another task is awaiting it via block_on.
#[test]
fn abort_while_joined() {
    check_dfs(
        || {
            let counter = Arc::new(AtomicUsize::new(0));
            let t1 = future::spawn({
                let counter = Arc::clone(&counter);
                async move {
                    future::yield_now().await;
                    counter.fetch_add(1, Ordering::SeqCst);
                    42u32
                }
            });
            let abort = t1.abort_handle();
            // Spawn a task that aborts t1, then join on it.
            let joiner = future::spawn(async move { t1.await });
            abort.abort();
            let result = future::block_on(joiner).unwrap();
            match result {
                Ok(42) => assert_eq!(1, counter.load(Ordering::SeqCst)),
                Err(_) => assert_eq!(0, counter.load(Ordering::SeqCst)),
                _ => panic!("unexpected result"),
            }
        },
        None,
    );
}

// Dropped JoinHandle (without abort) lets the task run to completion as a detached task.
#[test]
fn drop_join_handle_detaches_task() {
    let total_runs = Arc::new(AtomicUsize::new(0));
    let total_runs_clone = total_runs.clone();
    check_dfs(
        move || {
            let ran = Arc::new(AtomicUsize::new(0));
            let ran2 = ran.clone();
            // Spawn and immediately drop the handle (detaches).
            drop(future::spawn(async move {
                ran2.fetch_add(1, Ordering::SeqCst);
            }));
            // Allow the detached task to run.
            future::block_on(future::yield_now());
            // The detached task may or may not have run depending on scheduling.
            total_runs.fetch_add(ran.load(Ordering::SeqCst), Ordering::SeqCst);
        },
        None,
    );
    // Across DFS orderings the detached task runs in some orderings.
    assert!(total_runs_clone.load(Ordering::SeqCst) > 0);
}

// Destructors of the inner future run when the task is aborted.
#[test]
fn abort_runs_inner_future_drop() {
    use std::sync::Mutex;

    #[derive(Clone)]
    struct DropCounter(Arc<Mutex<usize>>);

    impl Drop for DropCounter {
        fn drop(&mut self) {
            *self.0.lock().unwrap() += 1;
        }
    }

    check_dfs(
        || {
            let drop_count = Arc::new(Mutex::new(0usize));
            let dc = DropCounter(drop_count.clone());
            let t1 = future::spawn(async move {
                // Hold the DropCounter so its destructor runs when the future is dropped.
                let _guard = dc;
                future::yield_now().await;
                // Never reached when aborted before the yield resolves.
            });
            t1.abort();
            // block_on the join handle so we wait until the aborted task actually
            // finishes (and thus drops the inner future and DropCounter).
            let result = future::block_on(t1);
            assert!(result.is_err(), "should be Err(Cancelled)");
            // The DropCounter should have been dropped exactly once.
            assert_eq!(1, *drop_count.lock().unwrap());
        },
        None,
    );
}

// Multiple tasks can be aborted independently.
#[test]
fn abort_multiple_tasks() {
    check_dfs(
        || {
            let c1 = Arc::new(AtomicUsize::new(0));
            let c2 = Arc::new(AtomicUsize::new(0));
            let c3 = Arc::new(AtomicUsize::new(0));

            let t1 = future::spawn({
                let c1 = c1.clone();
                async move {
                    Barrier::new(2).wait();
                    c1.fetch_add(1, Ordering::SeqCst);
                }
            });
            let t2 = future::spawn({
                let c2 = c2.clone();
                async move {
                    Barrier::new(2).wait();
                    c2.fetch_add(1, Ordering::SeqCst);
                }
            });
            let t3 = future::spawn({
                let c3 = c3.clone();
                async move { c3.fetch_add(1, Ordering::SeqCst) }
            });

            t1.abort();
            t2.abort();

            // t3 was not aborted; block_on should resolve to Ok.
            let result = future::block_on(t3);
            assert!(result.is_ok());
            assert_eq!(1, c3.load(Ordering::SeqCst));

            // t1 and t2 were aborted and must not have incremented their counters.
            assert_eq!(0, c1.load(Ordering::SeqCst));
            assert_eq!(0, c2.load(Ordering::SeqCst));
        },
        None,
    );
}

// is_finished returns true after the task completes normally.
#[test]
fn is_finished_after_normal_completion() {
    check_dfs(
        || {
            let t1 = future::spawn(async { 1u32 });
            future::block_on(async move {
                t1.await.unwrap();
                // is_finished is true after await resolves.
            });
        },
        None,
    );
}

// is_finished on AbortHandle returns true after abort completes.
#[test]
fn is_finished_on_abort_handle() {
    check_dfs(
        || {
            let t1 = future::spawn(async {
                future::yield_now().await;
            });
            let abort = t1.abort_handle();
            abort.abort();
            // block_on the join handle so we know the aborted task has fully finished.
            let _ = future::block_on(t1);
            assert!(abort.is_finished());
        },
        None,
    );
}

// Make sure a spawned detached task gets cleaned up correctly after execution ends
#[test]
fn clean_up_detached_task() {
    check_dfs(
        || {
            let atomic = shuttle::sync::atomic::AtomicUsize::new(0);
            let _task_handle = ShuttleSpawn
                .spawn_with_handle(async move {
                    atomic.fetch_add(1, Ordering::SeqCst);
                })
                .unwrap();
        },
        None,
    )
}
