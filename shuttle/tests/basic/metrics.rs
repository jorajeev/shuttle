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
        if metadata.target() == "shuttle_core::scheduler::metrics" {
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

#[cfg(feature = "metrics")]
mod shuttle_metrics {
    use shuttle::metrics::MetricsConfig;
    use shuttle::scheduler::RandomScheduler;
    use shuttle::{thread, Config, Runner};
    use std::io::Read;

    fn run_with_metrics(iterations: usize, task_metrics: bool, step_trace: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let base_path = dir.path().join("test-metrics");

        let mut mc = MetricsConfig::new(&base_path);
        if task_metrics {
            mc = mc.with_task_metrics();
        }
        if step_trace {
            mc = mc.with_step_trace();
        }

        let config = Config::new().with_metrics(mc);
        Runner::new(RandomScheduler::new(iterations), config).run(|| {
            thread::spawn(|| {
                thread::yield_now();
            });
        });

        dir
    }

    fn parse_records(dir: &tempfile::TempDir) -> Vec<[u64; 12]> {
        let bin_path = dir.path().join("test-metrics.bin");
        let mut f = std::fs::File::open(bin_path).unwrap();
        let mut header = [0u8; 10];
        f.read_exact(&mut header).unwrap();
        assert_eq!(&header[0..4], b"SHTL");
        let field_count = u16::from_le_bytes([header[8], header[9]]) as usize;
        assert_eq!(field_count, 11);
        let record_size = (1 + field_count) * 8;

        let mut records = Vec::new();
        let mut buf = vec![0u8; record_size];
        while f.read_exact(&mut buf).is_ok() {
            let mut fields = [0u64; 12];
            for (i, field) in fields.iter_mut().enumerate() {
                *field = u64::from_le_bytes(buf[i * 8..(i + 1) * 8].try_into().unwrap());
            }
            records.push(fields);
        }
        records
    }

    fn parse_manifest(dir: &tempfile::TempDir) -> serde_json::Value {
        let manifest_path = dir.path().join("test-metrics.manifest.json");
        let content = std::fs::read_to_string(manifest_path).unwrap();
        serde_json::from_str(&content).unwrap()
    }

    #[test]
    fn binary_header_valid() {
        let dir = run_with_metrics(5, false, false);
        let bin_path = dir.path().join("test-metrics.bin");
        let mut f = std::fs::File::open(bin_path).unwrap();
        let mut header = [0u8; 10];
        f.read_exact(&mut header).unwrap();
        assert_eq!(&header[0..4], b"SHTL");
        assert_eq!(u16::from_le_bytes([header[4], header[5]]), 1); // version
    }

    #[test]
    fn one_record_per_iteration() {
        let dir = run_with_metrics(10, false, false);
        let records = parse_records(&dir);
        assert_eq!(records.len(), 10);
    }

    #[test]
    fn wall_time_is_positive() {
        let dir = run_with_metrics(5, false, false);
        let records = parse_records(&dir);
        for r in &records {
            assert!(r[1] > 0, "wall_time_ns should be positive");
        }
    }

    #[test]
    fn schedule_len_is_positive() {
        let dir = run_with_metrics(5, false, false);
        let records = parse_records(&dir);
        for r in &records {
            assert!(r[2] > 0, "schedule_len should be positive");
        }
    }

    #[test]
    fn scheduler_decisions_are_positive() {
        let dir = run_with_metrics(5, false, false);
        let records = parse_records(&dir);
        for r in &records {
            assert!(r[3] > 0, "scheduler_decisions should be positive");
        }
    }

    #[test]
    fn task_completions_match_spawned_tasks() {
        let dir = run_with_metrics(5, false, false);
        let records = parse_records(&dir);
        for r in &records {
            // We spawn 1 main thread + 1 child thread = 2 completions
            assert_eq!(r[8], 2, "task_completions should be 2 (main + spawned)");
        }
    }

    #[test]
    fn manifest_has_correct_total_runs() {
        let dir = run_with_metrics(7, false, false);
        let manifest = parse_manifest(&dir);
        assert_eq!(manifest["total_runs"], 7);
    }

    #[test]
    fn manifest_has_summary_fields() {
        let dir = run_with_metrics(3, false, false);
        let manifest = parse_manifest(&dir);
        let fields = manifest["summary_fields"].as_array().unwrap();
        assert_eq!(fields.len(), 12);
        assert_eq!(fields[0], "seed");
        assert_eq!(fields[1], "wall_time_ns");
    }

    #[test]
    fn manifest_has_task_signatures() {
        let dir = run_with_metrics(3, true, false);
        let manifest = parse_manifest(&dir);
        let sigs = manifest["task_signatures"].as_object().unwrap();
        assert!(!sigs.is_empty());
    }

    #[test]
    fn manifest_has_task_aggregates_when_enabled() {
        let dir = run_with_metrics(5, true, false);
        let manifest = parse_manifest(&dir);
        let aggs = manifest["task_aggregates"].as_object().unwrap();
        assert!(!aggs.is_empty());
        for (_key, val) in aggs {
            assert!(val["total_scheduled"].as_u64().unwrap() > 0);
            assert!(val["runs_seen"].as_u64().unwrap() > 0);
        }
    }

    #[test]
    fn no_task_aggregates_without_flag() {
        let dir = run_with_metrics(5, false, false);
        let manifest = parse_manifest(&dir);
        assert!(manifest.get("task_aggregates").is_none());
    }

    #[test]
    fn trace_file_written_when_enabled() {
        let dir = run_with_metrics(3, false, true);
        let trace_path = dir.path().join("test-metrics.trace.bin");
        assert!(trace_path.exists());
        let mut f = std::fs::File::open(trace_path).unwrap();
        let mut header = [0u8; 6];
        f.read_exact(&mut header).unwrap();
        assert_eq!(&header[0..4], b"SHTR");
        assert_eq!(u16::from_le_bytes([header[4], header[5]]), 1); // version
    }

    #[test]
    fn no_trace_file_without_flag() {
        let dir = run_with_metrics(3, false, false);
        let trace_path = dir.path().join("test-metrics.trace.bin");
        assert!(!trace_path.exists());
    }

    #[test]
    fn trace_offsets_in_manifest() {
        let dir = run_with_metrics(5, false, true);
        let manifest = parse_manifest(&dir);
        let offsets = manifest["trace_offsets"].as_array().unwrap();
        assert_eq!(offsets.len(), 5);
        // First offset should be right after the 6-byte header
        assert_eq!(offsets[0].as_u64().unwrap(), 6);
    }

    #[test]
    fn trace_data_parseable() {
        let dir = run_with_metrics(2, false, true);
        let trace_path = dir.path().join("test-metrics.trace.bin");
        let mut f = std::fs::File::open(trace_path).unwrap();
        let mut header = [0u8; 6];
        f.read_exact(&mut header).unwrap();

        // Parse first run's trace
        let mut seed_buf = [0u8; 8];
        f.read_exact(&mut seed_buf).unwrap();

        let mut step_count_buf = [0u8; 4];
        f.read_exact(&mut step_count_buf).unwrap();
        let step_count = u32::from_le_bytes(step_count_buf);
        assert!(step_count > 0);

        // Skip chosen array
        let mut chosen = vec![0u8; step_count as usize * 2];
        f.read_exact(&mut chosen).unwrap();

        // Read event count
        let mut event_count_buf = [0u8; 4];
        f.read_exact(&mut event_count_buf).unwrap();
        let event_count = u32::from_le_bytes(event_count_buf);
        assert!(event_count > 0);

        // Verify events are parseable (7 bytes each)
        let mut events = vec![0u8; event_count as usize * 7];
        f.read_exact(&mut events).unwrap();
    }
}
