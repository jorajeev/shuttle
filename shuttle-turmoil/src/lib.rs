//! Run turmoil-net network simulations under Shuttle's deterministic
//! concurrency scheduler.
//!
//! # Usage
//!
//! ```ignore
//! use shuttle_turmoil::Sim;
//!
//! shuttle_turmoil::check(|| {
//!     let mut sim = Sim::new();
//!     sim.host("server", || async {
//!         let listener = turmoil_net::shim::tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
//!         let (mut s, _) = listener.accept().await.unwrap();
//!         // ...
//!     });
//!     sim.client("client", async {
//!         let mut s = turmoil_net::shim::tokio::net::TcpStream::connect("server:8080").await.unwrap();
//!         // ...
//!     });
//!     sim.run();
//! }, 100);
//! ```

use std::future::Future;

mod host_wrapper;
mod scheduler;
mod sim;

pub use sim::Sim;
pub use turmoil_net::{self, HostId, Latency, Net, Packet, Rule, RuleGuard, Verdict};

/// Spawn a task scoped to the current host. The spawned task will
/// have `set_current(host_id)` called on every poll, ensuring its
/// socket syscalls route to the correct kernel.
///
/// Must be called from within a host future (inside `Sim::host` or
/// `Sim::client`). Panics if called outside a host context.
pub fn spawn<F>(fut: F) -> shuttle::future::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let host_id = host_wrapper::current_host_id()
        .expect("shuttle_turmoil::spawn called outside a host context");
    let handle = shuttle::future::spawn(host_wrapper::HostWrapper {
        id: host_id,
        inner: Box::pin(fut),
    });
    // shuttle::future::spawn yields (thread::switch). When we resume,
    // another task may have changed net.current. Restore it so the
    // calling code can continue using the kernel.
    turmoil_net::try_set_current(host_id);
    handle
}


/// Run a simulation under Shuttle's random scheduler for `iterations` executions.
pub fn check<F>(f: F, iterations: usize)
where
    F: Fn() + Send + Sync + 'static,
{
    shuttle::check_random(f, iterations);
}

/// Run a simulation under Shuttle's PCT scheduler.
pub fn check_pct<F>(f: F, iterations: usize, depth: usize)
where
    F: Fn() + Send + Sync + 'static,
{
    shuttle::check_pct(f, iterations, depth);
}

/// Replay a specific Shuttle schedule for debugging.
pub fn replay<F>(f: F, schedule: &str)
where
    F: Fn() + Send + Sync + 'static,
{
    shuttle::replay(f, schedule);
}
