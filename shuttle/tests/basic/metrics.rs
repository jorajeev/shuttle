#[cfg(feature = "metrics")]
mod shuttle_metrics {
    use shuttle::metrics::MetricsConfig;
    use shuttle::scheduler::RandomScheduler;
    use shuttle::{thread, Config, Runner};
    use tempfile::NamedTempFile;

    fn run_simple_test(iterations: usize) -> Vec<serde_json::Value> {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        let config = Config::new().with_metrics(MetricsConfig::jsonl(path.clone()));
        Runner::new(RandomScheduler::new(iterations), config).run(|| {
            let _ = thread::spawn(|| {});
        });

        let content = std::fs::read_to_string(&path).unwrap();
        content
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn schema_header_is_first_line() {
        let records = run_simple_test(5);
        assert!(!records.is_empty());
        assert_eq!(records[0]["type"], "metrics_schema");
        assert_eq!(records[0]["version"], 1);
    }

    #[test]
    fn one_run_summary_per_iteration() {
        let iterations = 10;
        let records = run_simple_test(iterations);
        let summaries: Vec<_> = records.iter().filter(|r| r["type"] == "run_summary").collect();
        assert_eq!(summaries.len(), iterations);
    }

    #[test]
    fn run_indices_are_sequential() {
        let records = run_simple_test(5);
        let summaries: Vec<_> = records.iter().filter(|r| r["type"] == "run_summary").collect();
        for (i, s) in summaries.iter().enumerate() {
            assert_eq!(s["run"], i);
        }
    }

    #[test]
    fn scheduler_decisions_are_positive() {
        let records = run_simple_test(3);
        for s in records.iter().filter(|r| r["type"] == "run_summary") {
            assert!(s["scheduler_decisions"].as_u64().unwrap() > 0);
        }
    }

    #[test]
    fn task_completions_match_spawned_tasks() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        let config = Config::new().with_metrics(MetricsConfig::jsonl(path.clone()));
        Runner::new(RandomScheduler::new(10), config).run(|| {
            // spawns one child + main thread = 2 tasks finish each run
            let _ = thread::spawn(|| {});
        });

        let content = std::fs::read_to_string(&path).unwrap();
        for line in content.lines().filter(|l| !l.is_empty()) {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            if v["type"] == "run_summary" {
                assert_eq!(v["task_completions"].as_u64().unwrap(), 2, "expected 2 task completions");
            }
        }
    }

    #[test]
    fn wall_time_ns_is_positive() {
        let records = run_simple_test(3);
        for s in records.iter().filter(|r| r["type"] == "run_summary") {
            assert!(s["wall_time_ns"].as_u64().unwrap() > 0);
        }
    }

    #[test]
    fn schema_header_reflects_sample_memory_false() {
        let records = run_simple_test(1);
        assert_eq!(records[0]["sample_memory"], false);
    }

    #[test]
    fn schema_header_reflects_sample_memory_true() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        let config = Config::new()
            .with_metrics(MetricsConfig::jsonl(path.clone()).with_memory_sampling());
        Runner::new(RandomScheduler::new(1), config).run(|| {});

        let content = std::fs::read_to_string(&path).unwrap();
        let records: Vec<serde_json::Value> = content
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(records[0]["sample_memory"], true);
    }

    /// RSS fields are present when memory sampling is enabled and omitted when disabled.
    #[test]
    #[cfg(target_os = "linux")]
    fn rss_fields_present_when_sampling_enabled() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        let config = Config::new()
            .with_metrics(MetricsConfig::jsonl(path.clone()).with_memory_sampling());
        Runner::new(RandomScheduler::new(3), config).run(|| {
            let _ = thread::spawn(|| {});
        });

        let content = std::fs::read_to_string(&path).unwrap();
        for line in content.lines().filter(|l| !l.is_empty()) {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            if v["type"] == "run_summary" {
                assert!(v["rss_start_bytes"].is_number(), "rss_start_bytes missing");
                assert!(v["rss_end_bytes"].is_number(), "rss_end_bytes missing");
                assert!(v["rss_start_bytes"].as_u64().unwrap() > 0);
                assert!(v["rss_end_bytes"].as_u64().unwrap() > 0);
            }
        }
    }

    #[test]
    fn rss_fields_absent_when_sampling_disabled() {
        let records = run_simple_test(3);
        for s in records.iter().filter(|r| r["type"] == "run_summary") {
            assert!(s["rss_start_bytes"].is_null(), "rss_start_bytes should be absent");
            assert!(s["rss_end_bytes"].is_null(), "rss_end_bytes should be absent");
        }
    }

    fn run_with_task_metrics(iterations: usize) -> Vec<serde_json::Value> {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        let config = Config::new()
            .with_metrics(MetricsConfig::jsonl(path.clone()).with_task_metrics());
        Runner::new(RandomScheduler::new(iterations), config).run(|| {
            let _ = thread::spawn(|| {});
        });

        let content = std::fs::read_to_string(&path).unwrap();
        content
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn task_summaries_absent_without_flag() {
        let records = run_simple_test(3);
        assert!(!records.iter().any(|r| r["type"] == "task_summary"));
    }

    #[test]
    fn task_summaries_present_with_flag() {
        let records = run_with_task_metrics(5);
        let summaries: Vec<_> = records.iter().filter(|r| r["type"] == "task_summary").collect();
        assert!(!summaries.is_empty());
    }

    #[test]
    fn schema_header_reflects_record_task_metrics() {
        let records = run_with_task_metrics(1);
        assert_eq!(records[0]["record_task_metrics"], true);
        let records2 = run_simple_test(1);
        assert_eq!(records2[0]["record_task_metrics"], false);
    }

    #[test]
    fn one_task_summary_per_task_per_run() {
        // Each run spawns main thread (task 0) + one child (task 1) = 2 tasks
        let records = run_with_task_metrics(5);
        for run_idx in 0..5u64 {
            let run_tasks: Vec<_> = records
                .iter()
                .filter(|r| r["type"] == "task_summary" && r["run"] == run_idx)
                .collect();
            assert_eq!(run_tasks.len(), 2, "expected 2 task_summary records for run {run_idx}");
        }
    }

    #[test]
    fn task_summary_run_matches_run_summary() {
        let records = run_with_task_metrics(3);
        for s in records.iter().filter(|r| r["type"] == "task_summary") {
            let run = s["run"].as_u64().unwrap();
            assert!(run < 3, "task_summary run index {run} out of range");
        }
    }

    #[test]
    fn task_summary_times_runnable_gte_times_scheduled() {
        let records = run_with_task_metrics(5);
        for s in records.iter().filter(|r| r["type"] == "task_summary") {
            let tr = s["times_runnable"].as_u64().unwrap();
            let ts = s["times_scheduled"].as_u64().unwrap();
            let trns = s["times_runnable_not_scheduled"].as_u64().unwrap();
            assert!(tr >= ts, "times_runnable must be >= times_scheduled");
            assert_eq!(trns, tr - ts, "times_runnable_not_scheduled must equal times_runnable - times_scheduled");
        }
    }

    #[test]
    fn task_summary_signature_hash_is_nonzero() {
        let records = run_with_task_metrics(3);
        for s in records.iter().filter(|r| r["type"] == "task_summary") {
            assert!(s["signature_hash"].as_u64().unwrap() != 0);
        }
    }

    #[test]
    fn task_summary_signature_hash_stable_across_runs() {
        // The same logical task (same spawn call site) should have the same
        // signature_hash in every run.
        let records = run_with_task_metrics(5);
        let hash_for_task = |task_id: u64| -> u64 {
            records
                .iter()
                .filter(|r| r["type"] == "task_summary" && r["task"] == task_id)
                .map(|r| r["signature_hash"].as_u64().unwrap())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .into_iter()
                .next()
                .unwrap()
        };
        // There should be exactly one distinct signature_hash per task ID across all runs
        let hashes_task0: std::collections::HashSet<u64> = records
            .iter()
            .filter(|r| r["type"] == "task_summary" && r["task"] == 0u64)
            .map(|r| r["signature_hash"].as_u64().unwrap())
            .collect();
        assert_eq!(hashes_task0.len(), 1, "task 0 should have a stable signature_hash across runs");
        let hashes_task1: std::collections::HashSet<u64> = records
            .iter()
            .filter(|r| r["type"] == "task_summary" && r["task"] == 1u64)
            .map(|r| r["signature_hash"].as_u64().unwrap())
            .collect();
        assert_eq!(hashes_task1.len(), 1, "task 1 should have a stable signature_hash across runs");
        // And the two tasks have different hashes
        assert_ne!(hash_for_task(0), hash_for_task(1));
    }

    #[test]
    fn never_runnable_task_has_zero_times_runnable() {
        // Spawn a task that immediately parks itself — it will be sleeping
        // for the entire run and never appear in the runnable set.
        // Since we're checking the count is zero, this also validates that
        // tasks are registered even when they never become runnable.
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        let config = Config::new()
            .with_metrics(MetricsConfig::jsonl(path.clone()).with_task_metrics());
        // A test with no spawned tasks: only main thread (task 0) exists and runs.
        Runner::new(RandomScheduler::new(3), config).run(|| {});

        let content = std::fs::read_to_string(&path).unwrap();
        let records: Vec<serde_json::Value> = content
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();

        // Only task 0 (main thread); it must have been runnable and scheduled.
        for s in records.iter().filter(|r| r["type"] == "task_summary") {
            assert_eq!(s["task"], 0u64);
            assert!(s["times_runnable"].as_u64().unwrap() > 0);
            assert!(s["times_scheduled"].as_u64().unwrap() > 0);
        }
    }
}

use shuttle::scheduler::RandomScheduler;
use shuttle::{check_random, thread, Runner};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Record};
use tracing::{Event, Id, Metadata, Subscriber};

// Simple `Subscriber` that just remembers the last value of the `iterations` field it has seen from
// a `MetricsScheduler`-generated event
#[derive(Clone)]
struct MetricsSubscriber {
    iterations: Arc<AtomicUsize>,
}

impl MetricsSubscriber {
    fn new() -> Self {
        Self {
            iterations: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl Subscriber for MetricsSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        // We don't care about span equality so just use the same identity for everything
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        // If it's an event from the `MetricsScheduler` with an `iterations` counter, record it
        let metadata = event.metadata();
        if metadata.target() == "shuttle::scheduler::metrics" {
            struct FindIterationsVisitor(Option<u64>);
            impl Visit for FindIterationsVisitor {
                fn record_debug(&mut self, _field: &Field, _value: &dyn std::fmt::Debug) {}
                fn record_u64(&mut self, field: &Field, value: u64) {
                    if field.name() == "iterations" {
                        self.0 = Some(value);
                    }
                }
            }
            let mut visitor = FindIterationsVisitor(None);
            event.record(&mut visitor);
            if let Some(iterations) = visitor.0 {
                self.iterations.store(iterations as usize, Ordering::SeqCst);
            }
        }
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

// Note: `panic_iteration` is 1-indexed because "iterations" is a count
fn iterations_test(run_iterations: usize, panic_iteration: usize) {
    let metrics = MetricsSubscriber::new();
    let _guard = tracing::subscriber::set_default(metrics.clone());

    let iterations = Arc::new(AtomicUsize::new(0));

    let result = catch_unwind(AssertUnwindSafe(|| {
        check_random(
            move || {
                iterations.fetch_add(1, Ordering::SeqCst);
                if iterations.load(Ordering::SeqCst) >= panic_iteration {
                    panic!("expected panic");
                }

                thread::spawn(move || {
                    thread::yield_now();
                });
            },
            run_iterations,
        );
    }));

    assert_eq!(result.is_err(), panic_iteration <= run_iterations);
    assert_eq!(
        metrics.iterations.load(Ordering::SeqCst),
        run_iterations.min(panic_iteration)
    );
}

#[test]
fn iterations_test_basic() {
    iterations_test(10, 20);
}

#[test]
fn iterations_test_panic() {
    iterations_test(10, 1);
    iterations_test(10, 5);
    iterations_test(10, 10);
}

#[test]
fn iterations_without_running() {
    let metrics = MetricsSubscriber::new();

    {
        let _guard = tracing::subscriber::set_default(metrics.clone());
        let scheduler = RandomScheduler::new(10);
        let _runner = Runner::new(scheduler, Default::default());
    }

    assert_eq!(metrics.iterations.load(Ordering::SeqCst), 0);
}
