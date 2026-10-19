use shuttle::future::batch_semaphore::{BatchSemaphore, Fairness};
use shuttle::sync::{Mutex, RwLock};
use shuttle::{check_dfs, thread};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, PoisonError};
use test_log::test;

#[test]
fn mutex_poison() {
    check_dfs(
        || {
            let lock1 = Arc::new(Mutex::new(()));
            let lock1_clone = lock1.clone();

            let lock2 = Arc::new(Mutex::new(()));
            let _guard = lock2.lock();

            let jh = thread::spawn(move || {
                // panic while holding the mutex in order to poison it
                let _err = catch_unwind(move || {
                    let _lock = lock1_clone.lock().unwrap();
                    panic!();
                })
                .unwrap_err();
            });

            jh.join().unwrap();

            // lock1 should now be poisoned
            let result = lock1.lock();
            assert!(matches!(result, Err(PoisonError { .. })));

            drop(_guard);
            // lock2 should not be poisoned
            let lock_result = lock2.lock();
            assert!(lock_result.is_ok());
        },
        None,
    )
}

#[test]
fn rwlock_poison() {
    check_dfs(
        || {
            let lock1 = Arc::new(RwLock::new(()));
            let lock1_clone = lock1.clone();

            let lock2 = Arc::new(RwLock::new(()));
            let _guard = lock2.write();

            let jh = thread::spawn(move || {
                // panic while holding the mutex in order to poison it
                let _err = catch_unwind(move || {
                    let _lock = lock1_clone.write().unwrap();
                    panic!();
                })
                .unwrap_err();
            });

            jh.join().unwrap();

            // lock1 should now be poisoned
            let result = lock1.write();
            assert!(matches!(result, Err(PoisonError { .. })));

            drop(_guard);
            // lock2 should not be poisoned
            let lock_result = lock2.write();
            assert!(lock_result.is_ok());
        },
        None,
    )
}

/// Releases a semaphore's permit when dropped, as a lock guard does.
struct ReleaseOnDrop<'a>(&'a BatchSemaphore);

impl Drop for ReleaseOnDrop<'_> {
    fn drop(&mut self) {
        self.0.release(1);
    }
}

/// A task that holds a permit and panics releases it while unwinding. If the panic is caught, the
/// execution goes on, so a task waiting for the permit gets it. It used to be dropped from the
/// waiters, and to wait forever.
fn check_waiter_gets_permit_when_holder_panics(fairness: Fairness) {
    check_dfs(
        move || {
            let semaphore = Arc::new(BatchSemaphore::new(1, fairness));
            semaphore.acquire_blocking(1).unwrap();
            let waiter = {
                let semaphore = Arc::clone(&semaphore);
                thread::spawn(move || semaphore.acquire_blocking(1).unwrap())
            };
            catch_unwind(AssertUnwindSafe(|| {
                let _guard = ReleaseOnDrop(&semaphore);
                thread::yield_now();
                panic!("expected");
            }))
            .unwrap_err();
            waiter.join().unwrap();
        },
        None,
    )
}

#[test]
fn fair_waiter_gets_permit_when_holder_panics() {
    check_waiter_gets_permit_when_holder_panics(Fairness::StrictlyFair);
}

#[test]
fn unfair_waiter_gets_permit_when_holder_panics() {
    check_waiter_gets_permit_when_holder_panics(Fairness::Unfair);
}

/// A task that locks a mutex while another task panics with it locked gets the lock, poisoned or
/// not, depending on the order. It used to wait forever if it was waiting for the lock when the
/// panic released it, and to panic if the release came between its check for poisoning and its
/// acquire.
#[test]
fn mutex_lock_while_holder_panics() {
    check_dfs(
        || {
            let lock = Arc::new(Mutex::new(0));
            let holder = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    catch_unwind(AssertUnwindSafe(|| {
                        let _guard = lock.lock().unwrap();
                        thread::yield_now();
                        panic!("expected");
                    }))
                    .unwrap_err();
                })
            };
            let mut guard = lock.lock().unwrap_or_else(PoisonError::into_inner);
            *guard += 1;
            drop(guard);
            holder.join().unwrap();
            assert!(lock.lock().is_err());
        },
        None,
    )
}

/// The same for an `RwLock`, with a reader and a writer.
#[test]
fn rwlock_lock_while_holder_panics() {
    check_dfs(
        || {
            let lock = Arc::new(RwLock::new(0));
            let holder = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    catch_unwind(AssertUnwindSafe(|| {
                        let _guard = lock.write().unwrap();
                        thread::yield_now();
                        panic!("expected");
                    }))
                    .unwrap_err();
                })
            };
            let reader = {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    let _ = *lock.read().unwrap_or_else(PoisonError::into_inner);
                })
            };
            *lock.write().unwrap_or_else(PoisonError::into_inner) += 1;
            holder.join().unwrap();
            reader.join().unwrap();
            assert!(lock.write().is_err());
        },
        None,
    )
}
