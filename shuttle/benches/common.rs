//! Shared measurement utilities for shuttle benchmarks.
//!
//! Include in each benchmark binary with:
//!   `#[path = "common.rs"] mod common;`
//!
//! To enable allocation tracking, declare the global allocator in that binary:
//!   `#[global_allocator] static ALLOCATOR: common::CountingAllocator = common::CountingAllocator;`

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use shuttle::scheduler::{Schedule, Scheduler, Task, TaskId};
pub use shuttle::Runner;

// ── Counting Allocator ────────────────────────────────────────────────────────

/// A `GlobalAlloc` wrapper that counts every heap allocation and the total
/// bytes requested. Declare as the global allocator in binaries that need
/// allocation stats; the struct itself lives here so bench utilities can use it.
pub struct CountingAllocator;

static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        System.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
}

/// Snapshot of allocation counters at a point in time.
#[derive(Clone, Copy, Default)]
pub struct AllocSnapshot {
    count: u64,
    bytes: u64,
}

impl AllocSnapshot {
    pub fn take() -> Self {
        Self {
            count: ALLOC_COUNT.load(Ordering::Relaxed),
            bytes: ALLOC_BYTES.load(Ordering::Relaxed),
        }
    }
}

/// Difference between two `AllocSnapshot`s.
#[derive(Clone, Copy, Default)]
pub struct AllocDelta {
    pub count: u64,
    pub bytes: u64,
}

impl AllocDelta {
    pub fn since(before: AllocSnapshot) -> Self {
        let after = AllocSnapshot::take();
        Self {
            count: after.count.saturating_sub(before.count),
            bytes: after.bytes.saturating_sub(before.bytes),
        }
    }
}

// ── RSS Measurement ───────────────────────────────────────────────────────────

/// Current resident set size in KB, read from `/proc/self/status`.
/// Returns 0 on non-Linux platforms.
#[cfg(target_os = "linux")]
pub fn current_rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(0)
}

#[cfg(not(target_os = "linux"))]
pub fn current_rss_kb() -> u64 {
    0
}

// ── Stats Scheduler ───────────────────────────────────────────────────────────

struct StatsCounters {
    steps: AtomicU64,
    iterations: AtomicU64,
    context_switches: AtomicU64,
    preemptions: AtomicU64,
}

impl Default for StatsCounters {
    fn default() -> Self {
        Self {
            steps: AtomicU64::new(0),
            iterations: AtomicU64::new(0),
            context_switches: AtomicU64::new(0),
            preemptions: AtomicU64::new(0),
        }
    }
}

/// Read-only view of the stats collected by a `StatsScheduler`.
/// Remains valid after the runner (and the scheduler it consumed) are dropped.
pub struct StatsHandle(Arc<StatsCounters>);

impl StatsHandle {
    pub fn steps(&self) -> u64 {
        self.0.steps.load(Ordering::Relaxed)
    }
    pub fn iterations(&self) -> u64 {
        self.0.iterations.load(Ordering::Relaxed)
    }
    pub fn context_switches(&self) -> u64 {
        self.0.context_switches.load(Ordering::Relaxed)
    }
    pub fn preemptions(&self) -> u64 {
        self.0.preemptions.load(Ordering::Relaxed)
    }
}

/// Wraps any `Scheduler`, counting every scheduling decision and iteration
/// into shared atomics that are readable via a `StatsHandle` after the run.
pub struct StatsScheduler<S: Scheduler> {
    inner: S,
    counters: Arc<StatsCounters>,
    last_task: Option<TaskId>,
}

impl<S: Scheduler> StatsScheduler<S> {
    /// Construct a new `StatsScheduler` and its companion `StatsHandle`.
    /// Pass the scheduler to `Runner::new`; keep the handle to read stats
    /// once `runner.run()` returns.
    pub fn new(inner: S) -> (Self, StatsHandle) {
        let counters = Arc::new(StatsCounters::default());
        let handle = StatsHandle(Arc::clone(&counters));
        (Self { inner, counters, last_task: None }, handle)
    }
}

impl<S: Scheduler> Scheduler for StatsScheduler<S> {
    fn new_execution(&mut self) -> Option<Schedule> {
        self.counters.iterations.fetch_add(1, Ordering::Relaxed);
        self.last_task = None;
        self.inner.new_execution()
    }

    fn next_task(
        &mut self,
        runnable: &[&Task],
        current: Option<TaskId>,
        is_yielding: bool,
    ) -> Option<TaskId> {
        let choice = self.inner.next_task(runnable, current, is_yielding)?;
        self.counters.steps.fetch_add(1, Ordering::Relaxed);
        if let Some(last) = self.last_task {
            if last != choice {
                self.counters.context_switches.fetch_add(1, Ordering::Relaxed);
                if runnable.iter().any(|t| t.id() == last) {
                    self.counters.preemptions.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        self.last_task = Some(choice);
        Some(choice)
    }

    fn next_u64(&mut self) -> u64 {
        self.inner.next_u64()
    }
}

// ── Run Stats ─────────────────────────────────────────────────────────────────

/// All metrics collected for one benchmark run.
#[derive(Clone)]
pub struct RunStats {
    pub wall_ms: f64,
    pub alloc_count: u64,
    pub alloc_bytes: u64,
    pub rss_kb: u64,
    pub steps: u64,
    pub iterations: u64,
    pub context_switches: u64,
    pub preemptions: u64,
}

#[allow(dead_code)]
impl RunStats {
    pub fn steps_per_iter(&self) -> f64 {
        if self.iterations == 0 {
            0.0
        } else {
            self.steps as f64 / self.iterations as f64
        }
    }

    pub fn allocs_per_iter(&self) -> f64 {
        if self.iterations == 0 {
            0.0
        } else {
            self.alloc_count as f64 / self.iterations as f64
        }
    }

    /// Inner fields as JSON key-value pairs (no surrounding braces).
    pub fn to_json_fields(&self) -> String {
        format!(
            r#""wall_ms":{:.3},"alloc_count":{},"alloc_bytes":{},"rss_kb":{},"steps":{},"iterations":{},"context_switches":{},"preemptions":{}"#,
            self.wall_ms,
            self.alloc_count,
            self.alloc_bytes,
            self.rss_kb,
            self.steps,
            self.iterations,
            self.context_switches,
            self.preemptions,
        )
    }
}

// ── Measure ───────────────────────────────────────────────────────────────────

/// Run `workload` once as an unmeasured warm-up, then once more while
/// collecting wall time, allocations (via the global `CountingAllocator`),
/// RSS, and scheduler statistics. Returns the stats for the measured run.
///
/// `make_scheduler` is called twice so each run gets a fresh scheduler with
/// reset state.
pub fn measure<S, W>(make_scheduler: impl Fn() -> S, workload: W) -> RunStats
where
    S: Scheduler + 'static,
    W: Fn() + Send + Sync + Clone + 'static,
{
    // Warm-up: populate allocator pools, hot caches, etc.
    Runner::new(make_scheduler(), Default::default()).run(workload.clone());

    // Measured run.
    let (sched, handle) = StatsScheduler::new(make_scheduler());
    let snap = AllocSnapshot::take();
    let t = std::time::Instant::now();
    Runner::new(sched, Default::default()).run(workload);
    let elapsed = t.elapsed();
    let delta = AllocDelta::since(snap);

    RunStats {
        wall_ms: elapsed.as_secs_f64() * 1000.0,
        alloc_count: delta.count,
        alloc_bytes: delta.bytes,
        rss_kb: current_rss_kb(),
        steps: handle.steps(),
        iterations: handle.iterations(),
        context_switches: handle.context_switches(),
        preemptions: handle.preemptions(),
    }
}
