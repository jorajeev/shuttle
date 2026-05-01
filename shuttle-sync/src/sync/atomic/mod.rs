//! Atomic types
//!
//! See [`std::sync::atomic`] documentation for more details.

mod bool;
mod int;
mod ptr;

pub use self::bool::AtomicBool;
pub use int::*;
pub use ptr::AtomicPtr;
pub use std::sync::atomic::Ordering;

use shuttle_core::internal::{ExecutionState, VectorClock, ResourceSignature, ResourceType, switch, silence_warnings};
use std::cell::RefCell;
use std::panic::RefUnwindSafe;

static PRINTED_ORDERING_WARNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[inline]
fn maybe_warn_about_ordering(order: Ordering) {
    use owo_colors::OwoColorize;

    #[allow(clippy::collapsible_if)]
    if order != Ordering::SeqCst {
        if PRINTED_ORDERING_WARNING
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            if silence_warnings() {
                return;
            }

            if ExecutionState::with(|state| state.config.silence_warnings) {
                return;
            }

            eprintln!(
                "{}: Shuttle only correctly models SeqCst atomics and treats all other Orderings \
                as if they were SeqCst. Bugs caused by weaker orderings like {:?} may be missed. \
                See https://docs.rs/shuttle/*/shuttle/sync/atomic/index.html#warning-about-relaxed-behaviors \
                for details or to disable this warning.",
                "WARNING".yellow(),
                order
            );
        }
    }
}

/// An atomic fence, like the standard library's [std::sync::atomic::fence].
pub fn fence(order: Ordering) {
    if order == Ordering::Relaxed {
        panic!("there is no such thing as a relaxed fence");
    }

    maybe_warn_about_ordering(order);
}

pub use std::sync::atomic::compiler_fence;

/// Base type for atomic implementations.
#[derive(Debug)]
pub(super) struct Atomic<T> {
    inner: RefCell<T>,
    clock: RefCell<Option<VectorClock>>,
    #[allow(unused)]
    signature: ResourceSignature,
}

unsafe impl<T: Sync> Sync for Atomic<T> {}
impl<T: RefUnwindSafe> RefUnwindSafe for Atomic<T> {}

impl<T> Atomic<T> {
    #[track_caller]
    pub(super) const fn new(v: T) -> Self {
        Self {
            inner: RefCell::new(v),
            clock: RefCell::new(None),
            signature: ResourceSignature::new_const(ResourceType::Atomic),
        }
    }
}

impl<T: Copy + Eq> Atomic<T> {
    pub(super) fn get_mut(&mut self) -> &mut T {
        self.exhale_clock();
        self.inner.get_mut()
    }

    pub(super) fn into_inner(self) -> T {
        self.exhale_clock();
        self.inner.into_inner()
    }

    pub(super) fn load(&self, order: Ordering) -> T {
        maybe_warn_about_ordering(order);
        switch();
        self.exhale_clock();
        let value = *self.inner.borrow();
        value
    }

    pub(super) fn store(&self, val: T, order: Ordering) {
        maybe_warn_about_ordering(order);
        switch();
        self.inhale_clock();
        *self.inner.borrow_mut() = val;
    }

    pub(super) fn swap(&self, mut val: T, order: Ordering) -> T {
        maybe_warn_about_ordering(order);
        switch();
        self.exhale_clock();
        self.inhale_clock();
        std::mem::swap(&mut *self.inner.borrow_mut(), &mut val);
        val
    }

    pub(super) fn fetch_update<F>(&self, set_order: Ordering, fetch_order: Ordering, mut f: F) -> Result<T, T>
    where
        F: FnMut(T) -> Option<T>,
    {
        maybe_warn_about_ordering(set_order);
        maybe_warn_about_ordering(fetch_order);

        switch();
        self.exhale_clock();
        let current = *self.inner.borrow();
        let ret = if let Some(new) = f(current) {
            *self.inner.borrow_mut() = new;
            self.inhale_clock();
            Ok(current)
        } else {
            Err(current)
        };
        ret
    }

    pub(super) unsafe fn raw_load(&self) -> T {
        *self.inner.borrow()
    }

    fn init_clock(&self) {
        self.clock.borrow_mut().get_or_insert(VectorClock::new());
    }

    fn inhale_clock(&self) {
        self.init_clock();
        ExecutionState::with(|s| {
            let clock = s.increment_clock();
            let mut self_clock = self.clock.borrow_mut();
            self_clock.as_mut().unwrap().update(clock);
        });
    }

    fn exhale_clock(&self) {
        self.init_clock();
        ExecutionState::with(|s| {
            let self_clock = self.clock.borrow();
            s.update_clock(self_clock.as_ref().unwrap());
        });
    }

    #[cfg(test)]
    pub(super) fn signature(&self) -> ResourceSignature {
        self.signature.clone()
    }
}
