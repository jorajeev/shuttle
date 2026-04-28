//! Per-run metrics collection for Shuttle test runs.
//!
//! Enable with the `metrics` feature flag. Configure via [`MetricsConfig`] on [`crate::Config`].
//!
//! # Example
//!
//! ```no_run
//! # #[cfg(feature = "metrics")]
//! # {
//! use shuttle::metrics::MetricsConfig;
//! use shuttle::{Config, Runner, scheduler::RandomScheduler};
//!
//! let config = Config::new().with_metrics(
//!     MetricsConfig::jsonl("shuttle-metrics.jsonl")
//!         .with_memory_sampling()
//!         .with_task_metrics()
//! );
//! Runner::new(RandomScheduler::new(100), config).run(|| {
//!     // test body
//! });
//! # }
//! ```
//!
//! Output format:
//!
//! ```json
//! {"type":"metrics_schema","version":1,"sample_memory":true,"record_task_metrics":true}
//! {"type":"run_summary","run":0,"seed":12345,"wall_time_ns":1234567,...}
//! {"type":"task_summary","run":0,"task":0,"signature_hash":9876543210,"times_scheduled":41,"times_runnable":183,"times_runnable_not_scheduled":142}
//! {"type":"task_summary","run":0,"task":1,"signature_hash":1234567890,"times_scheduled":12,"times_runnable":12,"times_runnable_not_scheduled":0}
//! ```

use std::cell::RefCell;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

/// Configuration for Shuttle metrics output.
#[derive(Debug, Clone)]
pub struct MetricsConfig {
    /// Path to the JSON Lines output file.
    pub output: PathBuf,
    /// When true, sample process RSS before and after each execution.
    pub sample_memory: bool,
    /// When true, emit one `task_summary` record per task per run with per-task
    /// scheduling counters and a stable `signature_hash` for cross-run correlation.
    pub record_task_metrics: bool,
}

impl MetricsConfig {
    /// Write metrics as JSON Lines to the given file path.
    ///
    /// The file is created (or truncated) when [`Runner::run`](crate::Runner::run) starts.
    pub fn jsonl(path: impl Into<PathBuf>) -> Self {
        Self {
            output: path.into(),
            sample_memory: false,
            record_task_metrics: false,
        }
    }

    /// Enable per-run RSS sampling.
    ///
    /// Adds `rss_start_bytes` and `rss_end_bytes` to every `run_summary` record.
    /// Currently supported on Linux only.
    pub fn with_memory_sampling(mut self) -> Self {
        self.sample_memory = true;
        self
    }

    /// Enable per-task scheduling metrics.
    ///
    /// Emits one `task_summary` record per task per run. Each record includes:
    /// - `task`: the numeric task ID (local to this run, 0-indexed in spawn order)
    /// - `signature_hash`: a stable u64 derived from the task's spawn call site and
    ///   parent chain — equal across runs for the same logical task, allowing
    ///   correlation across different seeds and iterations
    /// - `times_scheduled`: how many times this task was chosen by the scheduler
    /// - `times_runnable`: how many scheduling points this task was in the runnable set
    /// - `times_runnable_not_scheduled`: `times_runnable - times_scheduled`
    ///
    /// From these you can answer:
    /// - Which tasks were scheduled most often (sort by `times_scheduled` desc)
    /// - Which tasks were runnable but unchosen most often (`times_runnable_not_scheduled`)
    /// - Which tasks were almost always runnable (`times_runnable / scheduler_decisions`)
    /// - Which tasks were never runnable (`times_runnable == 0`)
    pub fn with_task_metrics(mut self) -> Self {
        self.record_task_metrics = true;
        self
    }
}

/// Per-task counters collected during one execution.
#[derive(Default, Clone, Debug)]
pub(crate) struct TaskRunMetrics {
    /// Stable cross-run identifier derived from spawn call site and parent chain.
    pub signature_hash: u64,
    /// Number of times this task was chosen by the scheduler.
    pub times_scheduled: u64,
    /// Number of scheduling points at which this task was in the runnable set.
    pub times_in_runnable_set: u64,
}

/// Per-run scheduler and task counters, reset at the start of each execution.
#[derive(Default, Clone, Debug)]
pub(crate) struct RunMetrics {
    /// Number of times the scheduler chose the next task (calls to `next_task`).
    pub scheduler_decisions: u64,
    /// Number of times the scheduled task changed from one task to a different task.
    pub context_switches: u64,
    /// Number of times `is_yielding` was true at a scheduling point.
    pub task_yields: u64,
    /// Number of times a task transitioned to Blocked or Sleeping.
    pub task_blocks: u64,
    /// Number of times a task transitioned from Blocked/Sleeping to Runnable.
    pub task_unblocks: u64,
    /// Number of times a task transitioned to Finished.
    pub task_completions: u64,
    /// Number of calls to `next_u64` (random data requests).
    pub random_choices: u64,
    /// Maximum number of runnable tasks seen at any single scheduling point.
    pub max_runnable_tasks: u64,
    /// Maximum total number of live tasks seen at any single scheduling point.
    pub max_live_tasks: u64,
    /// Per-task counters; `Some` when `record_task_metrics` is enabled, `None` otherwise.
    pub per_task: Option<Vec<TaskRunMetrics>>,
}

thread_local! {
    pub(crate) static CURRENT_RUN_METRICS: RefCell<RunMetrics> = RefCell::new(RunMetrics::default());
}

impl RunMetrics {
    /// Reset counters to zero at the start of a new execution.
    ///
    /// Pass `record_task_metrics: true` to enable per-task tracking for this run.
    pub(crate) fn reset(record_task_metrics: bool) {
        CURRENT_RUN_METRICS.with(|m| {
            *m.borrow_mut() = RunMetrics {
                per_task: if record_task_metrics { Some(Vec::new()) } else { None },
                ..RunMetrics::default()
            };
        });
    }

    /// Apply a mutation to the current run's metrics.
    #[inline]
    pub(crate) fn with_current<F: FnOnce(&mut RunMetrics)>(f: F) {
        CURRENT_RUN_METRICS.with(|m| f(&mut m.borrow_mut()));
    }

    /// Return a snapshot of the current run's metrics.
    pub(crate) fn snapshot() -> RunMetrics {
        CURRENT_RUN_METRICS.with(|m| m.borrow().clone())
    }

    /// Register a newly spawned task. Call once per task in spawn order (task 0, 1, 2, …).
    pub(crate) fn register_task(task_id: usize, signature_hash: u64) {
        CURRENT_RUN_METRICS.with(|m| {
            let mut m = m.borrow_mut();
            if let Some(ref mut per_task) = m.per_task {
                debug_assert_eq!(per_task.len(), task_id, "tasks must be registered in order");
                per_task.push(TaskRunMetrics {
                    signature_hash,
                    ..TaskRunMetrics::default()
                });
            }
        });
    }
}

/// Return the current process RSS in bytes, or `None` if unavailable.
///
/// Reads `/proc/self/status` on Linux. Returns `None` on all other platforms.
pub(crate) fn sample_rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        // VmRSS line looks like: "VmRSS:   12345 kB"
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
                return Some(kb * 1024);
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    None
}

/// Writes per-run metric summaries as JSON Lines to a file.
#[derive(Debug)]
pub(crate) struct MetricsWriter {
    writer: BufWriter<File>,
    sample_memory: bool,
    record_task_metrics: bool,
}

impl MetricsWriter {
    /// Open the output file and write the schema header line.
    pub(crate) fn new(config: &MetricsConfig) -> std::io::Result<Self> {
        let file = File::create(&config.output)?;
        let mut writer = BufWriter::new(file);
        writeln!(
            writer,
            r#"{{"type":"metrics_schema","version":1,"sample_memory":{sm},"record_task_metrics":{rt}}}"#,
            sm = config.sample_memory,
            rt = config.record_task_metrics,
        )?;
        writer.flush()?;
        Ok(Self {
            writer,
            sample_memory: config.sample_memory,
            record_task_metrics: config.record_task_metrics,
        })
    }

    /// Whether this writer is configured to record RSS samples.
    pub(crate) fn sample_memory(&self) -> bool {
        self.sample_memory
    }

    /// Whether this writer is configured to record per-task scheduling metrics.
    pub(crate) fn record_task_metrics(&self) -> bool {
        self.record_task_metrics
    }

    /// Append one `run_summary` record and flush.
    ///
    /// `run` is the 0-based execution index.
    /// `rss` is `Some((start_bytes, end_bytes))` when memory sampling is enabled.
    pub(crate) fn write_run_summary(
        &mut self,
        run: u64,
        seed: u64,
        wall_time_ns: u128,
        m: &RunMetrics,
        rss: Option<(u64, u64)>,
    ) -> std::io::Result<()> {
        write!(
            self.writer,
            concat!(
                r#"{{"type":"run_summary","run":{run},"seed":{seed},"wall_time_ns":{wt},"#,
                r#""scheduler_decisions":{sd},"context_switches":{cs},"task_yields":{ty},"#,
                r#""task_blocks":{tb},"task_unblocks":{tu},"task_completions":{tc},"#,
                r#""random_choices":{rc},"max_runnable_tasks":{mrt},"max_live_tasks":{mlt}"#,
            ),
            run = run,
            seed = seed,
            wt = wall_time_ns,
            sd = m.scheduler_decisions,
            cs = m.context_switches,
            ty = m.task_yields,
            tb = m.task_blocks,
            tu = m.task_unblocks,
            tc = m.task_completions,
            rc = m.random_choices,
            mrt = m.max_runnable_tasks,
            mlt = m.max_live_tasks,
        )?;
        if let Some((start, end)) = rss {
            write!(
                self.writer,
                r#","rss_start_bytes":{start},"rss_end_bytes":{end}"#,
                start = start,
                end = end,
            )?;
        }
        writeln!(self.writer, "}}")?;
        self.writer.flush()?;
        Ok(())
    }

    /// Append one `task_summary` record per task and flush once at the end.
    ///
    /// Records are only written when `record_task_metrics` is enabled.
    pub(crate) fn write_task_summaries(
        &mut self,
        run: u64,
        tasks: &[TaskRunMetrics],
    ) -> std::io::Result<()> {
        for (task_id, m) in tasks.iter().enumerate() {
            writeln!(
                self.writer,
                concat!(
                    r#"{{"type":"task_summary","run":{run},"task":{task},"#,
                    r#""signature_hash":{sig},"times_scheduled":{ts},"#,
                    r#""times_runnable":{tr},"times_runnable_not_scheduled":{trns}}}"#,
                ),
                run = run,
                task = task_id,
                sig = m.signature_hash,
                ts = m.times_scheduled,
                tr = m.times_in_runnable_set,
                trns = m.times_in_runnable_set.saturating_sub(m.times_scheduled),
            )?;
        }
        self.writer.flush()?;
        Ok(())
    }
}
