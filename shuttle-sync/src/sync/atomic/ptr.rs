use crate::sync::atomic::Atomic;
#[cfg(test)]
use shuttle_core::internal::ResourceSignature;
use std::sync::atomic::Ordering;

/// A raw pointer type which can be safely shared between threads.
pub struct AtomicPtr<T> {
    inner: Atomic<*mut T>,
}

impl<T> Default for AtomicPtr<T> {
    fn default() -> Self {
        Self::new(std::ptr::null_mut())
    }
}

impl<T> From<*mut T> for AtomicPtr<T> {
    fn from(p: *mut T) -> Self {
        Self::new(p)
    }
}

impl<T> std::fmt::Debug for AtomicPtr<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        std::fmt::Debug::fmt(unsafe { &self.raw_load() }, f)
    }
}

unsafe impl<T> Send for AtomicPtr<T> {}
unsafe impl<T> Sync for AtomicPtr<T> {}

impl<T> AtomicPtr<T> {
    #[track_caller]
    pub const fn new(v: *mut T) -> Self {
        Self { inner: Atomic::new(v) }
    }

    pub fn get_mut(&mut self) -> &mut *mut T {
        self.inner.get_mut()
    }

    pub fn into_inner(self) -> *mut T {
        self.inner.into_inner()
    }

    pub fn load(&self, order: Ordering) -> *mut T {
        self.inner.load(order)
    }

    pub fn store(&self, val: *mut T, order: Ordering) {
        self.inner.store(val, order)
    }

    pub fn swap(&self, val: *mut T, order: Ordering) -> *mut T {
        self.inner.swap(val, order)
    }

    pub fn fetch_update<F>(&self, set_order: Ordering, fetch_order: Ordering, f: F) -> Result<*mut T, *mut T>
    where
        F: FnMut(*mut T) -> Option<*mut T>,
    {
        self.inner.fetch_update(set_order, fetch_order, f)
    }

    #[deprecated(since = "0.0.6", note = "Use `compare_exchange` or `compare_exchange_weak` instead")]
    pub fn compare_and_swap(&self, current: *mut T, new: *mut T, order: Ordering) -> *mut T {
        match self.compare_exchange(current, new, order, order) {
            Ok(v) => v,
            Err(v) => v,
        }
    }

    pub fn compare_exchange(
        &self,
        current: *mut T,
        new: *mut T,
        success: Ordering,
        failure: Ordering,
    ) -> Result<*mut T, *mut T> {
        self.fetch_update(success, failure, |val| (val == current).then_some(new))
    }

    pub fn compare_exchange_weak(
        &self,
        current: *mut T,
        new: *mut T,
        success: Ordering,
        failure: Ordering,
    ) -> Result<*mut T, *mut T> {
        self.compare_exchange(current, new, success, failure)
    }

    pub unsafe fn raw_load(&self) -> *mut T {
        self.inner.raw_load()
    }

    #[cfg(test)]
    pub(crate) fn signature(&self) -> ResourceSignature {
        self.inner.signature()
    }
}
