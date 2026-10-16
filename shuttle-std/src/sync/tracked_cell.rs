use crate::sync::{ResourceSignature, ResourceType};
use shuttle_engine::runtime::execution::ExecutionState;
use shuttle_engine::sync_types::AccessKind;
use std::cell::{Ref, RefCell, RefMut};
use std::fmt;

/// A shared, mutable value that reports every access to Shuttle's scheduling-point checker.
///
/// Wrappers that implement a primitive on top of Shuttle keep the primitive's shared state (state
/// that more than one task can access) in a `TrackedCell` rather than a `RefCell` or a std atomic.
/// Each borrow reports a read or a write of the cell (see
/// [`ExecutionState::record_access`](shuttle_engine::runtime::execution::ExecutionState::record_access)),
/// so that with the `check-scheduling-points` feature, a step that touches the cell and another
/// shared resource without a scheduling point in between fails the test.
///
/// A `TrackedCell` takes no scheduling point itself: the primitive that owns it takes one (with
/// `thread::switch`, say) before each operation that accesses the cell, as for any other shared
/// state. Don't hold a borrow across a scheduling point. Accesses are reported when the borrow is
/// taken, so the checker wouldn't see accesses through it in later steps (and another task's
/// conflicting borrow would panic).
pub struct TrackedCell<T> {
    value: RefCell<T>,
    signature: ResourceSignature,
}

impl<T> TrackedCell<T> {
    /// Creates a cell that holds `value`. Must be called in a Shuttle execution.
    #[track_caller]
    pub fn new(value: T) -> Self {
        Self {
            value: RefCell::new(value),
            signature: ExecutionState::new_resource_signature(ResourceType::TrackedCell),
        }
    }

    /// Creates a cell that holds `value`, for `const` contexts such as statics.
    ///
    /// The cell's signature can't depend on the task that creates it, so all cells created at the
    /// same source location have the same signature. That doesn't affect the scheduling-point
    /// checker, which tells resources apart by address, but it makes signatures less useful for
    /// telling resources apart across executions. Prefer [`TrackedCell::new`] where it can be used.
    #[track_caller]
    pub const fn const_new(value: T) -> Self {
        Self {
            value: RefCell::new(value),
            signature: ResourceSignature::new_const(ResourceType::TrackedCell),
        }
    }

    /// Borrows the value to read it, and reports a read of the cell.
    ///
    /// # Panics
    ///
    /// Panics if the value is mutably borrowed.
    #[track_caller]
    pub fn borrow(&self) -> Ref<'_, T> {
        ExecutionState::record_access(&self.signature, AccessKind::Read);
        self.value.borrow()
    }

    /// Borrows the value to change it, and reports a write of the cell.
    ///
    /// # Panics
    ///
    /// Panics if the value is borrowed.
    #[track_caller]
    pub fn borrow_mut(&self) -> RefMut<'_, T> {
        ExecutionState::record_access(&self.signature, AccessKind::Write);
        self.value.borrow_mut()
    }

    /// Returns a mutable reference to the value. No other task can access the cell meanwhile, so
    /// this isn't reported.
    pub fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }

    /// Returns the value. No other task can access the cell anymore, so this isn't reported.
    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    /// The cell's resource signature.
    pub fn signature(&self) -> &ResourceSignature {
        &self.signature
    }
}

impl<T: Copy> TrackedCell<T> {
    /// Returns a copy of the value, and reports a read of the cell.
    #[track_caller]
    pub fn get(&self) -> T {
        *self.borrow()
    }

    /// Sets the value, and reports a write of the cell.
    #[track_caller]
    pub fn set(&self, value: T) {
        *self.borrow_mut() = value;
    }

    /// Sets the value and returns the old one, and reports a write of the cell.
    #[track_caller]
    pub fn replace(&self, value: T) -> T {
        std::mem::replace(&mut *self.borrow_mut(), value)
    }
}

impl<T: Default> Default for TrackedCell<T> {
    #[track_caller]
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: fmt::Debug> fmt::Debug for TrackedCell<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Formatting reads the value, but it isn't an operation of the program, so don't report it.
        let mut d = f.debug_struct("TrackedCell");
        match self.value.try_borrow() {
            Ok(value) => d.field("value", &*value),
            Err(_) => d.field("value", &format_args!("<borrowed>")),
        };
        d.field("signature", &self.signature).finish()
    }
}

// Safety: Shuttle runs all of an execution's tasks on one OS thread, and a task can only switch at a
// scheduling point, not in the middle of a borrow, so the `RefCell` is never accessed from two real
// threads at once. This is the same argument as for `BatchSemaphore`, and it doesn't hold outside a
// Shuttle test, where a `TrackedCell` must not be shared between threads.
unsafe impl<T: Send> Sync for TrackedCell<T> {}
