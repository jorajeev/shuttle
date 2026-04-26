# Shuttle Crate Refactoring Proposal

## Background

The current `shuttle` crate bundles the core runtime, schedulers, standard library
primitives, and test utilities into a single package. This proposal describes how to
split it into focused crates in a way that supports five planned enhancements:

1. Detailed metrics on memory and computation overhead from the shuttle runtime
2. Constraint-based scheduler hints — user-provided hints on what tasks to schedule when
3. Execution graphs for optimal dynamic partial order reduction (DPOR)
4. Better debugging via causal information — trace minimization and root cause analysis
5. Better panic handling

---

## Proposed Crate Structure

```
shuttle-core          ← stable public API, minimal dependencies
shuttle-schedulers    ← all scheduler implementations
shuttle-sync          ← std::sync replacements
shuttle               ← thin facade, re-exports for backward compatibility
wrappers/             ← tokio, parking_lot, etc. (unchanged)
```

### `shuttle-core`

The stable foundation everything else builds on. Contains:

- `Scheduler` trait + `ExecutionContext`, `TaskView`, `ExecutionMetrics`
- `Task`, `TaskId`, `TaskState`, `TaskSet`
- `VectorClock` (feature-gated — see below)
- `DependencyGraph` (always-on — see below)
- `ResourceSignature`, `ResourceType` (cross-iteration resource identity)
- `Labels`, `ChildLabelFn`, `TaskName`
- `Schedule`, `ScheduleStep`, schedule serialization
- `BatchSemaphore` (or a replacement primitive — see below)
- `Runner`, `PortfolioRunner`, `Execution` (the internal engine)
- `Config`, `current` module
- `failure` / panic handling types

### `shuttle-schedulers`

Depends only on `shuttle-core`. Contains all concrete scheduler implementations:

- `RandomScheduler`, `PctScheduler`, `UrwRandomScheduler`, `DfsScheduler`
- `RoundRobinScheduler`, `ReplayScheduler`, `AnnotationScheduler`
- `UncontrolledNondeterminismCheckScheduler`
- `MetricsScheduler` wrapper
- Future: `ConstraintScheduler`, DPOR-based scheduler

### `shuttle-sync`

Depends on `shuttle-core`. Contains:

- `Mutex`, `RwLock`, `Condvar`, `Once`, `Barrier`
- `atomic` module
- `mpsc` channels
- `future::BatchSemaphore` (or its replacement)

### `shuttle` (facade)

Thin re-export crate. Preserves the existing public API surface with no breaking changes.

### Wrappers

`shuttle-tokio`, `shuttle-parking-lot`, etc. already depend only on the top-level
`shuttle` crate and require no structural changes.

---

## Key Design Decision: The `Scheduler` Trait

The current signature of `next_task` is the main bottleneck for the planned features:

```rust
// Current
fn next_task(
    &mut self,
    runnable_tasks: &[&Task],
    current_task: Option<TaskId>,
    is_yielding: bool,
) -> Option<TaskId>;
```

Three of the five planned enhancements require this to carry richer information. The
change is to replace the bare arguments with an `ExecutionContext`:

```rust
// Proposed
pub trait Scheduler {
    fn new_execution(&mut self) -> Option<Schedule>;
    fn next_task(&mut self, ctx: &ExecutionContext) -> Option<TaskId>;
    fn next_u64(&mut self) -> u64;

    /// Called at the end of each execution. Default no-op.
    /// Allows schedulers to observe runtime metrics without forcing
    /// all implementations to handle it.
    fn on_execution_complete(&mut self, _metrics: &ExecutionMetrics) {}
}
```

```rust
pub struct ExecutionContext<'a> {
    pub runnable_tasks: &'a [TaskView<'a>],
    pub current_task: Option<TaskId>,
    pub is_yielding: bool,
    pub step: usize,
    /// Always present. Built from resource access records (see below).
    pub dependency_graph: &'a DependencyGraph,
}

pub struct TaskView<'a> {
    pub id: TaskId,
    pub labels: &'a Labels,
    /// Only populated with the `vector-clocks` feature. None otherwise.
    pub clock: Option<&'a VectorClock>,
}
```

This single change unblocks all five features:

| Feature | How `ExecutionContext` enables it |
|---|---|
| Metrics | `on_execution_complete` hook + `ExecutionMetrics` from the runtime |
| Constraint hints | `TaskView::labels` — scheduler reads user-set weights/priorities |
| DPOR / execution graphs | `dependency_graph` always present |
| Causal debugging | `TaskView::clock` when feature is on; dependency graph always |
| Better panic handling | Orthogonal — addressed in `shuttle-core::failure` |

---

## Dependency Tracking vs. Vector Clocks

These are two separate concerns that the current implementation conflates under the
`vector-clocks` feature flag. The proposal separates them:

### Resource dependency tracking (always-on)

At each scheduling point, every sync primitive reports which resource it is accessing
and how. This produces a record:

```rust
pub struct AccessRecord {
    pub step: usize,
    pub task: TaskId,
    pub resource: ResourceSignature,
    pub access: AccessType,  // Read, Write, Acquire, Release, ...
}
```

Two operations are **dependent** iff they access the same resource with conflicting
access types (e.g., two lock acquisitions on the same mutex). The `DependencyGraph`
is built from these records.

Overhead: `O(1)` per scheduling point, `O(steps)` total per execution. Always enabled.
`ResourceSignature` already exists in the codebase; this adds only the `AccessType`
annotation and the graph structure on top.

### Vector clocks (feature-gated, unchanged)

`VectorClock` tracks the full happens-before relation across all tasks. Overhead is
`O(tasks)` per step and `O(steps × tasks)` total — non-trivial. Kept behind the
`vector-clocks` feature flag exactly as today.

Useful for: causal trace minimization, `current::clock()` API, assertions about
happens-before order in tests.

DPOR does **not** require vector clocks. It only requires the dependency graph.

---

## How Each Planned Feature Maps to the Architecture

### 1. Metrics on memory and computation overhead

`MetricsScheduler` currently wraps any `Scheduler` and collects step counts. With the
new design:

- The `Execution` engine populates an `ExecutionMetrics` struct per execution:
  - context switches, task count, steps taken, wall time, stack memory allocated
- `on_execution_complete(&mut self, metrics: &ExecutionMetrics)` is called on the
  scheduler at the end of each execution
- `MetricsScheduler` implements this hook to aggregate across executions
- Users implementing custom schedulers can opt in by overriding the default no-op

No separate crate needed; `ExecutionMetrics` lives in `shuttle-core`.

### 2. Constraint-based scheduler hints

The `Labels` type-map already allows attaching arbitrary typed values to tasks at
spawn time. With `TaskView::labels` visible to the scheduler in `ExecutionContext`,
a constraint-based scheduler can:

```rust
fn next_task(&mut self, ctx: &ExecutionContext) -> Option<TaskId> {
    ctx.runnable_tasks
        .iter()
        .max_by_key(|t| t.labels.get::<Priority>().copied().unwrap_or(0))
        .map(|t| t.id)
}
```

No new runtime machinery required beyond what `ExecutionContext` already exposes.
The `ConstraintScheduler` implementation lives in `shuttle-schedulers`.

### 3. Execution graphs and DPOR

The `DependencyGraph` in `ExecutionContext` gives a DPOR scheduler everything it needs:

- At each step, which tasks' past operations are dependent on the current candidate?
- If task B's next operation is dependent on a recent operation of task A, and there
  exists an alternative execution where B runs before A, the DPOR scheduler must
  explore that alternative.

The classic DPOR backtracking set computation runs entirely within the scheduler using
the dependency graph — no changes to the execution engine are needed beyond populating
`AccessRecord`s.

Optimal DPOR (Source-DPOR, Ideal-DPOR) variants that additionally use vector clocks
can use `TaskView::clock` when the `vector-clocks` feature is enabled.

### 4. Causal debugging and trace minimization

A `TraceMinimizer` can be implemented in `shuttle-schedulers` using:

- The `Schedule` (sequence of task IDs) from a failing execution
- The `DependencyGraph` from that execution (identifies which steps are causally related)
- `VectorClock`s when available (more precise minimization)

The minimizer replays the trace, attempts to remove steps that are not on the causal
path to the failure, and verifies the failure still reproduces. This is analogous to
delta-debugging but guided by causal structure rather than brute-force bisection.

The `failure` module in `shuttle-core` is extended to preserve the dependency graph
alongside the failing schedule.

### 5. Better panic handling

Currently panics inside tasks propagate through `catch_unwind` in the executor with
limited context. Improvements:

- A `TaskPanic` type that preserves: which task panicked, the panic message, the
  `Schedule` at the point of failure, the task's `Labels`, and optionally its
  `VectorClock`
- `FailureContext` wrapping `TaskPanic` plus the full causal chain (which tasks
  contributed to the state that caused the panic)
- The `failure` module surfaces this through the existing `FailurePersistence`
  mechanism, so existing replay workflows continue to work

---

## The `BatchSemaphore` Question

The issue notes considering replacing `BatchSemaphore` with a smaller base primitive.
`BatchSemaphore` currently serves as the universal async/sync bridge underlying
`Mutex`, `RwLock`, `Condvar`, and the async executor. It is 899 lines and has
complex internal logic.

A possible replacement is a `SchedulingPoint` — a minimal primitive that represents
"a set of tasks blocked on a condition, with acquire/release semantics." Properties:

- Simpler to reason about for dependency tracking (cleaner `AccessRecord` semantics)
- Easier to map to DPOR's notion of a "shared resource"
- Reduces the surface area that `shuttle-sync` exposes upward into `shuttle-core`

This is worth doing **alongside** the refactor but is not a prerequisite. The initial
crate split can proceed with `BatchSemaphore` as-is.

---

## What Stays Private

`ExecutionState` is the internal god object — accessed via `scoped_thread_local!`
from every sync primitive, scheduler, and the `current` module. It does **not** become
a public API as part of this refactor. The crate boundary is:

- `shuttle-sync` calls into `shuttle-core` via `current::*` and `thread::switch()`,
  exactly as today
- `shuttle-schedulers` implements the `Scheduler` trait from `shuttle-core` and sees
  only `ExecutionContext` — it never touches `ExecutionState` directly
- `ExecutionState` remains `pub(crate)` inside `shuttle-core`

---

## Backward Compatibility

The `shuttle` facade crate re-exports everything. Existing code using:

```rust
use shuttle::{check_random, sync::Mutex, scheduler::RandomScheduler};
```

continues to compile without changes.

The one breaking change is the `Scheduler` trait signature (`next_task` gains
`&ExecutionContext` instead of the three separate arguments). Users with custom
scheduler implementations need to update. A migration shim can be provided:

```rust
/// Adapter for the old next_task signature. Remove in the next major version.
pub trait LegacyScheduler {
    fn next_task(&mut self, runnable_tasks: &[&Task], current_task: Option<TaskId>, is_yielding: bool) -> Option<TaskId>;
}

impl<S: LegacyScheduler + ...> Scheduler for LegacyAdapter<S> { ... }
```

---

## Risks and Mitigations

| Risk | Mitigation |
|---|---|
| `ExecutionState` entanglement makes crate boundary hard to define | Factor out a clean internal API surface before extracting the crate |
| `switch()` / `scoped_thread_local!` doesn't cross crate boundaries cleanly | Keep `switch()` in `shuttle-core`, re-export it; wrappers call it as today |
| `Scheduler` trait change breaks existing custom schedulers | Provide `LegacyAdapter`; give one major version notice |
| `DependencyGraph` adds overhead even when unused | Benchmark; if significant, gate behind a second feature flag `dependency-tracking` that DPOR opt-in enables |
| `BatchSemaphore` replacement is risky | Defer to after the crate split; treat as a separate project |

---

## Implementation Order

Incremental approach — validate each step before cutting the crate boundary:

1. **Define new types** — `ExecutionContext`, `TaskView`, `DependencyGraph`,
   `AccessRecord`, `ExecutionMetrics` — in the current `shuttle` crate. No behavior
   change, just types.

2. **Migrate `Scheduler::next_task`** to take `&ExecutionContext`. Update all
   built-in schedulers. Add `LegacyAdapter`. This is the riskiest step and benefits
   from being done first while there is only one crate to update.

3. **Wire up `AccessRecord` collection** in the execution engine. Populate
   `DependencyGraph` per execution. No scheduler behavior changes yet.

4. **Add `on_execution_complete` hook** and migrate `MetricsScheduler` to use it.

5. **Factor `ExecutionState`** into a clean internal API: define what surface area
   `shuttle-sync` needs to call, make that surface explicit and stable.

6. **Extract `shuttle-core`** as a separate crate. Move `runtime/`, `scheduler/mod.rs`
   (trait + types only), `current.rs`, `Config`.

7. **Extract `shuttle-schedulers`** — move concrete scheduler implementations.

8. **Extract `shuttle-sync`** — move `sync/` and `future/`.

9. **Thin `shuttle`** down to re-exports only.

Each step produces a working, tested build. Steps 6–9 are mechanical once step 5 is
done cleanly.
