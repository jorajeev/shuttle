#![cfg(all(feature = "sync", feature = "rt"))]

use shuttle::current::causal;
use shuttle::future;
use shuttle::scheduler::RoundRobinScheduler;
use shuttle::{Config, Runner};
use shuttle_tokio_impl_inner::sync::{mpsc, oneshot, watch, Notify, Semaphore};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Copy)]
struct Bits(u8);

fn set(bits: u8) {
    causal::join_state(Bits(bits));
}

fn bits() -> u8 {
    causal::state::<Bits>().0
}

fn run(f: impl Fn() + Send + Sync + 'static) {
    let config = Config::default().with_causal_state(
        || Bits(1),
        |current, incoming| {
            current.0 |= incoming.0;
        },
    );
    Runner::new(RoundRobinScheduler::new(1), config).run(f);
}

#[test]
fn oneshot_propagates_send_and_close() {
    run(|| {
        future::block_on(async {
            let (tx, rx) = oneshot::channel();
            let child = future::spawn(async move {
                set(2);
                tx.send(()).unwrap();
            });

            let before = shuttle::current::clock();
            rx.await.unwrap();
            assert_eq!(shuttle::current::clock(), before);
            assert_eq!(bits(), 3);
            child.await.unwrap();
        });
    });

    run(|| {
        future::block_on(async {
            let (tx, rx) = oneshot::channel::<()>();
            let child = future::spawn(async move {
                set(2);
                drop(tx);
            });

            let before = shuttle::current::clock();
            assert!(rx.await.is_err());
            assert_eq!(shuttle::current::clock(), before);
            assert_eq!(bits(), 3);
            child.await.unwrap();
        });
    });
}

#[test]
fn notify_propagates_waiting_and_stored_notifications() {
    run(|| {
        future::block_on(async {
            let notify = Arc::new(Notify::new());
            let notified = notify.notified();
            let child = {
                let notify = Arc::clone(&notify);
                future::spawn(async move {
                    set(2);
                    notify.notify_one();
                })
            };

            let before = shuttle::current::clock();
            notified.await;
            assert_eq!(shuttle::current::clock(), before);
            assert_eq!(bits(), 3);
            child.await.unwrap();
        });
    });

    run(|| {
        future::block_on(async {
            let notify = Arc::new(Notify::new());
            let done = Arc::new(AtomicBool::new(false));
            let child = {
                let notify = Arc::clone(&notify);
                let done = Arc::clone(&done);
                future::spawn(async move {
                    set(2);
                    notify.notify_one();
                    done.store(true, Ordering::SeqCst);
                })
            };

            while !done.load(Ordering::SeqCst) {
                future::yield_now().await;
            }
            let before = shuttle::current::clock();
            notify.notified().await;
            assert_eq!(shuttle::current::clock(), before);
            assert_eq!(bits(), 3);
            child.await.unwrap();
        });
    });
}

#[test]
fn watch_changed_propagates_changes_and_close() {
    run(|| {
        future::block_on(async {
            let (tx, mut rx) = watch::channel(0);
            let child = future::spawn(async move {
                set(2);
                tx.send(1).unwrap();
            });

            let before = shuttle::current::clock();
            rx.changed().await.unwrap();
            assert_eq!(shuttle::current::clock(), before);
            assert_eq!(bits(), 3);
            child.await.unwrap();
        });
    });

    run(|| {
        future::block_on(async {
            let (tx, mut rx) = watch::channel::<u8>(0);
            let child = future::spawn(async move {
                set(2);
                drop(tx);
            });

            let before = shuttle::current::clock();
            assert!(rx.changed().await.is_err());
            assert_eq!(shuttle::current::clock(), before);
            assert_eq!(bits(), 3);
            child.await.unwrap();
        });
    });
}

#[test]
fn mpsc_and_semaphore_propagate_close() {
    run(|| {
        future::block_on(async {
            let (tx, mut rx) = mpsc::channel::<()>(1);
            let child = future::spawn(async move {
                set(2);
                drop(tx);
            });

            let before = shuttle::current::clock();
            assert!(rx.recv().await.is_none());
            assert_eq!(shuttle::current::clock(), before);
            assert_eq!(bits(), 3);
            child.await.unwrap();
        });
    });

    run(|| {
        future::block_on(async {
            let (tx, rx) = mpsc::channel::<()>(1);
            let child = future::spawn(async move {
                set(2);
                drop(rx);
            });

            let before = shuttle::current::clock();
            tx.closed().await;
            assert_eq!(shuttle::current::clock(), before);
            assert_eq!(bits(), 3);
            child.await.unwrap();
        });
    });

    run(|| {
        future::block_on(async {
            let semaphore = Arc::new(Semaphore::new(0));
            let child = {
                let semaphore = Arc::clone(&semaphore);
                future::spawn(async move {
                    set(2);
                    semaphore.close();
                })
            };

            let before = shuttle::current::clock();
            assert!(semaphore.acquire().await.is_err());
            assert_eq!(shuttle::current::clock(), before);
            assert_eq!(bits(), 3);
            child.await.unwrap();
        });
    });
}
