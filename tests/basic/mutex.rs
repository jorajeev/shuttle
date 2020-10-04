use shuttle::scheduler::PCTScheduler;
use shuttle::sync::Mutex;
use shuttle::{check, check_random, thread, Runner};
use std::sync::Arc;

#[test]
fn basic_lock_test() {
    check(move || {
        let lock = Arc::new(Mutex::new(0usize));
        let lock_clone = Arc::clone(&lock);

        thread::spawn(move || {
            let mut counter = lock_clone.lock().unwrap();
            *counter += 1;
        });

        let mut counter = lock.lock().unwrap();
        *counter += 1;
    });

    // TODO would be cool if we were allowed to smuggle the lock out of the run,
    // TODO so we can assert invariants about it *after* execution ends
}

// TODO we need a test that actually validates mutual exclusion? or is `deadlock` enough?

fn deadlock() {
    let lock1 = Arc::new(Mutex::new(0usize));
    let lock2 = Arc::new(Mutex::new(0usize));
    let lock1_clone = Arc::clone(&lock1);
    let lock2_clone = Arc::clone(&lock2);

    thread::spawn(move || {
        let _l1 = lock1_clone.lock().unwrap();
        let _l2 = lock2_clone.lock().unwrap();
    });

    let _l2 = lock2.lock().unwrap();
    let _l1 = lock1.lock().unwrap();
}

#[test]
#[should_panic(expected = "deadlock")]
fn deadlock_default() {
    // Round-robin should always fail this deadlock test
    check(|| deadlock());
}

#[test]
#[should_panic(expected = "deadlock")]
fn deadlock_random() {
    // 200 tries should be enough to find a deadlocking execution
    check_random(|| deadlock(), 200);
}

#[test]
#[should_panic(expected = "deadlock")]
fn deadlock_pct() {
    // 200 tries should be enough to find a deadlocking execution
    let scheduler = PCTScheduler::new(2, 100);
    let runner = Runner::new(scheduler);
    runner.run(deadlock);
}