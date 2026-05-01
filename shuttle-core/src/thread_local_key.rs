//! Thread-local storage key type.
//!
//! This type is extracted into shuttle-core because the runtime's Task type
//! needs to reference it for thread-local storage management.

use crate::runtime::execution::ExecutionState;
use std::marker::PhantomData;

/// A thread local storage key which owns its contents.
// Sadly, the fields of this thing need to be public because function pointers in const fns are
// unstable, so an explicit instantiation is the only way to construct this struct. User code should
// not rely on these fields.
pub struct LocalKey<T: 'static> {
    #[doc(hidden)]
    pub init: fn() -> T,
    #[doc(hidden)]
    pub _p: PhantomData<T>,
}

// Safety: `LocalKey` implements thread-local storage; each thread sees its own value of the type T.
unsafe impl<T> Send for LocalKey<T> {}
unsafe impl<T> Sync for LocalKey<T> {}

impl<T: 'static> std::fmt::Debug for LocalKey<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalKey").finish_non_exhaustive()
    }
}

impl<T: 'static> LocalKey<T> {
    /// Acquires a reference to the value in this TLS key.
    ///
    /// This will lazily initialize the value if this thread has not referenced this key yet.
    pub fn with<F, R>(&'static self, f: F) -> R
    where
        F: FnOnce(&T) -> R,
    {
        self.try_with(f).expect(
            "cannot access a Thread Local Storage value \
            during or after destruction",
        )
    }

    /// Acquires a reference to the value in this TLS key.
    ///
    /// This will lazily initialize the value if this thread has not referenced this key yet. If the
    /// key has been destroyed (which may happen if this is called in a destructor), this function
    /// will return an AccessError.
    pub fn try_with<F, R>(&'static self, f: F) -> std::result::Result<R, AccessError>
    where
        F: FnOnce(&T) -> R,
    {
        let value = self.get().unwrap_or_else(|| {
            let value = (self.init)();

            ExecutionState::with(move |state| {
                state.current_mut().init_local(self, value);
            });

            self.get().unwrap()
        })?;

        Ok(f(value))
    }

    fn get(&'static self) -> Option<std::result::Result<&'static T, AccessError>> {
        // Safety: see the usage below
        unsafe fn extend_lt<'b, T>(t: &'_ T) -> &'b T {
            std::mem::transmute(t)
        }

        ExecutionState::with(|state| {
            if let Ok(value) = state.current().local(self)? {
                // Safety: unfortunately the lifetime of a value in our thread-local storage is
                // bound to the lifetime of `ExecutionState`, which has no visible relation to the
                // lifetime of the thread we're running on. However, *we* know that the
                // `ExecutionState` outlives any thread, including the caller, and so it's safe to
                // give the caller the lifetime it's asking for here.
                Some(Ok(unsafe { extend_lt(value) }))
            } else {
                // Slot has already been destructed
                Some(Err(AccessError))
            }
        })
    }
}

/// An error returned by [`LocalKey::try_with`]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub struct AccessError;

impl std::fmt::Display for AccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt("already destroyed", f)
    }
}

impl std::error::Error for AccessError {}
