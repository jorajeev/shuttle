pub mod continuation;

pub use continuation::switch;

/// Body of a Shuttle thread, that runs the given closure, handles thread-local destructors, and
/// stores the result of the thread in the given lock.
pub fn thread_fn<F, T>(
    f: F,
    switch_before_exit: bool,
    result: std::sync::Arc<std::sync::Mutex<Option<std::thread::Result<T>>>>,
) where
    F: FnOnce() -> T,
{
    let ret = f();

    if switch_before_exit && crate::runtime::execution::ExecutionState::with(|s| s.exit_current_truncates_execution()) {
        switch();
    }

    tracing::trace!("thread finished, dropping thread locals");

    while let Some(local) = crate::runtime::execution::ExecutionState::with(|state| state.current_mut().pop_local()) {
        tracing::trace!("dropping thread local {:p}", local);
        drop(local);
    }

    tracing::trace!("done dropping thread locals");

    *result.lock().unwrap() = Some(Ok(ret));
    crate::runtime::execution::ExecutionState::with(|state| {
        if let Some(waiter) = state.current_mut().take_waiter() {
            state.get_mut(waiter).unblock();
        }
    });
}
