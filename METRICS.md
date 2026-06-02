# Shuttle Performance Metrics

Feature-gated instrumentation for measuring Shuttle's runtime overhead and comparing scheduling strategies.

## Enabling

Add the `metrics` feature to your dependency or use `--features metrics`:

```toml
[dev-dependencies]
shuttle = { path = ".", features = ["metrics"] }
```

```sh
cargo test --features metrics
```

When the feature is disabled, all instrumentation is compiled out (zero overhead).

## Configuration

Attach a `MetricsConfig` to the `Config` passed to a `Runner`:

```rust
use shuttle::metrics::MetricsConfig;
use shuttle::{Config, Runner};
use shuttle::scheduler::RandomScheduler;

let config = Config::new().with_metrics(
    MetricsConfig::new("/tmp/my-test")  // base path (no extension)
        .with_task_metrics()            // enable per-task aggregates
        .with_step_trace()              // enable per-step trace (implies task metrics)
);

Runner::new(RandomScheduler::new(100), config).run(|| {
    // test body
});
```

This produces up to three output files based on the base path:

| File | When | Contents |
|------|------|----------|
| `<base>.bin` | Always | Binary per-run summary records |
| `<base>.manifest.json` | Always | JSON manifest (schema, signatures, aggregates) |
| `<base>.trace.bin` | `with_step_trace()` | Per-step scheduler trace |

## Output Format

### Summary binary (`.bin`)

Fixed-size header followed by one 96-byte record per iteration.

**Header** (10 bytes):
| Offset | Size | Field |
|--------|------|-------|
| 0 | 4 | Magic: `SHTL` |
| 4 | 2 | Version (little-endian u16, currently 1) |
| 6 | 2 | Flags (bit 0 = task metrics, bit 1 = step trace) |
| 8 | 2 | Field count (little-endian u16, currently 11) |

**Per-run record** (96 bytes, all little-endian u64):
| Index | Field | Description |
|-------|-------|-------------|
| 0 | `seed` | Random seed for this iteration |
| 1 | `wall_time_ns` | Wall-clock time for this iteration |
| 2 | `schedule_len` | Total scheduling steps in this run |
| 3 | `scheduler_decisions` | Times the scheduler was invoked |
| 4 | `context_switches` | Times execution switched between tasks |
| 5 | `task_yields` | Times a task yielded |
| 6 | `task_blocks` | Times a task blocked (mutex, sleep, etc.) |
| 7 | `task_unblocks` | Times a task was unblocked |
| 8 | `task_completions` | Tasks that ran to completion |
| 9 | `random_choices` | Calls to `next_u64` |
| 10 | `max_runnable_tasks` | Peak runnable set size |
| 11 | `max_live_tasks` | Peak number of live tasks |

### Manifest (`.manifest.json`)

JSON written once at the end of all runs. Contains:

```json
{
  "version": 1,
  "total_runs": 100,
  "total_wall_time_ns": 1234567890,
  "record_task_metrics": true,
  "record_step_trace": true,
  "summary_fields": ["seed", "wall_time_ns", ...],
  "summary_record_size": 96,
  "task_signatures": {
    "12345678": {"file": "src/main.rs", "line": 42, "parent_hash": 0}
  },
  "task_aggregates": {
    "12345678": {"total_scheduled": 500, "total_runnable": 2000, "runs_seen": 100}
  },
  "trace_offsets": [6, 1234, 5678]
}
```

- **`task_signatures`**: Maps stable task identity hashes to spawn locations. A task's signature is derived from its spawn call site and parent chain, so the same logical task gets the same hash across runs.
- **`task_aggregates`**: Per-signature totals (only with `with_task_metrics()`).
- **`trace_offsets`**: Byte offset into `.trace.bin` for each run's trace data (only with `with_step_trace()`).

### Trace binary (`.trace.bin`)

**Header** (6 bytes):
| Offset | Size | Field |
|--------|------|-------|
| 0 | 4 | Magic: `SHTR` |
| 4 | 2 | Version (little-endian u16, currently 1) |

**Per-run block** (variable length):
| Field | Size | Description |
|-------|------|-------------|
| `seed` | 8 | u64, matches the summary record |
| `step_count` | 4 | u32, number of scheduling decisions |
| `chosen` | `step_count * 2` | u16 array: task chosen at each step |
| `event_count` | 4 | u32, number of state-change events |
| `events` | `event_count * 7` | Packed events (see below) |

**Event encoding** (7 bytes each):
| Offset | Size | Field |
|--------|------|-------|
| 0 | 4 | Step number (u32) |
| 4 | 1 | Event type: 0=Spawn, 1=Block, 2=Unblock, 3=Finish |
| 5 | 2 | Task ID (u16) |

The trace allows reconstruction of the full runnable set at any point by replaying events forward from the start.

## Reading the Data

### Python example

```python
import struct
import json

# Read summary records
with open("my-test.bin", "rb") as f:
    magic = f.read(4)
    assert magic == b"SHTL"
    version, flags, field_count = struct.unpack("<HHH", f.read(6))
    record_size = (1 + field_count) * 8

    records = []
    while True:
        data = f.read(record_size)
        if len(data) < record_size:
            break
        fields = struct.unpack(f"<{1 + field_count}Q", data)
        records.append(fields)

# Read manifest
with open("my-test.manifest.json") as f:
    manifest = json.load(f)

# Print summary
print(f"Runs: {manifest['total_runs']}")
for i, rec in enumerate(records):
    seed, wall_ns, sched_len = rec[0], rec[1], rec[2]
    print(f"  run {i}: seed={seed} wall={wall_ns/1e6:.1f}ms steps={sched_len}")
```

### Rust example

```rust
use std::fs::File;
use std::io::Read;

let mut f = File::open("my-test.bin").unwrap();
let mut header = [0u8; 10];
f.read_exact(&mut header).unwrap();

assert_eq!(&header[0..4], b"SHTL");
let field_count = u16::from_le_bytes([header[8], header[9]]) as usize;
let record_size = (1 + field_count) * 8;

let mut buf = vec![0u8; record_size];
while f.read_exact(&mut buf).is_ok() {
    let seed = u64::from_le_bytes(buf[0..8].try_into().unwrap());
    let wall_ns = u64::from_le_bytes(buf[8..16].try_into().unwrap());
    let steps = u64::from_le_bytes(buf[16..24].try_into().unwrap());
    println!("seed={seed} wall={wall_ns}ns steps={steps}");
}
```

## Use Cases

1. **Measuring Shuttle tax**: Compare `wall_time_ns` against native execution to quantify instrumentation overhead.
2. **Scheduler comparison**: Run the same test with different schedulers (Random, PCT, DFS) and compare `scheduler_decisions`, `context_switches`, and coverage.
3. **Tuning heuristics**: Use step traces to analyze what the runnable set looks like at each point and how different schedulers choose from it.
4. **Regression detection**: Track metrics across commits to catch performance regressions in the Shuttle runtime itself.

## Versioning

Both binary formats include a version number in the header. The current version is 1. If the format changes in a breaking way, the version will be incremented. The manifest's `summary_fields` array documents the field layout, so readers can be forward-compatible with additional fields.
