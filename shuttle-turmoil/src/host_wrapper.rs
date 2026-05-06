use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use shuttle::current::{get_label_for_task, me, set_label_for_task, ChildLabelFn};
use turmoil_net::HostId;

use crate::scheduler::Scheduler;

thread_local! {
    static SCHEDULER: RefCell<Option<Scheduler>> = const { RefCell::new(None) };
}

pub(crate) fn install_scheduler(s: Scheduler) {
    SCHEDULER.with(|cell| *cell.borrow_mut() = Some(s));
}

pub(crate) fn remove_scheduler() {
    SCHEDULER.with(|cell| *cell.borrow_mut() = None);
}

pub(crate) fn current_host_id() -> Option<HostId> {
    get_label_for_task::<HostId>(me())
}

/// Set the `ChildLabelFn` on the current task so that the next
/// spawned child inherits `host_id` as its label.
pub(crate) fn set_child_host_id(host_id: HostId) {
    set_label_for_task(
        me(),
        ChildLabelFn(Arc::new(move |_task_id, labels| {
            labels.insert(host_id);
        })),
    );
}

/// Run one scheduler step for `host`: advance its clock, deliver due
/// packets, collect egress. Returns false if the host's clock is too
/// far ahead (max_skew exceeded) — caller should return Pending.
fn step(host: HostId) -> bool {
    let sched = SCHEDULER.with(|cell| cell.borrow_mut().take());
    if let Some(mut sched) = sched {
        let ok = sched.step(host);
        SCHEDULER.with(|cell| *cell.borrow_mut() = Some(sched));
        ok
    } else {
        false
    }
}

/// Collect egress and deliver due packets without advancing the clock.
fn flush() {
    let sched = SCHEDULER.with(|cell| cell.borrow_mut().take());
    if let Some(mut sched) = sched {
        sched.flush();
        SCHEDULER.with(|cell| *cell.borrow_mut() = Some(sched));
    }
}

/// Future wrapper that calls `turmoil_net::set_current(id)` on every
/// poll, ensuring socket syscalls land in the correct kernel. Steps
/// the packet scheduler before and after each poll so that packets
/// are delivered based on the sim clock.
pub(crate) struct HostWrapper<F> {
    pub id: HostId,
    pub inner: F,
}

impl<F: Future + Unpin> Future for HostWrapper<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if !turmoil_net::try_set_current(self.id) {
            return Poll::Pending;
        }

        if !step(self.id) {
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }

        turmoil_net::set_current(self.id);
        let result = Pin::new(&mut self.inner).poll(cx);

        flush();

        result
    }
}
