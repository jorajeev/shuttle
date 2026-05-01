//! Configuration types for Shuttle test execution.

use std::cell::Cell;

/// Configuration parameters for Shuttle
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
    /// Stack size allocated for each thread
    pub stack_size: usize,

    /// How to persist schedules when a test fails
    pub failure_persistence: FailurePersistence,

    /// Maximum number of steps a single iteration of a test can take, and how to react when the
    /// limit is reached
    pub max_steps: MaxSteps,

    /// Time limit for an entire test. If set, calls to [`Runner::run`] will return when the time
    /// limit is exceeded or the [`Scheduler`](crate::scheduler::Scheduler) chooses to stop (e.g.,
    /// by hitting its maximum number of iterations), whichever comes first. This time limit will
    /// not abort a currently running test iteration; the limit is only checked between iterations.
    pub max_time: Option<std::time::Duration>,

    /// Whether to silence warnings about Shuttle behaviors that may miss bugs or introduce false
    /// positives.
    pub silence_warnings: bool,

    /// Whether to call the `Span::record()` method to update the step count.
    pub record_steps_in_span: bool,

    /// The config to define how to handle ungraceful shutdowns, ie. when the test panics.
    pub ungraceful_shutdown_config: UngracefulShutdownConfig,
}

impl Config {
    /// Create a new default configuration
    pub fn new() -> Self {
        Self {
            stack_size: 0xf000,
            failure_persistence: FailurePersistence::Print,
            max_steps: MaxSteps::FailAfter(1_000_000),
            max_time: None,
            silence_warnings: false,
            record_steps_in_span: false,
            ungraceful_shutdown_config: UngracefulShutdownConfig::default(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

/// Specifies how to persist schedules when a Shuttle test fails
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FailurePersistence {
    /// Do not persist failing schedules
    None,
    /// Print failing schedules to stdout/stderr
    Print,
    /// Persist schedules as files in the given directory, or the current directory if None.
    File(Option<std::path::PathBuf>),
}

/// Specifies an upper bound on the number of steps a single iteration of a Shuttle test can take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MaxSteps {
    /// Do not enforce any bound on the maximum number of steps
    None,
    /// Fail the test (by panicking) after the given number of steps
    FailAfter(usize),
    /// When the given number of steps is reached, stop the current iteration
    ContinueAfter(usize),
}

#[derive(Copy, Clone, Debug)]
#[non_exhaustive]
/// The config to define how to handle ungraceful shutdowns, ie. when the test panics.
pub struct UngracefulShutdownConfig {
    /// Setting this to `true` will cause scheduling to stop as soon as a task panics.
    pub immediately_return_on_panic: bool,

    /// What to do with the continuation function when it is dropped after a panic.
    pub continuation_function_behavior: ContinuationFunctionBehavior,
}

impl UngracefulShutdownConfig {
    /// Create a new default `UngracefulShutdownConfig`
    pub const fn new() -> Self {
        Self {
            immediately_return_on_panic: false,
            continuation_function_behavior: ContinuationFunctionBehavior::new(),
        }
    }
}

impl Default for UngracefulShutdownConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Copy, Clone, Debug)]
#[non_exhaustive]
/// What to do with the continuation function when a task panics.
pub enum ContinuationFunctionBehavior {
    /// Drop the continuation function when a task panics.
    Drop,
    /// Leak the continuation function when a task panics.
    Leak,
}

impl ContinuationFunctionBehavior {
    /// Create a new default `ContinuationFunctionBehavior`
    pub const fn new() -> Self {
        Self::Leak
    }
}

impl Default for ContinuationFunctionBehavior {
    fn default() -> Self {
        Self::new()
    }
}

std::thread_local! {
    /// Thread-local storage for the ungraceful shutdown config for the current execution.
    pub static UNGRACEFUL_SHUTDOWN_CONFIG: Cell<UngracefulShutdownConfig> = const { Cell::new(UngracefulShutdownConfig::new()) };
}

/// If this environment variable is set, then Shuttle will capture the backtrace of each task.
pub const CAPTURE_BACKTRACE: &str = "SHUTTLE_CAPTURE_BACKTRACE";

/// The random seed used to initialize schedulers.
pub const RANDOM_SEED: &str = "SHUTTLE_RANDOM_SEED";

/// If this is set, then certain warnings will not be emitted.
pub const SILENCE_WARNINGS: &str = "SHUTTLE_SILENCE_WARNINGS";

/// Used in the annotation scheduler to specify where to write the annotations.
pub const ANNOTATION_FILE: &str = "SHUTTLE_ANNOTATION_FILE";

#[cfg(feature = "annotation")]
pub fn annotation_file() -> String {
    std::env::var(ANNOTATION_FILE).unwrap_or_else(|_| "annotated.json".to_string())
}

pub fn silence_warnings() -> bool {
    std::env::var(SILENCE_WARNINGS).is_ok()
}

pub fn backtrace_enabled() -> bool {
    std::env::var(CAPTURE_BACKTRACE).is_ok()
}

pub fn seed_from_env(fallback_seed: u64) -> u64 {
    let seed_env = std::env::var(RANDOM_SEED);
    match seed_env {
        Ok(s) => match s.as_str().parse::<u64>() {
            Ok(seed) => {
                tracing::info!(
                    "Initializing scheduler with the seed provided by {}: {}",
                    RANDOM_SEED,
                    seed
                );
                seed
            }
            Err(err) => panic!("The seed provided by {RANDOM_SEED} is not a valid u64: {err}"),
        },
        Err(_) => fallback_seed,
    }
}
