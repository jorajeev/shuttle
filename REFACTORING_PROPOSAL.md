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
- `ExecutionTrace`, `TraceEvent` (for post-execution DPOR — see below)
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
    /// Receives both runtime metrics and the full execution trace.
    /// DPOR schedulers use the trace to compute backtracking sets;
    /// metrics schedulers use only the metrics and ignore the trace.
    fn on_execution_complete(&mut self, _metrics: &ExecutionMetrics, _trace: &ExecutionTrace) {}

    /// Incremental task-state hooks. Default no-ops.
    /// Schedulers that maintain their own sorted structures (e.g. a
    /// priority heap) implement these to update incrementally rather
    /// than scanning `runnable_tasks` on every `next_task` call.
    fn on_task_runnable(&mut self, _task: &TaskView) {}
    fn on_task_blocked(&mut self, _task_id: TaskId) {}
    fn on_task_finished(&mut self, _task_id: TaskId) {}
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

| Feature | Mechanism |
|---|---|
| Metrics | `on_execution_complete` + `ExecutionMetrics` |
| Constraint hints | `TaskView::labels` in `ExecutionContext`; incremental hooks for priority heaps |
| DPOR / execution graphs | `DependencyGraph` in `ExecutionContext` (online variants); `ExecutionTrace` in `on_execution_complete` (offline variants) |
| Causal debugging | `TaskView::clock` when feature is on; `ExecutionTrace` always |
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
- `on_execution_complete(&mut self, metrics: &ExecutionMetrics, trace: &ExecutionTrace)`
  is called on the scheduler at the end of each execution
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

**The shape of the `Scheduler` trait does not need to change for DPOR.** DPOR is a
smarter implementation of the existing interface: `new_execution()` returns the next
backtracking schedule, `next_task()` replays the prefix then diverges, and
`new_execution() -> None` signals exhaustion. The additions are purely in what data
the scheduler can observe.

#### Execution graph model (from Must, OOPSLA 2024)

Must builds an execution graph incrementally during forward exploration. In Shuttle's
setting the graph has:

- **Nodes** — events (scheduling points / sync operations)
- **`po` edges** — program order within a task (always present)
- **`rf` edges** — "reads-from" / communication: which earlier event a receive/acquire
  reads from
- **`hb`** — transitive closure of `po ∪ rf`; the happens-before relation

Two events are **dependent** iff they access the same resource with conflicting access
types (e.g., two lock acquisitions on the same mutex, a send and a receive on the same
channel). The DPOR algorithm finds pairs `(e1, e2)` that are dependent but unordered
by `hb` and queues alternative schedules that explore the other ordering.

Shuttle's dependency relation is simpler than the general memory-model case because
all concurrency goes through explicit synchronization primitives — there are no
uncontrolled memory reads/writes to track. Every event already carries a
`ResourceSignature`, so dependency detection reduces to: same signature, conflicting
`AccessType`.

#### Two mechanisms for different DPOR variants

**Online variants** (Source-DPOR, Optimal-DPOR with wakeup trees) compute backtracking
sets *during* forward execution. At each `next_task` call, the scheduler inspects the
`DependencyGraph` in `ExecutionContext` to ask: "for each candidate task, are there
past events dependent on it that are not in its `hb` cone?" If so, a backtracking
point is recorded immediately. These variants need no post-execution hook beyond what
`ExecutionContext` already provides.

**Offline variants** (Must-style) complete a full execution first and then compute the
revisit set from the entire execution graph. These use the `ExecutionTrace` passed to
`on_execution_complete`:

```rust
pub struct ExecutionTrace {
    pub events: Vec<TraceEvent>,
}

pub struct TraceEvent {
    pub step: usize,
    pub task: TaskId,
    pub resource: ResourceSignature,
    pub access: AccessType,
    /// Indices into `events` giving the happens-before frontier at this point.
    pub hb_predecessors: Vec<usize>,
}
```

The DPOR scheduler reads the completed trace, identifies dependent unordered pairs,
generates modified `Schedule`s that replay up to each backtracking point and then
diverge, and queues them to be returned by future `new_execution()` calls.

#### Lazy dependency tracking

Must includes an optimization: track ordering among events lazily, only recording an
`hb` edge between two events when they are actually dependent. Many operations commute
(two tasks sending on different channels have no dependency), so no edge is needed.
In Shuttle this falls out naturally from `ResourceSignature` — the engine only creates
a `hb` edge between events that share a resource signature. Events on different
resources never generate edges, keeping the graph sparse.

#### What the engine provides, what the scheduler owns

The engine tracks what happened (`AccessRecord`s → `DependencyGraph` → `ExecutionTrace`).
The scheduler decides what to explore next (backtracking sets → `Schedule` queue).
The `DporScheduler` implementation lives entirely in `shuttle-schedulers` — no DPOR
logic enters the engine.

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
   `AccessRecord`, `ExecutionMetrics`, `ExecutionTrace`, `TraceEvent` — in the current
   `shuttle` crate. No behavior change, just types.

2. **Migrate `Scheduler::next_task`** to take `&ExecutionContext`. Update all
   built-in schedulers. Add `LegacyAdapter`. This is the riskiest step and benefits
   from being done first while there is only one crate to update.

3. **Wire up `AccessRecord` collection** in the execution engine. Populate
   `DependencyGraph` and `ExecutionTrace` per execution. No scheduler behavior changes
   yet.

4. **Add `on_execution_complete` and incremental hooks** (`on_task_runnable`,
   `on_task_blocked`, `on_task_finished`). Migrate `MetricsScheduler` to use
   `on_execution_complete`.

5. **Factor `ExecutionState`** into a clean internal API: define what surface area
   `shuttle-sync` needs to call, make that surface explicit and stable.

6. **Extract `shuttle-core`** as a separate crate. Move `runtime/`, `scheduler/mod.rs`
   (trait + types only), `current.rs`, `Config`.

7. **Extract `shuttle-schedulers`** — move concrete scheduler implementations.

8. **Extract `shuttle-sync`** — move `sync/` and `future/`.

9. **Thin `shuttle`** down to re-exports only.

Each step produces a working, tested build. Steps 6–9 are mechanical once step 5 is
done cleanly.
