use shuttle::sync::atomic::{AtomicBool, Ordering};
use shuttle::sync::{mpsc, Arc, Barrier, Mutex, Once};
use shuttle::thread;
use shuttle::{future, Config, Runner};
use shuttle_schedulers::{DfsScheduler, RoundRobinScheduler};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Bits(u64);

fn runner() -> Runner<DfsScheduler> {
    let config = Config::default().with_causal_state(Bits(1), |current, incoming| {
        current.0 |= incoming.0;
    });
    Runner::new(DfsScheduler::new(Some(100), false), config)
}

fn set(bits: u64) {
    shuttle::current::set_causal_state(Bits(bits));
}

fn bits() -> u64 {
    shuttle::current::causal_state::<Bits>().0
}

#[test]
fn causal_state_is_inherited_and_joined() {
    runner().run(|| {
        assert_eq!(bits(), 1);
        assert_eq!(
            shuttle::current::get_label_for_task::<Bits>(shuttle::current::me()),
            Some(Bits(1))
        );
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
    let config = Config::default().with_causal_state(Bits(1), |current, incoming| {
        current.0 |= incoming.0;
    });
    Runner::new(RoundRobinScheduler::new(1), config).run(|| {
        future::block_on(async {
            let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let child = {
                let started = Arc::clone(&started);
                future::spawn(async move {
                    set(2);
                    started.store(true, std::sync::atomic::Ordering::SeqCst);
                    std::future::pending::<()>().await;
                })
            };

            while !started.load(std::sync::atomic::Ordering::SeqCst) {
                future::yield_now().await;
            }

            child.abort();
            assert!(matches!(child.await, Err(future::JoinError::Cancelled)));
            assert_eq!(bits(), 3);
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
