use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use shuttle::future::JoinHandle;
use turmoil_net::{HostId, Net, Rule, ToIpAddrs};

use crate::host_wrapper::{install_scheduler, remove_scheduler, set_child_host_id, HostWrapper};
use crate::scheduler::Scheduler;

type BoxFut = Pin<Box<dyn Future<Output = ()> + Send>>;

/// A Shuttle-driven turmoil-net simulation.
///
/// Register hosts with [`Sim::host`] and clients with [`Sim::client`],
/// then call [`Sim::run`] to execute until all clients complete. Hosts
/// and clients can be added between `run` calls (matching turmoil's
/// API). Packet delivery is driven by a simulated clock — each host
/// poll advances the clock by one tick, and packets with elapsed
/// delivery times are delivered in a random order (via Shuttle's
/// deterministic RNG).
pub struct Sim {
    net: Net,
    hosts: Vec<(HostId, BoxFut)>,
    clients: Vec<(HostId, BoxFut)>,
    tick: Duration,
    max_skew: Duration,
}

impl Sim {
    pub fn new() -> Self {
        Self {
            net: Net::new(),
            hosts: Vec::new(),
            clients: Vec::new(),
            tick: Duration::from_millis(1),
            max_skew: Duration::from_millis(100),
        }
    }

    /// Set the sim clock tick size (how much time advances per poll).
    /// Default: 1ms.
    pub fn tick(&mut self, tick: Duration) -> &mut Self {
        self.tick = tick;
        self
    }

    /// Set the maximum allowed clock skew between hosts. A host that
    /// is too far ahead will yield until others catch up. Default: 100ms.
    pub fn max_skew(&mut self, skew: Duration) -> &mut Self {
        self.max_skew = skew;
        self
    }

    /// Install a rule on the network (latency, packet loss, etc).
    pub fn rule(&mut self, rule: impl Rule) -> &mut Self {
        self.net.rule(rule);
        self
    }

    /// Register a host. `addrs` is typically a hostname string like
    /// `"server"` which turmoil-net auto-allocates to 192.168.x.x.
    /// The factory `Fn` (not `FnOnce`) mirrors turmoil — it may be
    /// called again if the host is bounced after a crash.
    pub fn host<A, F, Fut>(&mut self, addrs: A, software: F) -> &mut Self
    where
        A: ToIpAddrs,
        F: Fn() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let id = self.net.add_host(addrs);
        self.hosts.push((id, Box::pin(async move { software().await })));
        self
    }

    /// Register a client. The simulation runs until all clients
    /// complete.
    pub fn client<A, F>(&mut self, addrs: A, client_fut: F) -> &mut Self
    where
        A: ToIpAddrs,
        F: Future<Output = ()> + Send + 'static,
    {
        let id = self.net.add_host(addrs);
        self.clients.push((id, Box::pin(client_fut)));
        self
    }

    /// Run the simulation until all registered clients complete.
    /// Hosts are aborted after clients finish. Can be called multiple
    /// times — add more hosts/clients between calls.
    pub fn run(&mut self) {
        let net = std::mem::replace(&mut self.net, Net::new());
        let guard = net.enter();

        let mut scheduler = Scheduler::new()
            .with_tick(self.tick)
            .with_max_skew(self.max_skew);

        let hosts = std::mem::take(&mut self.hosts);
        let clients = std::mem::take(&mut self.clients);

        for &(id, _) in &hosts {
            scheduler.register_host(id);
        }
        for &(id, _) in &clients {
            scheduler.register_host(id);
        }

        install_scheduler(scheduler);

        let mut host_handles: Vec<JoinHandle<()>> = Vec::new();
        for (id, fut) in hosts {
            set_child_host_id(id);
            let handle = shuttle::future::spawn(HostWrapper { id, inner: fut });
            host_handles.push(handle);
        }

        let mut client_handles: Vec<JoinHandle<()>> = Vec::new();
        for (id, fut) in clients {
            set_child_host_id(id);
            let handle = shuttle::future::spawn(HostWrapper { id, inner: fut });
            client_handles.push(handle);
        }

        shuttle::future::block_on(async {
            for h in client_handles {
                h.await.unwrap();
            }
        });

        for h in host_handles {
            h.abort();
        }

        remove_scheduler();

        self.net = guard.exit();
    }
}

impl Default for Sim {
    fn default() -> Self {
        Self::new()
    }
}
