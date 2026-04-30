//! Compact binary scheduler step log for Shuttle test runs.
//!
//! Enable with the `metrics` feature. Configure via [`SchedulerLogConfig`] on [`crate::Config`].
//!
//! Records the chosen task and the delta of the runnable task set at every scheduling step,
//! compressed with zstd. Enables analyses such as which tasks enable or disable others and
//! which tasks were runnable most often but rarely chosen.
//!
//! # File format (version 1)
//!
//! A single zstd-compressed stream. Within the decompressed stream:
//!
//! ```text
//! FileHeader (10 bytes):
//!   magic:   [u8; 8] = b"SHTLSLOG"
//!   version: u8 = 1
//!   _pad:    u8 = 0
//!
//! (repeated per run)
//! RunStart (tag = 0x01):
//!   run_index: u64 LE
//!   seed:      u64 LE
//!
//! Step (tag = 0x02), one per scheduling decision:
//!   chosen:   u32 LE   -- task ID chosen by the scheduler
//!   n_add:    u8       -- tasks that became runnable since the previous step
//!   n_rem:    u8       -- tasks that left the runnable set since the previous step
//!   add_ids:  [u32 LE; n_add]
//!   rem_ids:  [u32 LE; n_rem]
//!
//! RunEnd (tag = 0x03):
//!   run_index: u64 LE
//!   num_steps: u64 LE
//! ```
//!
//! The runnable set at any step can be reconstructed by replaying deltas from the RunStart
//! (where the set starts empty) forward through each Step record.

use std::cell::RefCell;
use std::fs::File;
use std::io::{self, Write};
use std::path::PathBuf;

/// Configuration for the compact scheduler step log.
#[derive(Debug, Clone)]
pub struct SchedulerLogConfig {
    /// Path to the output file (written as a zstd-compressed binary stream).
    pub output: PathBuf,
}

impl SchedulerLogConfig {
    /// Write the scheduler step log to the given path (zstd-compressed).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { output: path.into() }
    }
}

/// Magic bytes at the start of every scheduler log file.
pub const MAGIC: &[u8; 8] = b"SHTLSLOG";
/// Current binary format version embedded in the file header.
pub const FILE_VERSION: u8 = 1;
const TAG_RUN_START: u8 = 0x01;
const TAG_STEP: u8 = 0x02;
const TAG_RUN_END: u8 = 0x03;

pub(crate) struct SchedulerLogWriter {
    encoder: zstd::Encoder<'static, File>,
    /// Sorted task IDs from the previous step; used to compute per-step deltas.
    prev_runnable: Vec<u32>,
    step_count: u64,
}

impl SchedulerLogWriter {
    pub(crate) fn new(config: &SchedulerLogConfig) -> io::Result<Self> {
        let file = File::create(&config.output)?;
        let mut encoder = zstd::Encoder::new(file, 3)?;
        encoder.write_all(MAGIC)?;
        encoder.write_all(&[FILE_VERSION, 0u8])?;
        Ok(Self {
            encoder,
            prev_runnable: Vec::new(),
            step_count: 0,
        })
    }

    pub(crate) fn write_run_start(&mut self, run_index: u64, seed: u64) -> io::Result<()> {
        self.prev_runnable.clear();
        self.step_count = 0;
        self.encoder.write_all(&[TAG_RUN_START])?;
        self.encoder.write_all(&run_index.to_le_bytes())?;
        self.encoder.write_all(&seed.to_le_bytes())?;
        Ok(())
    }

    /// Record one scheduling step.
    ///
    /// `curr_runnable` must be sorted in ascending task-ID order, which is guaranteed by the
    /// order tasks are iterated in `ExecutionState::schedule`.
    pub(crate) fn write_step(&mut self, chosen: u32, curr_runnable: &[u32]) -> io::Result<()> {
        let (adds, rems) = sorted_diff(&self.prev_runnable, curr_runnable);
        debug_assert!(adds.len() <= 255, "more than 255 tasks added in one step");
        debug_assert!(rems.len() <= 255, "more than 255 tasks removed in one step");

        self.encoder.write_all(&[TAG_STEP])?;
        self.encoder.write_all(&chosen.to_le_bytes())?;
        self.encoder.write_all(&[adds.len() as u8, rems.len() as u8])?;
        for &id in &adds {
            self.encoder.write_all(&id.to_le_bytes())?;
        }
        for &id in &rems {
            self.encoder.write_all(&id.to_le_bytes())?;
        }

        if !adds.is_empty() || !rems.is_empty() {
            self.prev_runnable.clear();
            self.prev_runnable.extend_from_slice(curr_runnable);
        }
        self.step_count += 1;
        Ok(())
    }

    pub(crate) fn write_run_end(&mut self, run_index: u64) -> io::Result<()> {
        self.encoder.write_all(&[TAG_RUN_END])?;
        self.encoder.write_all(&run_index.to_le_bytes())?;
        self.encoder.write_all(&self.step_count.to_le_bytes())?;
        Ok(())
    }

    /// Flush and finalise the zstd stream. Must be called after all runs are complete.
    pub(crate) fn finish(self) -> io::Result<()> {
        self.encoder.finish()?;
        Ok(())
    }
}

/// Compute additions and removals between two sorted u32 slices in O(n+m) time.
/// Returns `(elements in curr not in prev, elements in prev not in curr)`.
fn sorted_diff(prev: &[u32], curr: &[u32]) -> (Vec<u32>, Vec<u32>) {
    let mut adds = Vec::new();
    let mut rems = Vec::new();
    let (mut pi, mut ci) = (0usize, 0usize);
    while pi < prev.len() && ci < curr.len() {
        match prev[pi].cmp(&curr[ci]) {
            std::cmp::Ordering::Equal => {
                pi += 1;
                ci += 1;
            }
            std::cmp::Ordering::Less => {
                rems.push(prev[pi]);
                pi += 1;
            }
            std::cmp::Ordering::Greater => {
                adds.push(curr[ci]);
                ci += 1;
            }
        }
    }
    rems.extend_from_slice(&prev[pi..]);
    adds.extend_from_slice(&curr[ci..]);
    (adds, rems)
}

thread_local! {
    static SCHED_LOG: RefCell<Option<SchedulerLogWriter>> = RefCell::new(None);
}

/// Install the writer into the thread-local. Called by `Runner` before the run loop.
pub(crate) fn install(writer: SchedulerLogWriter) {
    SCHED_LOG.with(|w| *w.borrow_mut() = Some(writer));
}

/// Remove and return the writer from the thread-local. Called by `Runner` after the run loop.
pub(crate) fn uninstall() -> Option<SchedulerLogWriter> {
    SCHED_LOG.with(|w| w.borrow_mut().take())
}

pub(crate) fn run_start(run_index: u64, seed: u64) {
    SCHED_LOG.with(|w| {
        if let Some(ref mut writer) = *w.borrow_mut() {
            if let Err(e) = writer.write_run_start(run_index, seed) {
                eprintln!("shuttle: scheduler log error: {e}");
            }
        }
    });
}

pub(crate) fn write_step(chosen: u32, curr_runnable: &[u32]) {
    SCHED_LOG.with(|w| {
        if let Some(ref mut writer) = *w.borrow_mut() {
            if let Err(e) = writer.write_step(chosen, curr_runnable) {
                eprintln!("shuttle: scheduler log error: {e}");
            }
        }
    });
}

pub(crate) fn run_end(run_index: u64) {
    SCHED_LOG.with(|w| {
        if let Some(ref mut writer) = *w.borrow_mut() {
            if let Err(e) = writer.write_run_end(run_index) {
                eprintln!("shuttle: scheduler log error: {e}");
            }
        }
    });
}
