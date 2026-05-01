//! Shuttle's implementation of the [`rand`] crate, v0.8.
//!
//! [`rand`]: https://docs.rs/rand/0.8.5/rand/index.html

/// Random number generators and adapters
pub mod rngs {
    use shuttle_core::internal::ExecutionState;
    use rand::{CryptoRng, RngCore};
    use rand_core::impls::fill_bytes_via_next;

    /// A reference to the thread-local generator
    #[derive(Debug, Default, Clone)]
    pub struct ThreadRng {
        pub(super) _field: (),
    }

    impl RngCore for ThreadRng {
        #[inline]
        fn next_u32(&mut self) -> u32 {
            self.next_u64() as u32
        }

        #[inline]
        fn next_u64(&mut self) -> u64 {
            ExecutionState::next_u64()
        }

        fn fill_bytes(&mut self, dest: &mut [u8]) {
            fill_bytes_via_next(self, dest)
        }

        fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand::Error> {
            self.fill_bytes(dest);
            Ok(())
        }
    }

    impl CryptoRng for ThreadRng {}
}

/// Retrieve the thread-local random number generator, seeded by the system.
pub fn thread_rng() -> rngs::ThreadRng {
    rngs::ThreadRng { _field: () }
}

pub use rand::{Rng, RngCore};
