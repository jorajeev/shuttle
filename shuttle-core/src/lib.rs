//! Core runtime, scheduler trait, and internal API for Shuttle concurrency testing.
//!
//! This crate provides the execution engine and foundational types.
//! It is not intended for direct use — depend on the `shuttle` facade crate instead.

pub mod annotations;
pub mod config;
pub mod current;
pub mod future;
pub mod hint;
#[doc(hidden)]
pub mod runtime;
pub mod scheduler;
pub mod sync_types;
pub mod thread_local_key;

#[doc(hidden)]
pub mod internal;

// Re-exports for the public API (consumed by the facade crate)
pub use config::{
    backtrace_enabled, Config, ContinuationFunctionBehavior, FailurePersistence, MaxSteps,
    UngracefulShutdownConfig, CAPTURE_BACKTRACE, SILENCE_WARNINGS, ANNOTATION_FILE,
};
pub use runtime::runner::{PortfolioRunner, Runner};
