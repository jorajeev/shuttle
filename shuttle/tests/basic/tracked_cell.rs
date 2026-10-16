//! Tests for `TrackedCell`, which reports its accesses to the scheduling-point checker (see
//! `scheduling_point_checker.rs`). The checker is on with the `check-scheduling-points` feature.

use shuttle::sync::{Arc, Mutex, TrackedCell};
use shuttle::{check_dfs, check_random, thread};
use shuttle_engine::runtime::thread::switch;
use std::collections::HashSet;

/// Two tasks that each update two cells, with a scheduling point before each update.
#[test]
fn updates_with_scheduling_points_pass() {
    check_dfs(
        || {
            let a = Arc::new(TrackedCell::new(0));
            let b = Arc::new(TrackedCell::new(0));
            let handles = (0..2)
                .map(|_| {
                    let a = Arc::clone(&a);
                    let b = Arc::clone(&b);
                    thread::spawn(move || {
                        switch();
                        a.set(a.get() + 1);
                        switch();
                        *b.borrow_mut() += 1;
                    })
                })
                .collect::<Vec<_>>();
            for handle in handles {
                handle.join().unwrap();
            }
            switch();
            assert!(a.get() >= 1 && *b.borrow() >= 1);
        },
        None,
    );
}

/// The same, without the scheduling point between the updates.
fn update_two_cells_in_one_step() {
    let a = Arc::new(TrackedCell::new(0));
    let b = Arc::new(TrackedCell::new(0));
    let handle = {
        let a = Arc::clone(&a);
        let b = Arc::clone(&b);
        thread::spawn(move || {
            switch();
            a.set(a.get() + 1);
            *b.borrow_mut() += 1;
        })
    };
    handle.join().unwrap();
}

#[cfg(feature = "check-scheduling-points")]
#[test]
#[should_panic(expected = "missing scheduling point")]
fn updates_without_scheduling_point_fail() {
    check_dfs(update_two_cells_in_one_step, None);
}

#[cfg(not(feature = "check-scheduling-points"))]
#[test]
fn updates_without_scheduling_point_pass_without_checker() {
    check_dfs(update_two_cells_in_one_step, None);
}

/// A failure names the cell by where it was created.
#[cfg(feature = "check-scheduling-points")]
#[test]
#[should_panic(expected = "TrackedCell[ 1st@shuttle/tests/basic/tracked_cell.rs")]
fn failure_names_the_cells() {
    check_dfs(update_two_cells_in_one_step, None);
}

/// Cells created at the same places in each execution have the same signatures in every execution,
/// and different signatures from each other.
#[test]
fn signatures_are_stable_across_executions() {
    let signatures = std::sync::Arc::new(std::sync::Mutex::new(HashSet::new()));
    let signatures_clone = std::sync::Arc::clone(&signatures);
    check_random(
        move || {
            let a = TrackedCell::new(0);
            let cells = (0..2).map(|_| TrackedCell::new(0)).collect::<Vec<_>>();
            let mut signatures = signatures_clone.lock().unwrap();
            signatures.insert(a.signature().clone());
            for cell in &cells {
                signatures.insert(cell.signature().clone());
            }
            // A cell that a spawned task creates is distinguished by its task too.
            let signature = Arc::new(Mutex::new(None));
            let signature_clone = Arc::clone(&signature);
            thread::spawn(move || {
                let cell = TrackedCell::new(0);
                *signature_clone.lock().unwrap() = Some(cell.signature().clone());
            })
            .join()
            .unwrap();
            signatures.insert(signature.lock().unwrap().take().unwrap());
        },
        10,
    );
    assert_eq!(signatures.lock().unwrap().len(), 4);
}

/// Exclusive access and reading the value for formatting don't count as accesses.
#[test]
fn exclusive_access_and_debug_are_not_reported() {
    check_dfs(
        || {
            let a = TrackedCell::new(0);
            let mut b = TrackedCell::new(0);
            switch();
            a.set(1);
            *b.get_mut() = 1;
            let _ = format!("{b:?}");
            assert_eq!(b.into_inner(), 1);
        },
        None,
    );
}
