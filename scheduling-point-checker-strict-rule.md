# Revising the scheduling-point checker in PR #397 to a strict rule

## Overview

PR #397 on `awslabs/shuttle` (head branch `jorajeev:scheduling-point-checker`, at commit `a6744fa` when this was written) adds a checker that fails a Shuttle test when a task accesses shared state twice with no scheduling point in between. On Nov 9, dylanjwolff and sarsko pointed out that the rule the checker applies is too weak, and we agree with them. This document describes the rule we want to replace it with, the code changes that implement it, and what to publish on the PR afterwards: one new commit on the PR branch, an updated PR description, and a comment that explains the new approach to the two reviewers.

PR #397 is the first of a series of PRs. The document does not cover the later work in that series, because none of it changes what goes into this commit. That work is listed under "Changes that are out of scope" so that it isn't added by accident.

Note: nothing described here has been built or run. The statements about which tests change and what the new rule flags come from reading the code on the PR branch, so the commands in the verification section must be run before anything is published. Some of the choices are recommendations that Rajeev has not confirmed yet; they are collected in the last section.

## Background

Shuttle runs one task at a time and can only switch between tasks at a scheduling point (e.g., a call to `thread::switch()`). The checker is defined in terms of the following:

- A **step** is the code that a task runs between two consecutive scheduling points.
- A **resource** is one piece of shared state that a primitive owns (e.g., a semaphore or an atomic). Each resource holds a `ResourceSignature`.
- An **operation** is what one call into a primitive does to its resource, as the caller sees it (e.g., `fetch_update` on an atomic, or `release` on a semaphore).
- An **access** is one call to `ExecutionState::record_access`, which a primitive makes when it reads or changes a resource. We say that the primitive *reports* the access.

If a step makes two accesses, no execution that Shuttle explores runs another task between them, so Shuttle can miss bugs that depend on such an interleaving. We call this a **missing scheduling point**. Issue #262 and PR #389 are examples in Shuttle's own primitives.

The checker is in `shuttle-engine/src/runtime/step_check.rs`. It is compiled in only with the `check-scheduling-points` cargo feature, which is off by default and which only a dedicated job in Shuttle's CI enables, so people who use Shuttle to test their own code pay nothing for it. That job runs the tests of the four **core crates** (`shuttle`, `shuttle-engine`, `shuttle-std` and `shuttle-schedulers`); the wrapper crates under `wrappers/` don't pass with the checker yet. On the PR branch, `BatchSemaphore` and the atomics in `shuttle-std` report their accesses.

The **current rule** (in `Access::conflicts_with`) is that a step fails if it accesses two distinct resources and at least one of the two accesses is a write. A primitive whose operation needs several accesses in one step can wrap them in `ExecutionState::atomic_step`, which makes the checker count them as one access; nothing on the PR branch uses it outside of tests.

We have already agreed with sarsko on the order of the series: the **fix PRs** (each fixes missing scheduling points in one primitive and adds regression tests) go in first, and this PR goes in after them. So this PR stays open while the fix PRs land, and the commit described here revises it in place.

## Why the current rule is too weak

The current rule passes each step in the table below, although in each one Shuttle never produces an outcome that a real execution can produce. In the table, `R(x)` is a read of resource `x` and `W(x)` is a write of it. Task `T1` makes its accesses in a single step, and each access of task `T2` is in a step of its own.

| Name of the case | Accesses of `T1` in one step | Accesses of `T2` | Outcome that Shuttle never produces |
|---|---|---|---|
| repeated read | `a = R(x); b = R(x)` | `W(x)` | `a != b` |
| repeated write | `W(x); W(x)` | `R(x)` | `T2` reads the value of the first write |
| reads of two resources | `a = R(x); b = R(y)` | `W(x); W(y)` | `a` is the old value of `x` and `b` is the new value of `y` |

The repeated read is the example from dylanjwolff's comment, and the checker passes it because both accesses are to the same resource. The repeated write is sarsko's point about several changes to one resource, and it passes for the same reason. Nobody raised the reads of two resources on the PR, but they follow from the same argument: the checker passes them because neither access is a write. Thus both conditions in the current rule (that the resources are distinct, and that one access is a write) let missing scheduling points through.

## The strict rule

We propose the **strict rule**: a step makes at most one access. It does not matter which resources the accesses are to, or whether they read or write. With at most one access in a step, there is no pair of accesses that another task would have needed to run between, so the strict rule fails all of the cases in the table.

The cost of the strict rule is that it also fails a step in which one operation makes several accesses to its own resource, which the current rule passes. On the PR branch, `fetch_update` on the atomics is such a step, because it reports a read and then a write. Each such step has to be changed to make one access, or be wrapped in `atomic_step` with a justification (see the next section). We expect more of them in the wrapper crates and on the branches that hold the fixes, and we don't know how many yet.

We considered a weaker alternative that only drops the condition about writes, so that a step may access one resource any number of times. It fails the reads of two resources, but passes the repeated read and the repeated write, which are the cases the reviewers raised. For this reason we are not proposing it.

## Deliberate exceptions and ATOMICITY comments

A **deliberate exception** is a step that is allowed to make more than one access, because the primitive's author has argued that no interleaving is lost. There are two kinds, and we want both to carry a comment that begins with `// ATOMICITY:` and gives the argument, in the same way that an `unsafe` block carries a `// SAFETY:` comment. Every deliberate exception can then be found by searching for one string.

The first kind is an operation that is atomic by design and needs several accesses (e.g., an operation that releases a lock and adds the task to a wait list as one action). The primitive wraps the accesses in `ExecutionState::atomic_step`, and the comment goes above the call:

```rust
// ATOMICITY: <why no other task can observe the state between these accesses>
ExecutionState::atomic_step(|| {
    a.access();
    b.access();
});
```

The second kind is an access that the primitive does not report, because it commutes with every other operation on the resource. `Acquire::poll` in `shuttle-engine/src/future/batch_semaphore.rs` has two of these: a first poll of an unfair semaphore's `Acquire` that only blocks, and a poll that finds permits that `release` already granted. `atomic_step` cannot express this kind, because the unreported access shares its step with an access that the caller made before the poll, which is outside any scope that `atomic_step` could wrap. The comment block in `Acquire::poll` already gives the argument; it needs the `// ATOMICITY:` tag.

## Changes to make in the commit

The work is one new commit on top of `a6744fa` on `scheduling-point-checker`. The branch is behind `awslabs/shuttle` `main` but merged cleanly as of Nov 15, so no rebase or force-push should be needed. Paths below are relative to the root of the repository.

1. In `shuttle-engine/src/runtime/step_check.rs`, change the check so that a second access in a step fails unless both accesses were made inside the same `atomic_step` scope. Keep the list of the step's accesses and their call sites, because the failure message prints them. Remove the `resource` address field from `Access` and the paragraph in the module docs about identifying resources by address, since the check no longer compares resources. Restate the module docs and the failure message for the strict rule. The message must still begin with `missing scheduling point`, which the tests match on, and it should mention the `// ATOMICITY:` comment where it mentions `atomic_step`.
2. Remove `AccessKind`, since the strict rule never reads it: the enum in `shuttle-engine/src/sync_types.rs`, its re-export in `shuttle-engine/src/lib.rs`, the parameter of `ExecutionState::record_access` and `check_access` in `shuttle-engine/src/runtime/execution.rs`, the `record_access` helper and its seven call sites in `shuttle-engine/src/future/batch_semaphore.rs`, and the five call sites in `shuttle-std/src/sync/atomic/mod.rs`. Also remove the sentence in the docs of `record_access` that says the argument must be the signature the resource holds and not a copy, which only mattered for the address comparison.
3. In `shuttle-std/src/sync/atomic/mod.rs`, make `fetch_update` report one access after its `thread::switch()`, where it reports two today.
4. In the docs of `ExecutionState::atomic_step`, replace the sentence that asks for a comment with the `// ATOMICITY:` convention. Tag the existing comment block in `Acquire::poll` with `// ATOMICITY:`.
5. In `.github/workflows/tests.yml`, add a step to the `test-check-scheduling-points` job that fails when a call to `atomic_step(` in a `.rs` file has no `// ATOMICITY:` in the comment directly above it.
6. Restate the rule in the comments on the `check-scheduling-points` feature in `shuttle-engine/Cargo.toml`, `shuttle-std/Cargo.toml` and `shuttle/Cargo.toml`.
7. Update the tests in `shuttle/tests/basic/scheduling_point_checker.rs`, as the next paragraph describes.

For the tests, the `Resource` helper needs only one method that reports an access, in place of `read` and `write`. `accesses_to_same_resource_pass` becomes a test that expects the failure (gated on the feature and marked `should_panic`, like the other failing tests in the file). Once reads and writes are not distinguished, this one test covers both the repeated read and the repeated write. `reads_of_two_resources_pass` and `read_then_write_of_two_resources_fails` become duplicates of `two_resources_in_one_step_fail` and should be removed. We'd add one test that two accesses to one resource inside `atomic_step` pass. The two existing `atomic_step` calls in the file need `// ATOMICITY:` comments, and the module docs at the top of the file need the strict rule. The remaining tests, including the two for the unreported accesses in `Acquire::poll`, should pass unchanged.

The commit message should follow the style of the three commits already on the branch (a subject that says what the change does, e.g., "Fail a step that makes more than one access to shared state").

## Changes that are out of scope

The following are deliberately left out of this commit, and should not be added to it:

- Reporting the reads in `BatchSemaphore::is_closed` and `available_permits`. Under the strict rule each is an access, but reporting them means changing where std `Mutex::lock` takes its scheduling point, which changes scheduling and belongs in its own PR. The `TODO` comments on those two methods stay.
- `TrackedCell`, a cell type that reports every borrow as an access, and the helpers for scheduling points in `Drop` and `poll`. These are on the `tracked-cell` branch of the fork.
- The fixes on the `checker-demo` branch of the fork. Both `tracked-cell` and `checker-demo` are based on the checker with the current rule and will need rebasing later. `checker-demo` has four `atomic_step` calls in the tokio wrapper's `Notify` that will need `// ATOMICITY:` comments at that point.
- The permit that sarsko suggested on the PR: `switch` would return a permit, and an access to a `TrackedCell` would require one.
- Any change to where a primitive takes a scheduling point. If the strict rule flags a missing scheduling point in the core crates, that is a finding to report to Rajeev, and the fix goes in a fix PR ahead of this one.

## Verification

The commit is ready when the following pass. These are the commands that the CI job runs, plus the same tests with the feature off:

```
cargo nextest run --release -p shuttle -p shuttle-engine -p shuttle-std -p shuttle-schedulers --features shuttle/check-scheduling-points
cargo nextest run --release -p shuttle -p shuttle-engine -p shuttle-std -p shuttle-schedulers
cargo clippy --all-targets --features check-scheduling-points -- -D clippy::all
cargo fmt --check
```

We don't know yet whether the strict rule fails steps in the core crates other than `fetch_update`. For each new failure, read the failure message (it lists the step's accesses and where the step began) and decide which of two things it is. If the accesses belong to one operation that is atomic by design, wrap them in `atomic_step` with an `// ATOMICITY:` comment in this commit. If it is a missing scheduling point, stop and report it to Rajeev with the failure message, and do not change the primitive in this commit.

The PR description has a table of instruction counts from `perf stat -e instructions:u` on fixed-seed lock and counter drivers. If those drivers are available on the machine, rerun them for the "Checker on" column, and confirm that the "checker off" numbers are unchanged. If they aren't available, say in the description that the "Checker on" column predates this commit.

## Publishing

Rajeev should see the diff, the test output and the final text of the comment before anything is published. After that, the steps are:

1. Push the new commit to `scheduling-point-checker` on `jorajeev/shuttle` as a fast-forward. Do not rebase or force-push.
2. Edit the description of PR #397. Under "How the checker works", replace the first paragraph with the text below. In the paragraph about `BatchSemaphore`, change "reports a write" to "reports an access". In the paragraph about the two decisions, remove the second one (identifying resources by address), which no longer applies. Update the table of instruction counts as the verification section describes.
3. Post the comment in the next section on PR #397, adjusted for anything that turned out differently during the work.

The replacement paragraph for the PR description is:

> A **step** is the code a task runs between two consecutive scheduling points. With the new `check-scheduling-points` cargo feature, a test fails as soon as a step makes a second access to shared state, whichever resources the two accesses are to and whether they read or write. The failure names the scheduling point at which the step began, and the call site of each access. Primitives report accesses with `ExecutionState::record_access`, right after their scheduling point. Accesses inside `ExecutionState::atomic_step` count as one access, for primitives that deliberately make several accesses in one step; each call carries an `// ATOMICITY:` comment that justifies it (nothing uses it yet).

## Draft comment for the PR

The text of the comment is everything between the two horizontal rules below. It is written to be posted after the commit is pushed, so it calls the current rule "the old rule" and the strict rule "the new rule".

---

Thanks both. You're right that the rule was too weak, and I've pushed a commit that replaces it.

The old rule failed a step only if it accessed two distinct resources and one of the accesses was a write. That passes each of these, where `T1` makes its accesses in one step:

```
T1: a = R(x); b = R(x)      T2: W(x)          a != b is never observed
T1: W(x); W(x)              T2: R(x)          T2 never reads the value of the first write
T1: a = R(x); b = R(y)      T2: W(x); W(y)    the old x together with the new y is never observed
```

The first is dylanjwolff's example and the second is the case sarsko raised. The third wasn't raised here, but it has the same cause: two reads of different resources also need a scheduling point between them.

The new rule is that a step makes at most one access to shared state, whichever resources the accesses are to and whether they read or write. `AccessKind` is gone, because nothing reads it anymore.

A primitive that has to make several accesses in one step on purpose wraps them in `ExecutionState::atomic_step`, and each call now needs an `// ATOMICITY:` comment that says why the accesses form one operation, like the `// SAFETY:` comment on an `unsafe` block. A CI step checks for the comment. The two places in `Acquire::poll` that don't report an access, because it commutes with every other operation on the semaphore, carry the same comment.

On the permit: the new rule is the one that a permit from `switch` would enforce, since one scheduling point allows one access. I don't think a permit can be checked at compile time everywhere, though. A `Drop` impl, or a poll after a wake-up, has no `switch` call to get a permit from, and a permit can be held across a later scheduling point, so we'd still need the runtime check for those. I'd like to get the fixes in first and then look at the permit as a layer on top of `TrackedCell`.

The order we agreed on doesn't change: the fix PRs first, then `TrackedCell`, then this PR. I'll rerun the fix branches with the new rule, because it will flag steps that the old one passed.

`is_closed` and `available_permits` still don't report. Under the new rule each is an access, and reporting them means changing where std `Mutex::lock` takes its scheduling point, so I'll do that in a separate PR.

---

## Decisions that Rajeev has not confirmed

Rajeev has agreed to the strict rule, to `atomic_step` as the way to make a deliberate exception, to the `// ATOMICITY:` comments, and to keeping the checker as a feature that only Shuttle's developers enable. The following points in this document are recommendations that he has not confirmed, and each one changes either the commit or the comment:

- Removing `AccessKind`. The alternative is to keep it for the failure message only, in which case the tests keep `read` and `write` and get separate tests for the repeated read and the repeated write.
- Enforcing the `// ATOMICITY:` comment with a grep step in CI. The alternative is a `&'static str` reason argument to `atomic_step`, which the compiler enforces.
- Requiring the `// ATOMICITY:` tag on the two unreported accesses in `Acquire::poll`.
- Leaving `is_closed` and `available_permits` out of this commit.
- The position on the permit in the draft comment (the runtime check is needed in any case, and the permit is something to look at after the fix PRs land). In an earlier comment on the PR, Rajeev said he would prototype a hybrid of a permit and a runtime check, so the paragraph should say whatever his current view is.
