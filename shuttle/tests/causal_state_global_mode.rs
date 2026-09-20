use shuttle::scheduler::RoundRobinScheduler;
use shuttle::{Config, Runner};

#[test]
fn conflicting_global_causal_state_modes_panic() {
    let _disabled = Runner::new(RoundRobinScheduler::new(1), Config::default());
    let enabled = Config::default().with_causal_state(|| (), |_, _| {});

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Runner::new(RoundRobinScheduler::new(1), enabled)
    }))
    .expect_err("conflicting causal state modes should panic");
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .expect("panic should contain a message");

    assert!(message.contains("causal state was globally initialized as false"));
    assert!(message.contains("this runtime requested true"));
}
