//! Shuttle's implementation of the [`lazy_static`] crate, v1.4.0.
//!
//! [`lazy_static`]: https://crates.io/crates/lazy_static

use shuttle_core::internal::{ExecutionState, StorageKey, silence_warnings};
use crate::sync::Once;
use std::marker::PhantomData;

#[doc(hidden)]
pub use core::ops::Deref as __Deref;

pub use crate::lazy_static;

/// Shuttle's implementation of `lazy_static::Lazy`.
pub struct Lazy<T: Sync> {
    cell: Once,
    init: fn() -> T,
    _p: PhantomData<T>,
}

impl<T: Sync> std::fmt::Debug for Lazy<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lazy").finish_non_exhaustive()
    }
}

impl<T: Sync> Lazy<T> {
    /// Constructs a new `Lazy` with a given function for lazy initialization.
    pub const fn new(init: fn() -> T) -> Self {
        Self {
            cell: Once::new(),
            init,
            _p: PhantomData,
        }
    }

    /// Get a reference to the lazy value, initializing it first if necessary.
    pub fn get(&'static self) -> &'static T {
        unsafe fn extend_lt<T>(t: &T) -> &'static T {
            std::mem::transmute(t)
        }

        let initialize = ExecutionState::with(|state| state.get_storage::<_, DropGuard<T>>(self).is_none());

        if initialize {
            self.cell.call_once(|| {
                let value = (self.init)();
                ExecutionState::with(|state| {
                    state.init_storage(self, DropGuard::new(value, state.config.silence_warnings))
                });
            });
        }

        ExecutionState::with(|state| {
            let drop_guard: &DropGuard<T> = state.get_storage(self).expect("should be initialized");
            unsafe { extend_lt(&drop_guard.value) }
        })
    }
}

impl<T: Sync> From<&Lazy<T>> for StorageKey {
    fn from(lazy: &Lazy<T>) -> Self {
        StorageKey(lazy as *const _ as usize, 0x3)
    }
}

/// Support trait for enabling a few common operation on lazy static values.
pub trait LazyStatic {
    #[doc(hidden)]
    fn initialize(lazy: &Self);
}

/// Takes a shared reference to a lazy static and initializes it if it has not been already.
pub fn initialize<T: LazyStatic>(lazy: &T) {
    LazyStatic::initialize(lazy);
}

static PRINTED_DROP_WARNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn maybe_warn_about_drop(silence_warnings_config: bool) {
    use owo_colors::OwoColorize;
    use std::sync::atomic::Ordering;

    if PRINTED_DROP_WARNING
        .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        if silence_warnings_config || silence_warnings() {
            return;
        }

        eprintln!(
            "{}: Shuttle runs the `Drop` method of `lazy_static` values at the end of an execution, \
                unlike the actual `lazy_static` implementation. This difference may cause false positives. \
                See https://docs.rs/shuttle/*/shuttle/lazy_static/index.html#warning-about-drop-behavior \
                for details or to disable this warning.",
            "WARNING".yellow(),
        );
    }
}

#[derive(Debug)]
struct DropGuard<T> {
    value: T,
    silence_warnings: bool,
}

impl<T> DropGuard<T> {
    fn new(value: T, silence_warnings: bool) -> Self {
        Self {
            value,
            silence_warnings,
        }
    }
}

impl<T> Drop for DropGuard<T> {
    fn drop(&mut self) {
        maybe_warn_about_drop(self.silence_warnings);
    }
}
