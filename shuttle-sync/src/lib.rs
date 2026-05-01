//! Synchronization primitives for Shuttle concurrency testing.
//!
//! This crate provides drop-in replacements for `std::sync` types
//! (Mutex, RwLock, Condvar, atomics, mpsc, etc.) that are deterministically
//! scheduled by the Shuttle runtime.
//! It is not intended for direct use — depend on the `shuttle` facade crate instead.

pub mod sync;
pub mod thread;
pub mod rand;
pub mod lazy_static;
pub mod future;

/// The `lazy_static!` macro for Shuttle.
///
/// This is a re-implementation of the `lazy_static!` macro that uses Shuttle's `Once` primitive
/// to mediate initialization, ensuring deterministic initialization under Shuttle's scheduler.
#[macro_export]
macro_rules! lazy_static {
    // Implementation adapted from the real lazy_static crate
    ($(#[$attr:meta])* static ref $N:ident : $T:ty = $e:expr; $($t:tt)*) => {
        $crate::__lazy_static_internal!($(#[$attr])* () static ref $N : $T = $e; $($t)*);
    };
    ($(#[$attr:meta])* pub static ref $N:ident : $T:ty = $e:expr; $($t:tt)*) => {
        $crate::__lazy_static_internal!($(#[$attr])* (pub) static ref $N : $T = $e; $($t)*);
    };
    ($(#[$attr:meta])* pub ($($vis:tt)+) static ref $N:ident : $T:ty = $e:expr; $($t:tt)*) => {
        $crate::__lazy_static_internal!($(#[$attr])* (pub($($vis)+)) static ref $N : $T = $e; $($t)*);
    };
    () => ()
}

#[macro_export]
#[doc(hidden)]
macro_rules! __lazy_static_internal {
    ($(#[$attr:meta])* ($($vis:tt)*) static ref $N:ident : $T:ty = $e:expr; $($t:tt)*) => {
        #[allow(missing_copy_implementations)]
        #[allow(non_camel_case_types)]
        #[allow(dead_code)]
        $(#[$attr])*
        $($vis)* struct $N {__private_field: ()}
        #[doc(hidden)]
        $($vis)* static $N: $N = $N {__private_field: ()};
        impl $crate::lazy_static::__Deref for $N {
            type Target = $T;
            fn deref(&self) -> &$T {
                #[inline(always)]
                fn __static_ref_initialize() -> $T { $e }
                #[inline(always)]
                fn __stability() -> &'static $T {
                    static LAZY: $crate::lazy_static::Lazy<$T> =
                        $crate::lazy_static::Lazy::new(__static_ref_initialize);
                    LAZY.get()
                }
                __stability()
            }
        }
        impl $crate::lazy_static::LazyStatic for $N {
            fn initialize(lazy: &Self) {
                let _ = &**lazy;
            }
        }
        $crate::lazy_static!($($t)*);
    };
}
