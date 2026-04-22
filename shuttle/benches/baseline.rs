//! Standalone shuttle benchmark runner.
//!
//! Measures wall time, heap allocations, RSS, and scheduler statistics for a
//! set of representative workloads. Results are printed as a human-readable
//! table (stderr) and a machine-readable JSON object (stdout).
//!
//! Usage:
//!   # Run and display results
//!   cargo bench --bench baseline --features bench-no-vector-clocks
//!
//!   # Save a baseline for later comparison
//!   cargo bench --bench baseline --features bench-no-vector-clocks -- --output baseline.json
//!
//!   # Compare current results against a saved baseline
//!   cargo bench --bench baseline --features bench-no-vector-clocks -- --compare baseline.json
//!
//!   # Save and compare in one pass (updates the baseline file)
//!   cargo bench --bench baseline --features bench-no-vector-clocks -- --output baseline.json --compare baseline.json

#[path = "common.rs"]
mod common;
use common::*;

// Every allocation in this binary goes through the counting allocator so that
// `AllocDelta::since` captures the full cost of each workload.
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

use shuttle::scheduler::{DfsScheduler, PctScheduler, RandomScheduler};
use shuttle::sync::atomic::{AtomicUsize, Ordering};
use shuttle::sync::{Condvar, Mutex};
use shuttle::{future, thread};
use std::sync::Arc;

const SEED: u64 = 0x12345678;

// ── Workload factories ────────────────────────────────────────────────────────
// Each factory returns a `Fn() + Send + Sync + Clone + 'static` closure so
// that `measure` can run it twice (warm-up + measured) without re-constructing
// the factory.

fn lock_workload(num_tasks: u32, events_per_task: u32) -> impl Fn() + Send + Sync + Clone + 'static {
    move || {
        let lock = Arc::new(Mutex::new(0usize));
        let handles: Vec<_> = (0..num_tasks)
            .map(|_| {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    for _ in 0..events_per_task {
                        *lock.lock().unwrap() += 1;
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }
}

fn counter_async_workload(
    num_tasks: u32,
    events_per_task: u32,
) -> impl Fn() + Send + Sync + Clone + 'static {
    move || {
        let counter = Arc::new(AtomicUsize::new(0));
        let tasks: Vec<_> = (0..num_tasks)
            .map(|_| {
                let counter = Arc::clone(&counter);
                future::spawn(async move {
                    for _ in 0..events_per_task {
                        counter.fetch_add(1, Ordering::SeqCst);
                    }
                })
            })
            .collect();
        future::block_on(async move {
            for t in tasks {
                t.await.unwrap();
            }
        });
    }
}

fn create_workload(num_tasks: u32) -> impl Fn() + Send + Sync + Clone + 'static {
    move || {
        let counter = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = (0..num_tasks)
            .map(|_| {
                let counter = Arc::clone(&counter);
                thread::spawn(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }
}

fn buffer_workload() -> impl Fn() + Send + Sync + Clone + 'static {
    const NUM_PRODUCERS: usize = 3;
    const NUM_CONSUMERS: usize = 3;
    const NUM_EVENTS: usize = NUM_PRODUCERS * NUM_CONSUMERS * 3;
    const MAX_QUEUE_SIZE: usize = 3;

    move || {
        let lock = Arc::new(Mutex::new(()));
        let has_space = Arc::new(Condvar::new());
        let has_elements = Arc::new(Condvar::new());
        let count = Arc::new(AtomicUsize::new(0));

        let consumers = (0..NUM_CONSUMERS)
            .map(|_| {
                let (lock, has_space, has_elements, count) = (
                    Arc::clone(&lock),
                    Arc::clone(&has_space),
                    Arc::clone(&has_elements),
                    Arc::clone(&count),
                );
                thread::spawn(move || {
                    for _ in 0..(NUM_EVENTS / NUM_CONSUMERS) {
                        let mut g = lock.lock().unwrap();
                        while count.load(Ordering::SeqCst) == 0 {
                            g = has_elements.wait(g).unwrap();
                        }
                        count.fetch_sub(1, Ordering::SeqCst);
                        has_space.notify_one();
                        drop(g);
                    }
                })
            })
            .collect::<Vec<_>>();

        let producers = (0..NUM_PRODUCERS)
            .map(|_| {
                let (lock, has_space, has_elements, count) = (
                    Arc::clone(&lock),
                    Arc::clone(&has_space),
                    Arc::clone(&has_elements),
                    Arc::clone(&count),
                );
                thread::spawn(move || {
                    for _ in 0..(NUM_EVENTS / NUM_PRODUCERS) {
                        let mut g = lock.lock().unwrap();
                        while count.load(Ordering::SeqCst) == MAX_QUEUE_SIZE {
                            g = has_space.wait(g).unwrap();
                        }
                        count.fetch_add(1, Ordering::SeqCst);
                        has_elements.notify_one();
                        drop(g);
                    }
                })
            })
            .collect::<Vec<_>>();

        for c in consumers {
            c.join().unwrap();
        }
        for p in producers {
            p.join().unwrap();
        }
    }
}

/// Small program whose full DFS state space is exhaustible in reasonable time:
/// two threads each acquiring the same mutex three times.
fn dfs_small_workload() -> impl Fn() + Send + Sync + Clone + 'static {
    move || {
        let lock = Arc::new(Mutex::new(0usize));
        let lock2 = Arc::clone(&lock);
        let t = thread::spawn(move || {
            for _ in 0..3 {
                *lock2.lock().unwrap() += 1;
            }
        });
        for _ in 0..3 {
            *lock.lock().unwrap() += 1;
        }
        t.join().unwrap();
    }
}

// ── Entry ─────────────────────────────────────────────────────────────────────

struct Entry {
    label: String,
    stats: RunStats,
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut output_path: Option<String> = None;
    let mut compare_path: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--output" | "-o" if i + 1 < args.len() => {
                i += 1;
                output_path = Some(args[i].clone());
            }
            "--compare" | "-c" if i + 1 < args.len() => {
                i += 1;
                compare_path = Some(args[i].clone());
            }
            _ => {}
        }
        i += 1;
    }

    print_header();

    let mut entries: Vec<Entry> = Vec::new();

    macro_rules! bench {
        ($label:expr, $make_sched:expr, $workload:expr) => {{
            let label: String = $label.to_string();
            eprint!("{:<55} ...", label);
            let stats = measure($make_sched, $workload);
            eprint!("\r");
            print_row(&label, &stats);
            entries.push(Entry { label, stats });
        }};
    }

    // ── lock (mutex contention, single iteration, long schedules) ─────────────
    bench!("lock/narrow/random", || RandomScheduler::new_from_seed(SEED, 1), lock_workload(5, 2000));
    bench!("lock/wide/random",   || RandomScheduler::new_from_seed(SEED, 1), lock_workload(100, 100));
    bench!("lock/narrow/pct",    || PctScheduler::new_from_seed(SEED, 2, 1), lock_workload(5, 2000));
    bench!("lock/wide/pct",      || PctScheduler::new_from_seed(SEED, 2, 1), lock_workload(100, 100));

    // ── counter async (future scheduling) ────────────────────────────────────
    bench!("counter_async/narrow/random", || RandomScheduler::new_from_seed(SEED, 1), counter_async_workload(5, 2000));
    bench!("counter_async/wide/random",   || RandomScheduler::new_from_seed(SEED, 1), counter_async_workload(100, 100));
    bench!("counter_async/narrow/pct",    || PctScheduler::new_from_seed(SEED, 2, 1), counter_async_workload(5, 2000));
    bench!("counter_async/wide/pct",      || PctScheduler::new_from_seed(SEED, 2, 1), counter_async_workload(100, 100));

    // ── create (task-spawn overhead, many iterations) ─────────────────────────
    bench!("create/narrow/random", || RandomScheduler::new_from_seed(SEED, 100), create_workload(5));
    bench!("create/wide/random",   || RandomScheduler::new_from_seed(SEED, 100), create_workload(100));
    bench!("create/narrow/pct",    || PctScheduler::new_from_seed(SEED, 2, 100), create_workload(5));
    bench!("create/wide/pct",      || PctScheduler::new_from_seed(SEED, 2, 100), create_workload(100));

    // ── buffer (condvar + contention, many iterations) ────────────────────────
    bench!("buffer/pct",    || PctScheduler::new_from_seed(SEED, 2, 1000), buffer_workload());
    bench!("buffer/random", || RandomScheduler::new_from_seed(SEED, 1000), buffer_workload());

    // ── dfs exhaustive (measures scheduler search overhead) ───────────────────
    bench!("dfs/mutex_2t_3ops", || DfsScheduler::new(None, false), dfs_small_workload());

    print_footer();

    // JSON output: always to stdout so it can be redirected/piped
    let json = to_json(&entries);
    if let Some(ref path) = output_path {
        std::fs::write(path, &json).unwrap_or_else(|e| eprintln!("warning: could not write {path}: {e}"));
        eprintln!("baseline saved → {path}");
    }
    if let Some(ref path) = compare_path {
        // If we just wrote to the same path, re-read it so comparison uses the
        // same format (handles the --output X --compare X case for self-check).
        let saved = std::fs::read_to_string(path)
            .unwrap_or_else(|e| { eprintln!("error: could not read {path}: {e}"); std::process::exit(1); });
        compare(&saved, &entries);
    }
    println!("{json}");
}

// ── Table rendering ───────────────────────────────────────────────────────────

const W: usize = 120;

fn print_header() {
    eprintln!("\nshuttle baseline runner");
    eprintln!("{:─<W$}", "");
    eprintln!(
        "{:<45} {:>10} {:>10} {:>12} {:>8} {:>8} {:>10} {:>10}",
        "benchmark", "wall(ms)", "allocs", "bytes", "rss(KB)", "iters", "steps", "ctx_sw"
    );
    eprintln!("{:─<W$}", "");
}

fn print_row(label: &str, s: &RunStats) {
    eprintln!(
        "{:<45} {:>10.1} {:>10} {:>12} {:>8} {:>8} {:>10} {:>10}",
        label, s.wall_ms, s.alloc_count, s.alloc_bytes, s.rss_kb, s.iterations, s.steps, s.context_switches,
    );
}

fn print_footer() {
    eprintln!("{:─<W$}", "");
    eprintln!("rss_kb is the process RSS after all benchmarks have run (cumulative).");
    eprintln!("allocs/bytes are for the measured run only (after one warm-up run).\n");
}

// ── JSON serialisation ────────────────────────────────────────────────────────

fn to_json(entries: &[Entry]) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut out = format!("{{\n  \"timestamp_unix\": {now},\n  \"results\": [\n");
    for (i, e) in entries.iter().enumerate() {
        out.push_str(&format!(
            "    {{\"label\":\"{}\",{}}}",
            e.label,
            e.stats.to_json_fields()
        ));
        if i + 1 < entries.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n}");
    out
}

// ── Baseline comparison ───────────────────────────────────────────────────────

fn compare(baseline_json: &str, current: &[Entry]) {
    eprintln!("\n{:═<W$}", "");
    eprintln!("COMPARISON  (negative delta = improvement)");
    eprintln!("{:═<W$}", "");
    eprintln!(
        "{:<40} {:<22} {:>12} {:>12} {:>10}",
        "benchmark / metric", "", "baseline", "current", "Δ%"
    );
    eprintln!("{:─<W$}", "");

    let mut any_missing = false;
    for entry in current {
        match parse_entry(baseline_json, &entry.label) {
            None => {
                eprintln!("  {} — not found in baseline", entry.label);
                any_missing = true;
            }
            Some(base) => {
                eprintln!("{}:", entry.label);
                cmp_field("  wall_ms",          base.wall_ms,             entry.stats.wall_ms,            true);
                cmp_field("  alloc_count",      base.alloc_count as f64,  entry.stats.alloc_count as f64, true);
                cmp_field("  alloc_bytes",      base.alloc_bytes as f64,  entry.stats.alloc_bytes as f64, true);
                cmp_field("  steps",            base.steps as f64,        entry.stats.steps as f64,       false);
                cmp_field("  context_switches", base.context_switches as f64, entry.stats.context_switches as f64, false);
            }
        }
    }

    eprintln!("{:─<W$}", "");
    if any_missing {
        eprintln!("note: some benchmarks had no baseline entry (new or renamed workloads).");
    }
}

fn cmp_field(name: &str, baseline: f64, current: f64, lower_is_better: bool) {
    let delta_pct = if baseline != 0.0 {
        (current - baseline) / baseline * 100.0
    } else {
        0.0
    };
    let tag = match delta_pct {
        p if p < -2.0 && lower_is_better  => "✓ better",
        p if p >  2.0 && lower_is_better  => "✗ worse ",
        p if p >  2.0 && !lower_is_better => "✓ better",
        p if p < -2.0 && !lower_is_better => "✗ worse ",
        _                                  => "  same  ",
    };
    eprintln!(
        "{:<40} {:<10} {:>12.1} {:>12.1} {:>+9.1}%  {}",
        name, "", baseline, current, delta_pct, tag
    );
}

// ── Minimal JSON extraction ───────────────────────────────────────────────────
// Parses only the specific numeric fields we care about; no full JSON parser.

fn parse_entry(json: &str, label: &str) -> Option<RunStats> {
    // Find the object that contains this label.
    let label_pat = format!("\"label\":\"{}\"", label);
    let label_pos = json.find(&label_pat)?;
    // Walk back to the opening `{` for this object.
    let obj_start = json[..label_pos].rfind('{')?;
    // Walk forward to the closing `}`.
    let rest = &json[obj_start..];
    let obj_end = rest.find('}').unwrap_or(rest.len());
    let chunk = &rest[..=obj_end];

    Some(RunStats {
        wall_ms:          parse_f64(chunk, "wall_ms").unwrap_or(0.0),
        alloc_count:      parse_u64(chunk, "alloc_count").unwrap_or(0),
        alloc_bytes:      parse_u64(chunk, "alloc_bytes").unwrap_or(0),
        rss_kb:           parse_u64(chunk, "rss_kb").unwrap_or(0),
        steps:            parse_u64(chunk, "steps").unwrap_or(0),
        iterations:       parse_u64(chunk, "iterations").unwrap_or(0),
        context_switches: parse_u64(chunk, "context_switches").unwrap_or(0),
        preemptions:      parse_u64(chunk, "preemptions").unwrap_or(0),
    })
}

fn parse_f64(chunk: &str, key: &str) -> Option<f64> {
    let pat = format!("\"{}\":", key);
    let pos = chunk.find(&pat)? + pat.len();
    let rest = chunk[pos..].trim_start_matches([' ', '\t', '\n', '\r']);
    let end = rest
        .find(|c: char| c != '-' && c != '.' && !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn parse_u64(chunk: &str, key: &str) -> Option<u64> {
    let pat = format!("\"{}\":", key);
    let pos = chunk.find(&pat)? + pat.len();
    let rest = chunk[pos..].trim_start_matches([' ', '\t', '\n', '\r']);
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}
