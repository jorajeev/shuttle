//! Information about the current thread and current Shuttle execution.
//!
//! This module provides access to information about the current Shuttle execution. It is useful for
//! building tools that need to exploit Shuttle's total ordering of concurrent operations; for
//! example, a tool that wants to check linearizability might want access to a global timestamp for
//! events, which the [`context_switches`] function provides.
//!
//! This module also provides functions to manage the assocation of `labels` to threads and async tasks.
//! Labels are typed values that can be associated with a task.  They are useful for debugging: for
//! instance, the `TaskName` label can be set to assign names to tasks to make debug output easier to read.
//! Labels can also be used to build customized schedulers: for instance, they can be used to assign
//! numeric weights to tasks, which can be used to implement a priority-preemptive scheduler.

#[allow(deprecated)]
use crate::runtime::execution::TASK_ID_TO_TAGS;
use crate::runtime::execution::{CurrentSchedule, ExecutionState, LABELS};
use crate::runtime::task::clock::VectorClock;
pub use crate::runtime::task::labels::Labels;
pub use crate::runtime::task::{ChildLabelFn, TaskId, TaskName};
#[allow(deprecated)]
pub use crate::runtime::task::{Tag, Taggable};
use std::fmt::Debug;
use std::sync::Arc;

/// The number of context switches that happened so far in the current Shuttle execution.
///
/// Note that this is the number of *possible* context switches, i.e., including times when the
/// scheduler decided to continue with the same task. This means the result can be used as a
/// timestamp for atomic actions during an execution.
///
/// Panics if called outside of a Shuttle execution.
pub fn context_switches() -> usize {
    ExecutionState::context_switches()
}

/// Get the current thread's vector clock
pub fn clock() -> VectorClock {
    ExecutionState::with(|state| {
        let me = state.current();
        state.get_clock(me.id()).clone()
    })
}

/// Gets the clock for the thread with the given task ID
pub fn clock_for(task_id: TaskId) -> VectorClock {
    ExecutionState::with(|state| state.get_clock(task_id).clone())
}

/// Causal state associated with the current task.
///
/// Applications normally use [`state`] and [`join_state`]. The opaque
/// [`Snapshot`] API is intended for synchronization-library implementations.
pub mod causal {
    use crate::runtime::execution::ExecutionState;

    pub use crate::runtime::causality::ApplicationStateSnapshot;
    /// An opaque snapshot containing vector-clock and application-defined
    /// causal state.
    pub use crate::runtime::causality::CausalSnapshot as Snapshot;

    /// Capture the current task's causal state.
    ///
    /// This advances the current task's vector clock but does not yield to the
    /// scheduler.
    pub fn capture() -> Snapshot {
        ExecutionState::with(|state| state.increment_causality())
    }

    /// Join a snapshot into the current task's causal state.
    ///
    /// This advances the current task's vector clock but does not yield to the
    /// scheduler. Snapshots from older executions are ignored.
    pub fn join(snapshot: &Snapshot) {
        ExecutionState::with(|state| state.update_causality(snapshot));
    }

    /// Capture only the current task's application-defined causal state.
    ///
    /// This operation does not read or advance the current task's vector
    /// clock, and it does not yield to the scheduler. It is intended for
    /// synchronization-library implementations that propagate application
    /// state across an edge that does not propagate Shuttle's vector clocks.
    pub fn capture_application_state() -> ApplicationStateSnapshot {
        if !enabled() {
            return ApplicationStateSnapshot::new();
        }
        ExecutionState::with(|state| {
            let task_id = state.current().id();
            state.get_application_causality(task_id)
        })
    }

    /// Join only application-defined causal state into the current task.
    ///
    /// This operation does not read, advance, or join vector clocks, and it
    /// does not yield to the scheduler. Snapshots from older executions are
    /// ignored. It is intended for synchronization-library implementations.
    pub fn join_application_state(snapshot: &ApplicationStateSnapshot) {
        if !enabled() {
            return;
        }
        ExecutionState::with(|state| {
            let task_id = state.current().id();
            state.join_application_causality_for_task(task_id, snapshot);
        });
    }

    /// Join a snapshot without advancing the current task's vector clock.
    #[doc(hidden)]
    pub fn join_without_increment(snapshot: &Snapshot) {
        ExecutionState::with(|state| {
            let task_id = state.current().id();
            state.join_causality_for_task(task_id, snapshot);
        });
    }

    /// Get a clone of the application-defined causal state for the current task.
    pub fn state<T>() -> T
    where
        T: Clone + Send + Sync + 'static,
    {
        ExecutionState::with(|execution| {
            let task_id = execution.current().id();
            execution.causal_state(task_id)
        })
    }

    /// Join a value into the current task's application-defined causal state.
    ///
    /// The configured join operation must only add information.
    pub fn join_state<T>(incoming: T)
    where
        T: Clone + Send + Sync + 'static,
    {
        ExecutionState::with(|execution| {
            let task_id = execution.current().id();
            execution.join_causal_state(task_id, incoming);
        });
    }

    /// Snapshot the current task without advancing its vector clock.
    #[doc(hidden)]
    pub fn snapshot() -> Snapshot {
        ExecutionState::with(|state| {
            let task_id = state.current().id();
            state.get_causality(task_id)
        })
    }

    /// Try to capture a snapshot without advancing the vector clock.
    #[doc(hidden)]
    pub fn try_snapshot() -> Option<Snapshot> {
        ExecutionState::try_with(|state| {
            let task_id = state.try_current()?.id();
            Some(state.get_causality(task_id))
        })
        .ok()
        .flatten()
    }

    /// Try to capture the current task and advance its vector clock.
    #[doc(hidden)]
    pub fn try_capture() -> Option<Snapshot> {
        ExecutionState::try_with(|state| {
            state.try_current()?;
            Some(state.increment_causality())
        })
        .ok()
        .flatten()
    }

    /// Try to capture only the current task's application-defined causal
    /// state.
    #[doc(hidden)]
    pub fn try_capture_application_state() -> Option<ApplicationStateSnapshot> {
        if !enabled() {
            return None;
        }
        ExecutionState::try_with(|state| {
            let task_id = state.try_current()?.id();
            Some(state.get_application_causality(task_id))
        })
        .ok()
        .flatten()
    }

    /// Whether application-defined causal state is globally enabled.
    #[doc(hidden)]
    #[inline]
    pub fn enabled() -> bool {
        crate::runtime::causality::causal_state_enabled()
    }
}

/// Apply the given function to the Labels for the specified task
pub fn with_labels_for_task<F, T>(task_id: TaskId, f: F) -> T
where
    F: FnOnce(&mut Labels) -> T,
{
    LABELS.with(|cell| {
        let mut map = cell.borrow_mut();
        let m = map.entry(task_id).or_default();
        f(m)
    })
}

/// Get a label of the given type for the specified task, if any
pub fn get_label_for_task<T: Clone + Debug + 'static>(task_id: TaskId) -> Option<T> {
    with_labels_for_task(task_id, |labels| labels.get().cloned())
}

/// Add the given label to the specified task, returning the old label for the type, if any
pub fn set_label_for_task<T: Clone + Debug + 'static>(task_id: TaskId, value: T) -> Option<T> {
    with_labels_for_task(task_id, |labels| labels.insert(value))
}

/// Remove a label of the given type for the specified task, returning the old label for the type, if any
pub fn remove_label_for_task<T: Clone + Debug + 'static>(task_id: TaskId) -> Option<T> {
    with_labels_for_task(task_id, |labels| labels.remove())
}

/// Get the debug name for a task
pub fn get_name_for_task(task_id: TaskId) -> Option<TaskName> {
    get_label_for_task::<TaskName>(task_id)
}

/// Set the debug name for a task, returning the old name, if any
pub fn set_name_for_task(task_id: TaskId, task_name: impl Into<TaskName>) -> Option<TaskName> {
    let task_name = task_name.into();
    crate::annotations::record_name_for_task(task_id, &task_name);
    // Do note that `record` simply appends the new name as a field, meaning the step span will end up looking something like this:
    // step{task="main-thread(2)" task="Child"}
    // when running with something like the `tracing_subscriber::fmt` subscriber.
    // This either has to be lived with, or the task name should be set via the `ChildLabelFn` mechanism, or a different subscriber should be used
    // (if this is done, then `record_steps_in_span` should be set to true as well), or Shuttle will have to be chanegd to recreate the Span
    let res = ExecutionState::try_with(|state| {
        state
            .get_mut(task_id)
            .step_span
            .record("task", format!("{task_name:?}"));
    });
    if let Err(e) = res {
        tracing::error!("`set_name_for_task` failed with error: {e:?}");
    }
    set_label_for_task::<TaskName>(task_id, task_name)
}

/// Gets the `TaskId` of the current task, or `None` if there is no current task.
pub fn get_current_task() -> Option<TaskId> {
    ExecutionState::with(|s| Some(s.try_current()?.id()))
}

/// Get the `TaskId` of the current task.  Panics if there is no current task.
pub fn me() -> TaskId {
    get_current_task().unwrap()
}

/// Sets the number of scheduling steps used (wrt. the step bound) to 0.
///
/// The idea behind this is to run the test with some step bound, and then call this function whenever it is known that progress has been made.
/// This allows tests to run with tighter step bounds, and to scale a test up without also changing the step bound.
///
/// NOTE: Be careful when using this, as if used wrongly it can be used to make a test execute forever.
pub fn reset_step_count() {
    ExecutionState::with(|s| s.steps_reset_at = CurrentSchedule::len());
}

/// Sets the `tag` field of the current task.
/// Returns the `tag` which was there previously.
#[deprecated]
#[allow(deprecated)]
pub fn set_tag_for_current_task(tag: Arc<dyn Tag>) -> Option<Arc<dyn Tag>> {
    ExecutionState::set_tag_for_current_task(tag)
}

/// Gets the `tag` field of the current task.
#[deprecated]
#[allow(deprecated)]
pub fn get_tag_for_current_task() -> Option<Arc<dyn Tag>> {
    ExecutionState::get_tag_for_current_task()
}

/// Gets the `tag` field of the specified task.
#[deprecated]
#[allow(deprecated)]
pub fn get_tag_for_task(task_id: TaskId) -> Option<Arc<dyn Tag>> {
    TASK_ID_TO_TAGS.with(|cell| {
        let map = cell.borrow();
        map.get(&task_id).cloned()
    })
}

/// Sets the `tag` field of the specified task.
#[deprecated]
#[allow(deprecated)]
pub fn set_tag_for_task(task: TaskId, tag: Arc<dyn Tag>) -> Option<Arc<dyn Tag>> {
    ExecutionState::set_tag_for_task(task, tag)
}
