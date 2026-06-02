//! Per-run metrics collection for Shuttle test runs.
//!
//! Enable with the `metrics` feature flag. Configure via [`MetricsConfig`] on [`crate::Config`].
//!
//! # Output files
//!
//! Metrics are written to up to three files:
//!
//! - **`<base>.bin`** — Binary per-run summary records, appended after each iteration.
//! - **`<base>.manifest.json`** — JSON manifest written at the end, containing format version,
//!   field schema, task signature table, and per-run offsets into the trace file.
//! - **`<base>.trace.bin`** — (Optional) Per-step scheduler trace using event-based encoding.
//!
//! # Example
//!
//! ```no_run
//! # #[cfg(feature = "metrics")]
//! # {
//! use shuttle_core::metrics::MetricsConfig;
//! use shuttle_core::{Config, Runner};
//! use shuttle_schedulers::RandomScheduler;
//!
//! let config = Config::new().with_metrics(MetricsConfig::new("shuttle-metrics"));
//! Runner::new(RandomScheduler::new(100), config).run(|| {
//!     // test body
//! });
//! # }
//! ```

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

// --- Binary format constants ---

const SUMMARY_MAGIC: &[u8; 4] = b"SHTL";
const SUMMARY_VERSION: u16 = 1;

const TRACE_MAGIC: &[u8; 4] = b"SHTR";
const TRACE_VERSION: u16 = 1;

/// Number of u64 fields in a run summary record (after the seed).
const RUN_SUMMARY_FIELDS: u16 = 11;

// --- Configuration ---

/// Configuration for Shuttle metrics output.
#[derive(Debug, Clone)]
pub struct MetricsConfig {
    /// Base path (without extension) for output files.
    pub base_path: PathBuf,
    /// When true, collect per-task aggregate metrics (written to manifest).
    pub record_task_metrics: bool,
    /// When true, write a per-step trace file.
    pub record_step_trace: bool,
}

impl MetricsConfig {
    /// Create a metrics configuration that writes to `<base_path>.bin` and
    /// `<base_path>.manifest.json`.
    pub fn new(base_path: impl Into<PathBuf>) -> Self {
        Self {
            base_path: base_path.into(),
            record_task_metrics: false,
            record_step_trace: false,
        }
    }

    /// Enable per-task scheduling metrics (written to manifest at end).
    pub fn with_task_metrics(mut self) -> Self {
        self.record_task_metrics = true;
        self
    }

    /// Enable per-step scheduler trace (`<base_path>.trace.bin`).
    ///
    /// This records every scheduling decision and task state transition,
    /// allowing reconstruction of the full runnable set at any point.
    /// Implies `with_task_metrics()`.
    pub fn with_step_trace(mut self) -> Self {
        self.record_step_trace = true;
        self.record_task_metrics = true;
        self
    }

    fn bin_path(&self) -> PathBuf {
        self.base_path.with_extension("bin")
    }

    fn manifest_path(&self) -> PathBuf {
        let mut p = self.base_path.as_os_str().to_owned();
        p.push(".manifest.json");
        PathBuf::from(p)
    }

    fn trace_path(&self) -> PathBuf {
        self.base_path.with_extension("trace.bin")
    }
}

// --- Per-run summary record (binary) ---
// Layout (all little-endian):
//   seed:                u64
//   wall_time_ns:        u64
//   schedule_len:        u64
//   scheduler_decisions: u64
//   context_switches:    u64
//   task_yields:         u64
//   task_blocks:         u64
//   task_unblocks:       u64
//   task_completions:    u64
//   random_choices:      u64
//   max_runnable_tasks:  u64
//   max_live_tasks:      u64
// Total: 12 * 8 = 96 bytes per record (seed + 11 fields)

const RUN_RECORD_SIZE: usize = (1 + RUN_SUMMARY_FIELDS as usize) * 8;

// --- Trace event types ---

/// Events recorded in the step trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum TraceEvent {
    Spawn = 0,
    Block = 1,
    Unblock = 2,
    Finish = 3,
}

// --- In-memory run metrics ---

/// Per-task counters collected during one execution.
#[derive(Default, Clone, Debug)]
pub(crate) struct TaskRunMetrics {
    pub signature_hash: u64,
    pub times_scheduled: u64,
    pub times_in_runnable_set: u64,
}

/// Location info for a task signature (for the manifest).
#[derive(Clone, Debug)]
pub(crate) struct TaskLocationInfo {
    pub signature_hash: u64,
    pub file: &'static str,
    pub line: u32,
    pub parent_hash: u64,
}

/// Per-run counters, reset at the start of each execution.
#[derive(Default, Clone, Debug)]
pub(crate) struct RunMetrics {
    pub scheduler_decisions: u64,
    pub context_switches: u64,
    pub task_yields: u64,
    pub task_blocks: u64,
    pub task_unblocks: u64,
    pub task_completions: u64,
    pub random_choices: u64,
    pub max_runnable_tasks: u64,
    pub max_live_tasks: u64,
    pub scheduler_time_ns: u64,
    /// Per-task counters (only when task metrics are enabled).
    pub per_task: Option<Vec<TaskRunMetrics>>,
    /// Task location info collected during this run (only when task metrics are enabled).
    pub task_locations: Vec<TaskLocationInfo>,
}

/// Per-step trace data collected during one execution.
#[derive(Default, Clone, Debug)]
pub(crate) struct StepTrace {
    /// Task chosen at each scheduling point (index = step number).
    pub chosen: Vec<u16>,
    /// State transition events, sparse.
    pub events: Vec<(u32, TraceEvent, u16)>,
}

thread_local! {
    pub(crate) static CURRENT_RUN_METRICS: RefCell<RunMetrics> = RefCell::new(RunMetrics::default());
    pub(crate) static CURRENT_STEP_TRACE: RefCell<Option<StepTrace>> = const { RefCell::new(None) };
}

impl RunMetrics {
    pub(crate) fn reset(record_task_metrics: bool, record_step_trace: bool) {
        CURRENT_RUN_METRICS.with(|m| {
            *m.borrow_mut() = RunMetrics {
                per_task: if record_task_metrics { Some(Vec::new()) } else { None },
                ..RunMetrics::default()
            };
        });
        CURRENT_STEP_TRACE.with(|t| {
            *t.borrow_mut() = if record_step_trace {
                Some(StepTrace::default())
            } else {
                None
            };
        });
    }

    #[inline]
    pub(crate) fn with_current<F: FnOnce(&mut RunMetrics)>(f: F) {
        CURRENT_RUN_METRICS.with(|m| f(&mut m.borrow_mut()));
    }

    pub(crate) fn snapshot() -> RunMetrics {
        CURRENT_RUN_METRICS.with(|m| m.borrow().clone())
    }

    pub(crate) fn register_task(
        task_id: usize,
        signature_hash: u64,
        file: &'static str,
        line: u32,
        parent_hash: u64,
    ) {
        CURRENT_RUN_METRICS.with(|m| {
            let mut m = m.borrow_mut();
            if let Some(ref mut per_task) = m.per_task {
                debug_assert_eq!(per_task.len(), task_id, "tasks must be registered in order");
                per_task.push(TaskRunMetrics {
                    signature_hash,
                    ..TaskRunMetrics::default()
                });
            }
            m.task_locations.push(TaskLocationInfo {
                signature_hash,
                file,
                line,
                parent_hash,
            });
        });
    }
}

impl StepTrace {
    /// Record which task was chosen at this scheduling point.
    #[inline]
    pub(crate) fn record_choice(task_id: u16) {
        CURRENT_STEP_TRACE.with(|t| {
            if let Some(ref mut trace) = *t.borrow_mut() {
                trace.chosen.push(task_id);
            }
        });
    }

    /// Record a task state transition event.
    #[inline]
    pub(crate) fn record_event(event: TraceEvent, task_id: u16) {
        CURRENT_STEP_TRACE.with(|t| {
            if let Some(ref mut trace) = *t.borrow_mut() {
                let step = trace.chosen.len() as u32;
                trace.events.push((step, event, task_id));
            }
        });
    }

    pub(crate) fn take() -> Option<StepTrace> {
        CURRENT_STEP_TRACE.with(|t| t.borrow_mut().take())
    }
}

// --- Writers ---

/// Manages writing all metrics files.
#[derive(Debug)]
pub(crate) struct MetricsWriter {
    summary_writer: BufWriter<File>,
    trace_writer: Option<BufWriter<File>>,
    config: MetricsConfig,
    /// Task signatures seen across all runs: signature_hash -> (file, line, parent_hash).
    task_signatures: HashMap<u64, TaskSignatureInfo>,
    /// Byte offset into the trace file at the start of each run's trace data.
    trace_offsets: Vec<u64>,
    /// Current byte offset in the trace file.
    trace_pos: u64,
    run_count: u64,
}

#[derive(Debug, Clone)]
struct TaskSignatureInfo {
    file: &'static str,
    line: u32,
    parent_hash: u64,
}

impl MetricsWriter {
    pub(crate) fn new(config: &MetricsConfig) -> std::io::Result<Self> {
        let summary_file = File::create(config.bin_path())?;
        let mut summary_writer = BufWriter::new(summary_file);

        // Write summary file header
        summary_writer.write_all(SUMMARY_MAGIC)?;
        summary_writer.write_all(&SUMMARY_VERSION.to_le_bytes())?;
        let flags: u16 = if config.record_task_metrics { 1 } else { 0 }
            | if config.record_step_trace { 2 } else { 0 };
        summary_writer.write_all(&flags.to_le_bytes())?;
        summary_writer.write_all(&RUN_SUMMARY_FIELDS.to_le_bytes())?;
        summary_writer.flush()?;

        let (trace_writer, trace_pos) = if config.record_step_trace {
            let trace_file = File::create(config.trace_path())?;
            let mut tw = BufWriter::new(trace_file);
            tw.write_all(TRACE_MAGIC)?;
            tw.write_all(&TRACE_VERSION.to_le_bytes())?;
            tw.flush()?;
            (Some(tw), 6) // 4 magic + 2 version = 6 bytes header
        } else {
            (None, 0)
        };

        Ok(Self {
            summary_writer,
            trace_writer,
            config: config.clone(),
            task_signatures: HashMap::new(),
            trace_offsets: Vec::new(),
            trace_pos,
            run_count: 0,
        })
    }

    pub(crate) fn record_task_metrics(&self) -> bool {
        self.config.record_task_metrics
    }

    pub(crate) fn record_step_trace(&self) -> bool {
        self.config.record_step_trace
    }

    /// Write one run summary record to the .bin file.
    pub(crate) fn write_run_summary(
        &mut self,
        seed: u64,
        wall_time_ns: u64,
        schedule_len: u64,
        m: &RunMetrics,
    ) -> std::io::Result<()> {
        let mut buf = [0u8; RUN_RECORD_SIZE];
        let fields: [u64; 12] = [
            seed,
            wall_time_ns,
            schedule_len,
            m.scheduler_decisions,
            m.context_switches,
            m.task_yields,
            m.task_blocks,
            m.task_unblocks,
            m.task_completions,
            m.random_choices,
            m.max_runnable_tasks,
            m.max_live_tasks,
        ];
        for (i, &val) in fields.iter().enumerate() {
            buf[i * 8..(i + 1) * 8].copy_from_slice(&val.to_le_bytes());
        }
        self.summary_writer.write_all(&buf)?;
        self.summary_writer.flush()?;
        self.run_count += 1;
        Ok(())
    }

    /// Collect task signature info for the manifest.
    pub(crate) fn collect_task_signatures(&mut self, tasks: &[TaskRunMetrics], locations: &[TaskLocationInfo]) {
        for loc in locations {
            self.task_signatures.entry(loc.signature_hash).or_insert_with(|| TaskSignatureInfo {
                file: loc.file,
                line: loc.line,
                parent_hash: loc.parent_hash,
            });
        }
        for t in tasks {
            self.task_signatures.entry(t.signature_hash).or_insert_with(|| TaskSignatureInfo {
                file: "<unknown>",
                line: 0,
                parent_hash: 0,
            });
        }
    }

    /// Write a run's trace data to the .trace.bin file.
    ///
    /// Format per run:
    ///   seed:          u64
    ///   step_count:    u32
    ///   chosen:        [u16; step_count]
    ///   event_count:   u32
    ///   events:        [(step: u32, event_type: u8, task_id: u16); event_count]  (7 bytes each)
    pub(crate) fn write_trace(&mut self, seed: u64, trace: &StepTrace) -> std::io::Result<()> {
        let writer = match self.trace_writer.as_mut() {
            Some(w) => w,
            None => return Ok(()),
        };

        self.trace_offsets.push(self.trace_pos);

        // seed
        writer.write_all(&seed.to_le_bytes())?;
        self.trace_pos += 8;

        // step_count + chosen array
        let step_count = trace.chosen.len() as u32;
        writer.write_all(&step_count.to_le_bytes())?;
        self.trace_pos += 4;
        for &task_id in &trace.chosen {
            writer.write_all(&task_id.to_le_bytes())?;
        }
        self.trace_pos += step_count as u64 * 2;

        // event_count + events
        let event_count = trace.events.len() as u32;
        writer.write_all(&event_count.to_le_bytes())?;
        self.trace_pos += 4;
        for &(step, event_type, task_id) in &trace.events {
            writer.write_all(&step.to_le_bytes())?;
            writer.write_all(&[event_type as u8])?;
            writer.write_all(&task_id.to_le_bytes())?;
        }
        self.trace_pos += event_count as u64 * 7;

        writer.flush()?;
        Ok(())
    }

    /// Write the manifest JSON file at the end of all runs.
    pub(crate) fn write_manifest(
        &self,
        total_wall_time_ns: u128,
        task_aggregates: Option<&HashMap<u64, TaskAggregate>>,
    ) -> std::io::Result<()> {
        let manifest_path = self.config.manifest_path();
        let file = File::create(&manifest_path)?;
        let mut w = BufWriter::new(file);

        writeln!(w, "{{")?;
        writeln!(w, "  \"version\": {SUMMARY_VERSION},")?;
        writeln!(w, "  \"total_runs\": {},", self.run_count)?;
        writeln!(w, "  \"total_wall_time_ns\": {total_wall_time_ns},")?;
        writeln!(w, "  \"record_task_metrics\": {},", self.config.record_task_metrics)?;
        writeln!(w, "  \"record_step_trace\": {},", self.config.record_step_trace)?;

        // Schema description for binary file
        writeln!(w, "  \"summary_fields\": [")?;
        let fields = [
            "seed", "wall_time_ns", "schedule_len", "scheduler_decisions",
            "context_switches", "task_yields", "task_blocks", "task_unblocks",
            "task_completions", "random_choices", "max_runnable_tasks", "max_live_tasks",
        ];
        for (i, name) in fields.iter().enumerate() {
            let comma = if i + 1 < fields.len() { "," } else { "" };
            writeln!(w, "    \"{name}\"{comma}")?;
        }
        writeln!(w, "  ],")?;
        writeln!(w, "  \"summary_record_size\": {RUN_RECORD_SIZE},")?;

        // Task signature table
        writeln!(w, "  \"task_signatures\": {{")?;
        let mut sigs: Vec<_> = self.task_signatures.iter().collect();
        sigs.sort_by_key(|(hash, _)| *hash);
        for (i, (hash, info)) in sigs.iter().enumerate() {
            let comma = if i + 1 < sigs.len() { "," } else { "" };
            writeln!(
                w,
                "    \"{hash}\": {{\"file\": \"{}\", \"line\": {}, \"parent_hash\": {}}}{comma}",
                info.file, info.line, info.parent_hash
            )?;
        }
        write!(w, "  }}")?;

        // Task aggregates (if enabled)
        if let Some(aggregates) = task_aggregates {
            writeln!(w, ",")?;
            writeln!(w, "  \"task_aggregates\": {{")?;
            let mut aggs: Vec<_> = aggregates.iter().collect();
            aggs.sort_by_key(|(hash, _)| *hash);
            for (i, (hash, agg)) in aggs.iter().enumerate() {
                let comma = if i + 1 < aggs.len() { "," } else { "" };
                writeln!(
                    w,
                    "    \"{hash}\": {{\"total_scheduled\": {}, \"total_runnable\": {}, \"runs_seen\": {}}}{comma}",
                    agg.total_scheduled, agg.total_runnable, agg.runs_seen
                )?;
            }
            write!(w, "  }}")?;
        }

        // Trace offsets (if enabled)
        if !self.trace_offsets.is_empty() {
            writeln!(w, ",")?;
            write!(w, "  \"trace_offsets\": [")?;
            for (i, offset) in self.trace_offsets.iter().enumerate() {
                if i > 0 {
                    write!(w, ", ")?;
                }
                write!(w, "{offset}")?;
            }
            write!(w, "]")?;
        }

        writeln!(w, "\n}}")?;
        w.flush()?;
        Ok(())
    }
}

/// Aggregated per-task metrics across all runs (for the manifest).
#[derive(Debug, Default, Clone)]
pub(crate) struct TaskAggregate {
    pub total_scheduled: u64,
    pub total_runnable: u64,
    pub runs_seen: u64,
}
