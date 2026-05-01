//! Internal API surface for sibling crates (shuttle-sync, shuttle-schedulers).
//!
//! **This module is `#[doc(hidden)]` and not part of the public API.**
//! Types and functions here may change without notice.

// Runtime execution
pub use crate::runtime::execution::{ExecutionState, ExecutionStateBorrowError, LABELS, TASK_ID_TO_TAGS};

// Tasks
pub use crate::runtime::task::{
    ChildLabelFn, Task, TaskId, TaskName, TaskSet, TaskSignature, TaskState,
    DEFAULT_INLINE_TASKS,
};
#[allow(deprecated)]
pub use crate::runtime::task::Tag;
pub use crate::runtime::task::clock::VectorClock;
pub use crate::runtime::task::labels::Labels;

// Storage
pub use crate::runtime::storage::{AlreadyDestructedError, StorageKey, StorageMap};

// Thread / continuation primitives
pub use crate::runtime::thread::{switch, thread_fn};
pub use crate::runtime::thread::continuation::{
    ContinuationPool, PooledContinuation, CONTINUATION_POOL,
};

// Sync resource types
pub use crate::sync_types::{ResourceSignature, ResourceType};

// Thread-local key (for TLS support)
pub use crate::thread_local_key::{AccessError, LocalKey};

// Config re-exports
pub use crate::config::{
    backtrace_enabled, silence_warnings, Config, MaxSteps, UngracefulShutdownConfig,
    SILENCE_WARNINGS, UNGRACEFUL_SHUTDOWN_CONFIG,
};

// Annotations
pub use crate::annotations;

// Future support (block_on, yield_now, batch_semaphore)
pub use crate::future;
