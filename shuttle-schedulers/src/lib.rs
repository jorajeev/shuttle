//! Scheduler implementations for Shuttle concurrency testing.
//!
//! This crate provides all concrete scheduler implementations (random, PCT, DFS, etc.)
//! that implement the [`Scheduler`] trait defined in `shuttle-core`.
//! It is not intended for direct use — depend on the `shuttle` facade crate instead.

mod annotation;
mod dfs;
mod pct;
mod random;
mod replay;
mod round_robin;
mod uncontrolled_nondeterminism;
mod urw;

pub use annotation::AnnotationScheduler;
pub use dfs::DfsScheduler;
pub use pct::PctScheduler;
pub use random::RandomScheduler;
pub use replay::ReplayScheduler;
pub use round_robin::RoundRobinScheduler;
pub use uncontrolled_nondeterminism::UncontrolledNondeterminismCheckScheduler;
pub use urw::UrwRandomScheduler;

// Re-export core scheduler types for convenience
pub use shuttle_core::scheduler::data::DataSource;
pub use shuttle_core::scheduler::{Schedule, Scheduler, Task, TaskId};
