//! Shuttle's implementation of [`std::hint`].

pub use std::hint::*;

/// Emits a machine instruction to signal the processor that it is running in a busy-wait spin-loop.
pub fn spin_loop() {
    std::hint::spin_loop();
    // Inline the yield logic (avoids dependency on shuttle-sync's thread module)
    let waker = crate::runtime::execution::ExecutionState::with(|state| state.current().waker());
    waker.wake_by_ref();
    crate::runtime::execution::ExecutionState::request_yield();
    crate::runtime::thread::switch();
}
