//! Shuttle's implementation of [`std::sync`].
//!
//! Shuttle provides drop-in replacements for several [`std::sync`] types. Using these replacements
//! allows Shuttle to control the scheduling of synchronization operations and so explore different
//! interleavings of concurrent code.

pub mod atomic;
mod barrier;
mod condvar;
pub mod mpsc;
mod mutex;
mod once;
mod rwlock;

pub use barrier::{Barrier, BarrierWaitResult};
pub use condvar::{Condvar, WaitTimeoutResult};
pub use mutex::{Mutex, MutexGuard};
pub use once::{Once, OnceState};
pub use rwlock::{RwLock, RwLockReadGuard, RwLockWriteGuard};

pub use shuttle_core::sync_types::{ResourceSignature, ResourceType};

pub use std::sync::{LockResult, PoisonError, TryLockError, TryLockResult};
