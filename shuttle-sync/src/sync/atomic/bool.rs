use crate::sync::atomic::Atomic;
#[cfg(test)]
use shuttle_core::internal::ResourceSignature;
use std::sync::atomic::Ordering;

/// A boolean type which can be safely shared between threads.
pub struct AtomicBool {
    inner: Atomic<bool>,
}

impl Default for AtomicBool {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

impl From<bool> for AtomicBool {
    fn from(b: bool) -> Self {
        Self::new(b)
    }
}

impl std::fmt::Debug for AtomicBool {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        std::fmt::Debug::fmt(unsafe { &self.raw_load() }, f)
    }
}

impl AtomicBool {
    /// Creates a new atomic boolean.
    #[track_caller]
    pub const fn new(v: bool) -> Self {
        Self { inner: Atomic::new(v) }
    }

    /// Returns a mutable reference to the underlying boolean.
    pub fn get_mut(&mut self) -> &mut bool {
        self.inner.get_mut()
    }

    /// Consumes the atomic and returns the contained value.
    pub fn into_inner(self) -> bool {
        self.inner.into_inner()
    }

    /// Loads a value from the atomic boolean.
    pub fn load(&self, order: Ordering) -> bool {
        self.inner.load(order)
    }

    /// Stores a value into the atomic boolean.
    pub fn store(&self, val: bool, order: Ordering) {
        self.inner.store(val, order)
    }

    /// Stores a value into the atomic boolean, returning the previous value.
    pub fn swap(&self, val: bool, order: Ordering) -> bool {
        self.inner.swap(val, order)
    }

    /// Fetches the value, and applies a function to it that returns an optional new value.
    pub fn fetch_update<F>(&self, set_order: Ordering, fetch_order: Ordering, f: F) -> Result<bool, bool>
    where
        F: FnMut(bool) -> Option<bool>,
    {
        self.inner.fetch_update(set_order, fetch_order, f)
    }

    #[deprecated(since = "0.0.6", note = "Use `compare_exchange` or `compare_exchange_weak` instead")]
    pub fn compare_and_swap(&self, current: bool, new: bool, order: Ordering) -> bool {
        match self.compare_exchange(current, new, order, order) {
            Ok(v) => v,
            Err(v) => v,
        }
    }

    pub fn compare_exchange(
        &self,
        current: bool,
        new: bool,
        success: Ordering,
        failure: Ordering,
    ) -> Result<bool, bool> {
        self.fetch_update(success, failure, |val| (val == current).then_some(new))
    }

    pub fn compare_exchange_weak(
        &self,
        current: bool,
        new: bool,
        success: Ordering,
        failure: Ordering,
    ) -> Result<bool, bool> {
        self.compare_exchange(current, new, success, failure)
    }

    pub fn fetch_and(&self, val: bool, order: Ordering) -> bool {
        self.fetch_update(order, order, |old| Some(old & val)).unwrap()
    }

    pub fn fetch_nand(&self, val: bool, order: Ordering) -> bool {
        self.fetch_update(order, order, |old| Some(!(old & val))).unwrap()
    }

    pub fn fetch_or(&self, val: bool, order: Ordering) -> bool {
        self.fetch_update(order, order, |old| Some(old | val)).unwrap()
    }

    pub fn fetch_xor(&self, val: bool, order: Ordering) -> bool {
        self.fetch_update(order, order, |old| Some(old ^ val)).unwrap()
    }

    pub unsafe fn raw_load(&self) -> bool {
        self.inner.raw_load()
    }

    #[cfg(test)]
    pub(crate) fn signature(&self) -> ResourceSignature {
        self.inner.signature()
    }
}
