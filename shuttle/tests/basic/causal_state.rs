use shuttle::sync::atomic::{AtomicBool, Ordering};
use shuttle::sync::{mpsc, Arc, Barrier, Condvar, Mutex, Once, RwLock};
use shuttle::thread;
use shuttle::{future, Config, Runner};
use shuttle_schedulers::{DfsScheduler, RandomScheduler, RoundRobinScheduler};
use std::sync::atomic::AtomicUsize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Bits(u64);

struct AssertBitOnDrop(u64);

impl Drop for AssertBitOnDrop {
    fn drop(&mut self) {
        assert_ne!(bits() & self.0, 0);
    }
}

fn runner() -> Runner<DfsScheduler> {
    let config = Config::default().with_causal_state(
        || Bits(1),
        |current, incoming| {
            current.0 |= incoming.0;
        },
    );
    Runner::new(DfsScheduler::new(Some(100), false), config)
}

fn set(bits: u64) {
    shuttle::current::causal::join_state(Bits(bits));
}

fn bits() -> u64 {
    shuttle::current::causal::state::<Bits>().0
}

#[test]
fn causal_state_is_inherited_and_joined() {
    runner().run(|| {
        assert_eq!(bits(), 1);
        assert_eq!(
            shuttle::current::get_label_for_task::<Bits>(shuttle::current::me()),
            None
        );
        shuttle::current::set_label_for_task(shuttle::current::me(), Bits(32));
        assert_eq!(bits(), 1);
        shuttle::current::remove_label_for_task::<Bits>(shuttle::current::me());
        assert_eq!(bits(), 1);
        set(3);

        let child = thread::spawn(|| {
            assert_eq!(bits(), 3);
            set(4);
        });

        child.join().unwrap();
        assert_eq!(bits(), 7);
    });
}

#[test]
fn causal_state_initializer_runs_once_per_execution() {
    static INITIALIZATIONS: AtomicUsize = AtomicUsize::new(0);

    let config = Config::default().with_causal_state(
        || {
            INITIALIZATIONS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Bits(1)
        },
        |current, incoming| current.0 |= incoming.0,
    );

    let executions = Runner::new(DfsScheduler::new(Some(20), false), config).run(|| {
        let child = thread::spawn(|| assert_eq!(bits(), 1));
        thread::yield_now();
        child.join().unwrap();
    });

    assert_eq!(INITIALIZATIONS.load(std::sync::atomic::Ordering::SeqCst), executions);
}

#[test]
fn stale_causal_state_is_ignored_by_static_atomics() {
    static ATOMIC: AtomicBool = AtomicBool::new(false);

    let config = Config::default().with_causal_state(
        || Bits(1),
        |current, incoming| {
            current.0 |= incoming.0;
        },
    );
    let executions = Runner::new(RandomScheduler::new_from_seed(0, 3), config).run(|| {
        ATOMIC.load(Ordering::SeqCst);
        assert_eq!(bits(), 1);

        let child = thread::spawn(thread::yield_now);
        child.join().unwrap();

        set(2);
        ATOMIC.store(true, Ordering::SeqCst);
    });

    assert!(executions > 1);
}

#[test]
fn stale_state_is_ignored_across_different_join_functions() {
    static ATOMIC: AtomicBool = AtomicBool::new(false);

    let first = Config::default().with_causal_state(
        || Bits(1),
        |current, incoming| {
            current.0 |= incoming.0;
        },
    );
    Runner::new(RoundRobinScheduler::new(1), first).run(|| {
        set(8);
        ATOMIC.store(true, Ordering::SeqCst);
    });

    let second = Config::default().with_causal_state(
        || Bits(1),
        |current, incoming| {
            current.0 = current.0.max(incoming.0);
        },
    );
    Runner::new(RoundRobinScheduler::new(1), second).run(|| {
        assert!(ATOMIC.load(Ordering::SeqCst));
        assert_eq!(bits(), 1);
    });
}

#[test]
fn custom_primitives_can_capture_and_join_causality() {
    let config = Config::default().with_causal_state(
        || Bits(1),
        |current, incoming| {
            current.0 |= incoming.0;
        },
    );
    Runner::new(RoundRobinScheduler::new(1), config).run(|| {
        let snapshot = Arc::new(std::sync::Mutex::new(shuttle::current::causal::Snapshot::new()));
        let ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let child = {
            let snapshot = Arc::clone(&snapshot);
            let ready = Arc::clone(&ready);
            thread::spawn(move || {
                while !ready.load(std::sync::atomic::Ordering::SeqCst) {
                    thread::yield_now();
                }

                let snapshot = snapshot.lock().unwrap().clone();
                shuttle::current::causal::join(&snapshot);
                assert_eq!(bits(), 3);
            })
        };

        set(2);
        let current = shuttle::current::causal::capture();
        snapshot.lock().unwrap().update(&current);
        ready.store(true, std::sync::atomic::Ordering::SeqCst);
        child.join().unwrap();
    });
}

#[test]
fn causal_state_is_joined_by_async_join_handle() {
    runner().run(|| {
        future::block_on(async {
            let child = future::spawn(async {
                set(2);
            });

            child.await.unwrap();
            assert_eq!(bits(), 3);
        });
    });
}

#[test]
fn causal_state_is_joined_by_cancelled_async_join_handle() {
    let config = Config::default().with_causal_state(
        || Bits(1),
        |current, incoming| {
            current.0 |= incoming.0;
        },
    );
    Runner::new(RoundRobinScheduler::new(1), config).run(|| {
        future::block_on(async {
            let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let child = {
                let started = Arc::clone(&started);
                future::spawn(async move {
                    let _check_abort_causality = AssertBitOnDrop(4);
                    set(2);
                    started.store(true, std::sync::atomic::Ordering::SeqCst);
                    std::future::pending::<()>().await;
                })
            };

            while !started.load(std::sync::atomic::Ordering::SeqCst) {
                future::yield_now().await;
            }

            set(4);
            child.abort();
            assert!(matches!(child.await, Err(future::JoinError::Cancelled)));
            assert_eq!(bits(), 7);
        });
    });
}

#[test]
fn causal_state_crosses_mutex_and_atomic_edges() {
    runner().run(|| {
        let mutex = Arc::new(Mutex::new(false));
        let atomic = Arc::new(AtomicBool::new(false));

        let child = {
            let mutex = Arc::clone(&mutex);
            let atomic = Arc::clone(&atomic);
            thread::spawn(move || {
                set(2);
                *mutex.lock().unwrap() = true;
                atomic.store(true, Ordering::SeqCst);
            })
        };

        if *mutex.lock().unwrap() {
            assert_ne!(bits() & 2, 0);
        }
        if atomic.load(Ordering::SeqCst) {
            assert_ne!(bits() & 2, 0);
        }

        child.join().unwrap();
    });
}

#[test]
fn causal_state_crosses_rwlock_edges() {
    let config = Config::default().with_causal_state(
        || Bits(1),
        |current, incoming| {
            current.0 |= incoming.0;
        },
    );
    Runner::new(RoundRobinScheduler::new(1), config).run(|| {
        let lock = Arc::new(RwLock::new(false));
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let child = {
            let lock = Arc::clone(&lock);
            let done = Arc::clone(&done);
            thread::spawn(move || {
                set(2);
                *lock.write().unwrap() = true;
                done.store(true, std::sync::atomic::Ordering::SeqCst);
            })
        };

        while !done.load(std::sync::atomic::Ordering::SeqCst) {
            thread::yield_now();
        }
        assert!(*lock.read().unwrap());
        assert_eq!(bits(), 3);
        child.join().unwrap();
    });
}

#[test]
fn causal_state_crosses_condvar_notifications() {
    runner().run(|| {
        let pair = Arc::new((Mutex::new(false), Condvar::new()));
        let child = {
            let pair = Arc::clone(&pair);
            thread::spawn(move || {
                let (lock, condvar) = &*pair;
                let mut ready = lock.lock().unwrap();
                set(2);
                *ready = true;
                condvar.notify_one();
            })
        };

        let (lock, condvar) = &*pair;
        let mut ready = lock.lock().unwrap();
        while !*ready {
            ready = condvar.wait(ready).unwrap();
        }
        assert_eq!(bits(), 3);
        child.join().unwrap();
    });
}

#[test]
fn causal_state_crosses_barrier_edges() {
    runner().run(|| {
        let barrier = Arc::new(Barrier::new(2));
        let child = {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                set(2);
                barrier.wait();
                assert_eq!(bits(), 3);
            })
        };

        barrier.wait();
        assert_eq!(bits(), 3);
        child.join().unwrap();
    });
}

#[test]
fn causal_state_crosses_unbounded_channel_edges() {
    runner().run(|| {
        let (tx, rx) = mpsc::channel();
        let child = thread::spawn(move || {
            set(2);
            tx.send(()).unwrap();
        });

        rx.recv().unwrap();
        assert_eq!(bits(), 3);
        child.join().unwrap();
    });
}

#[test]
fn causal_state_crosses_bounded_channel_edges_in_both_directions() {
    runner().run(|| {
        let (tx, rx) = mpsc::sync_channel(1);
        tx.send(0).unwrap();

        let child = thread::spawn(move || {
            set(2);
            assert_eq!(rx.recv().unwrap(), 0);
            assert_eq!(rx.recv().unwrap(), 1);
        });

        tx.send(1).unwrap();
        assert_ne!(bits() & 2, 0);
        child.join().unwrap();
    });
}

#[test]
fn causal_state_crosses_rendezvous_channel_edges_in_both_directions() {
    runner().run(|| {
        let (tx, rx) = mpsc::sync_channel(0);
        let child = thread::spawn(move || {
            set(2);
            rx.recv().unwrap();
            assert_eq!(bits(), 3);
        });

        tx.send(()).unwrap();
        assert_eq!(bits(), 3);
        child.join().unwrap();
    });
}

#[test]
fn causal_state_crosses_once_completion() {
    runner().run(|| {
        let once = Arc::new(Once::new());
        let initialized_by_child = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let child = {
            let once = Arc::clone(&once);
            let initialized_by_child = Arc::clone(&initialized_by_child);
            thread::spawn(move || {
                set(2);
                once.call_once(|| initialized_by_child.store(true, std::sync::atomic::Ordering::SeqCst));
            })
        };

        once.call_once(|| {});
        if initialized_by_child.load(std::sync::atomic::Ordering::SeqCst) {
            assert_ne!(bits() & 2, 0);
        }

        child.join().unwrap();
    });
}

#[test]
fn causal_state_crosses_implicit_scoped_thread_completion() {
    runner().run(|| {
        thread::scope(|scope| {
            scope.spawn(|| set(2));
        });

        assert_eq!(bits(), 3);
    });
}

#[test]
fn causal_state_crosses_channel_closure() {
    let config = Config::default().with_causal_state(
        || Bits(1),
        |current, incoming| {
            current.0 |= incoming.0;
        },
    );
    Runner::new(RoundRobinScheduler::new(1), config).run(|| {
        let (tx, rx) = mpsc::channel::<()>();
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let child = {
            let closed = Arc::clone(&closed);
            thread::spawn(move || {
                set(2);
                drop(rx);
                closed.store(true, std::sync::atomic::Ordering::SeqCst);
            })
        };

        while !closed.load(std::sync::atomic::Ordering::SeqCst) {
            thread::yield_now();
        }
        assert!(tx.send(()).is_err());
        assert_eq!(bits(), 3);
        child.join().unwrap();
    });

    let config = Config::default().with_causal_state(
        || Bits(1),
        |current, incoming| {
            current.0 |= incoming.0;
        },
    );
    Runner::new(RoundRobinScheduler::new(1), config).run(|| {
        let (tx, rx) = mpsc::channel::<()>();
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let child = {
            let closed = Arc::clone(&closed);
            thread::spawn(move || {
                set(2);
                drop(tx);
                closed.store(true, std::sync::atomic::Ordering::SeqCst);
            })
        };

        while !closed.load(std::sync::atomic::Ordering::SeqCst) {
            thread::yield_now();
        }
        assert!(rx.recv().is_err());
        assert_eq!(bits(), 3);
        child.join().unwrap();
    });
}
