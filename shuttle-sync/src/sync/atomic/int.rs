use crate::sync::atomic::Atomic;
use std::sync::atomic::Ordering;

macro_rules! atomic_int {
    ($name:ident, $int_type:ty) => {
        /// An integer type which can be safely shared between threads.
        pub struct $name {
            inner: Atomic<$int_type>,
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new(Default::default())
            }
        }

        impl From<$int_type> for $name {
            fn from(v: $int_type) -> Self {
                Self::new(v)
            }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                std::fmt::Debug::fmt(unsafe { &self.raw_load() }, f)
            }
        }

        impl $name {
            #[track_caller]
            pub const fn new(v: $int_type) -> Self {
                Self {
                    inner: Atomic::new(v),
                }
            }

            pub fn get_mut(&mut self) -> &mut $int_type {
                self.inner.get_mut()
            }

            pub fn into_inner(self) -> $int_type {
                self.inner.into_inner()
            }

            pub fn load(&self, order: Ordering) -> $int_type {
                self.inner.load(order)
            }

            pub fn store(&self, val: $int_type, order: Ordering) {
                self.inner.store(val, order)
            }

            pub fn swap(&self, val: $int_type, order: Ordering) -> $int_type {
                self.inner.swap(val, order)
            }

            pub fn fetch_update<F>(
                &self,
                set_order: Ordering,
                fetch_order: Ordering,
                f: F,
            ) -> Result<$int_type, $int_type>
            where
                F: FnMut($int_type) -> Option<$int_type>,
            {
                self.inner.fetch_update(set_order, fetch_order, f)
            }

            #[deprecated(
                since = "0.0.6",
                note = "Use `compare_exchange` or `compare_exchange_weak` instead"
            )]
            pub fn compare_and_swap(&self, current: $int_type, new: $int_type, order: Ordering) -> $int_type {
                match self.compare_exchange(current, new, order, order) {
                    Ok(v) => v,
                    Err(v) => v,
                }
            }

            pub fn compare_exchange(
                &self,
                current: $int_type,
                new: $int_type,
                success: Ordering,
                failure: Ordering,
            ) -> Result<$int_type, $int_type> {
                self.fetch_update(success, failure, |val| (val == current).then(|| new))
            }

            pub fn compare_exchange_weak(
                &self,
                current: $int_type,
                new: $int_type,
                success: Ordering,
                failure: Ordering,
            ) -> Result<$int_type, $int_type> {
                self.compare_exchange(current, new, success, failure)
            }

            pub fn fetch_add(&self, val: $int_type, order: Ordering) -> $int_type {
                self.fetch_update(order, order, |old| Some(old.wrapping_add(val)))
                    .unwrap()
            }

            pub fn fetch_sub(&self, val: $int_type, order: Ordering) -> $int_type {
                self.fetch_update(order, order, |old| Some(old.wrapping_sub(val)))
                    .unwrap()
            }

            pub fn fetch_and(&self, val: $int_type, order: Ordering) -> $int_type {
                self.fetch_update(order, order, |old| Some(old & val)).unwrap()
            }

            pub fn fetch_nand(&self, val: $int_type, order: Ordering) -> $int_type {
                self.fetch_update(order, order, |old| Some(!(old & val))).unwrap()
            }

            pub fn fetch_or(&self, val: $int_type, order: Ordering) -> $int_type {
                self.fetch_update(order, order, |old| Some(old | val)).unwrap()
            }

            pub fn fetch_xor(&self, val: $int_type, order: Ordering) -> $int_type {
                self.fetch_update(order, order, |old| Some(old ^ val)).unwrap()
            }

            pub fn fetch_max(&self, val: $int_type, order: Ordering) -> $int_type {
                self.fetch_update(order, order, |old| Some(old.max(val))).unwrap()
            }

            pub fn fetch_min(&self, val: $int_type, order: Ordering) -> $int_type {
                self.fetch_update(order, order, |old| Some(old.min(val))).unwrap()
            }

            pub unsafe fn raw_load(&self) -> $int_type {
                self.inner.raw_load()
            }

            #[cfg(test)]
            pub(crate) fn signature(&self) -> shuttle_core::internal::ResourceSignature {
                self.inner.signature()
            }
        }
    };
}

atomic_int!(AtomicI8, i8);
atomic_int!(AtomicI16, i16);
atomic_int!(AtomicI32, i32);
atomic_int!(AtomicI64, i64);
atomic_int!(AtomicIsize, isize);
atomic_int!(AtomicU8, u8);
atomic_int!(AtomicU16, u16);
atomic_int!(AtomicU32, u32);
atomic_int!(AtomicU64, u64);
atomic_int!(AtomicUsize, usize);
