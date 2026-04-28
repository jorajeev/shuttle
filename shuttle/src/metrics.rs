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
//! let config = Config { metrics: Some(MetricsConfig::jsonl("shuttle-metrics.jsonl")), ..Config::new() };
//! Runner::new(RandomScheduler::new(100), config).run(|| {
//!     // test body
//! });
//! # }
//! ```
//!
//! Each run produces one JSON Lines record in the output file:
//!
//! ```json
//! {"type":"metrics_schema","version":1}
//! {"type":"run_summary","run":0,"seed":12345,"wall_time_ns":1234567,...}
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
}

impl MetricsConfig {
    /// Write metrics as JSON Lines to the given file path.
    ///
    /// The file is created (or truncated) when [`Runner::run`](crate::Runner::run) starts.
    pub fn jsonl(path: impl Into<PathBuf>) -> Self {
        Self { output: path.into() }
    }
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
}

thread_local! {
    pub(crate) static CURRENT_RUN_METRICS: RefCell<RunMetrics> = RefCell::new(RunMetrics::default());
}

impl RunMetrics {
    /// Reset counters to zero at the start of a new execution.
    pub(crate) fn reset() {
        CURRENT_RUN_METRICS.with(|m| *m.borrow_mut() = RunMetrics::default());
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
}

/// Writes per-run metric summaries as JSON Lines to a file.
#[derive(Debug)]
pub(crate) struct MetricsWriter {
    writer: BufWriter<File>,
    run_index: u64,
}

impl MetricsWriter {
    /// Open the output file and write the schema header line.
    pub(crate) fn new(config: &MetricsConfig) -> std::io::Result<Self> {
        let file = File::create(&config.output)?;
        let mut writer = BufWriter::new(file);
        writeln!(writer, r#"{{"type":"metrics_schema","version":1}}"#)?;
        writer.flush()?;
        Ok(Self { writer, run_index: 0 })
    }

    /// Append one `run_summary` record and flush.
    pub(crate) fn write_run_summary(
        &mut self,
        seed: u64,
        wall_time_ns: u128,
        m: &RunMetrics,
    ) -> std::io::Result<()> {
        writeln!(
            self.writer,
            concat!(
                r#"{{"type":"run_summary","run":{run},"seed":{seed},"wall_time_ns":{wt},"#,
                r#""scheduler_decisions":{sd},"context_switches":{cs},"task_yields":{ty},"#,
                r#""task_blocks":{tb},"task_unblocks":{tu},"task_completions":{tc},"#,
                r#""random_choices":{rc},"max_runnable_tasks":{mrt},"max_live_tasks":{mlt}}}"#,
            ),
            run = self.run_index,
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
        self.writer.flush()?;
        self.run_index += 1;
        Ok(())
    }
}
