# Shuttle × turmoil-net Integration

## Overview

`shuttle-turmoil` runs turmoil-net network simulations under Shuttle's
deterministic concurrency scheduler. Shuttle explores different task
interleavings across iterations; turmoil-net provides a simulated POSIX socket
stack. Together they find concurrency and protocol bugs that depend on network
timing.

turmoil-net is runtime-agnostic (kernel is pure `&mut self`), and a
`shuttle` feature flag gates out its tokio-specific fixture module.
Everything else — the kernel, fabric, shim, rules — is reused unchanged.

---

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│  Shuttle Runtime                                        │
│                                                         │
│   ┌──────────┐  ┌──────────┐  ┌──────────┐             │
│   │ Host A   │  │ Host B   │  │ Client   │             │
│   │ (task)   │  │ (task)   │  │ (task)   │             │
│   └──────────┘  └──────────┘  └──────────┘             │
│        │              │              │                  │
│        └──────────────┴──────────────┘                  │
│                       │                                 │
│          Shuttle's random scheduler picks               │
│          which task to poll next                        │
│                                                         │
└─────────────────────────────────────────────────────────┘
         │
         │  thread-local CURRENT
         ▼
┌─────────────────────────────────────────────────────────┐
│  turmoil-net                                            │
│                                                         │
│   Net ─── Fabric ─── Kernel (per host)                  │
│              │                                          │
│         egress/deliver/evaluate (free functions)        │
│              │                                          │
│   shim::tokio::net (TcpStream, UdpSocket, etc.)        │
│                                                         │
└─────────────────────────────────────────────────────────┘
         │
         │  thread-local SCHEDULER
         ▼
┌─────────────────────────────────────────────────────────┐
│  Packet Scheduler                                       │
│                                                         │
│   sim clock (µs)                                        │
│   per-host clocks + max_skew enforcement                │
│   priority queue: (deliver_at, packet)                  │
│                                                         │
└─────────────────────────────────────────────────────────┘
```

---

## How It Works

### HostWrapper

Every host task is wrapped in `HostWrapper<F>`, which drives packet
delivery inline on each poll — no extra Shuttle tasks needed:

```rust
impl<F: Future + Unpin> Future for HostWrapper<F> {
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if !turmoil_net::try_set_current(self.id) {
            return Poll::Pending; // Net gone — simulation ended
        }
        if !step(self.id) {
            cx.waker().wake_by_ref();
            return Poll::Pending; // Clock skew limit — yield
        }
        turmoil_net::set_current(self.id);
        let result = Pin::new(&mut self.inner).poll(cx);
        flush();
        result
    }
}
```

Each poll of a host task:
1. **step** — advance the host's sim clock by one tick, run retransmit
   for this host, deliver due packets, collect egress from all kernels,
   evaluate rules, enqueue.
2. **poll inner** — run the user's future (socket ops route to the
   correct kernel via `set_current`).
3. **flush** — collect and deliver any packets produced during the inner
   poll (e.g., SYN from connect, data from write, FIN from shutdown).

### Packet scheduler

The scheduler holds a simulated clock and a priority queue. On each
step, it advances the host's clock, then runs a drain loop:

1. Deliver all packets with `deliver_at <= clock` to their target kernels.
2. Collect egress from all kernels, evaluate rules, enqueue with delay.
3. Repeat until no immediately-due packets remain (handles chains like
   SYN → SYN-ACK → ACK within a single step).

`flush()` runs the same drain loop without advancing the clock.

### Rules

Rules are evaluated at egress time. The rule determines a packet's
`deliver_at` timestamp:
- `Verdict::Pass` → deliver at current clock (immediately due)
- `Verdict::Deliver(delay)` → deliver at `clock + delay`
- `Verdict::Drop` → discard

### Max clock skew

The `max_skew` parameter bounds how far any host's clock can get ahead
of the slowest host. When a host exceeds the limit, it yields until
others catch up. Default: 100ms.

### Retransmit

turmoil-net's count-based retransmit (`check_retx`) increments a
per-socket counter on each call and triggers retransmission when it
crosses a threshold. Under shuttle, the scheduler calls
`check_retx_for_host(host)` once per step — only for the host being
polled — so the counter reflects the host's own progress rather than
global egress cadence.

### Host-scoped spawn

Sub-tasks spawned via `shuttle_turmoil::spawn` inherit the parent's
`HostId`, so socket operations in spawned tasks route to the correct
kernel automatically.

---

## What Shuttle Explores

Across iterations with different random seeds:

| Dimension | Mechanism |
|-----------|-----------|
| Task interleaving | Which host task runs next (Shuttle scheduling) |
| Packet delivery timing | Which host's clock advances first |
| Connection race | Server bind vs. client connect ordering |
| Multi-task races | Interleaving of sub-tasks within a host |
| Packet loss/delay | Rules with `Verdict::Deliver(delay)` or `Drop` |

All choices are deterministic within an iteration and replayable via
`shuttle::replay(f, schedule)`.

---

## Usage

```rust
use shuttle_turmoil::Sim;
use turmoil_net::shim::tokio::net::{TcpListener, TcpStream};

async fn connect(addr: &str) -> TcpStream {
    loop {
        match TcpStream::connect(addr).await {
            Ok(s) => return s,
            Err(_) => shuttle::future::yield_now().await,
        }
    }
}

shuttle_turmoil::check(|| {
    let mut sim = Sim::new();
    sim.host("server", || async {
        let listener = TcpListener::bind("0.0.0.0:9000").await.unwrap();
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 5];
        sock.read_exact(&mut buf).await.unwrap();
        sock.write_all(&buf).await.unwrap();
    });
    sim.client("client", async {
        let mut c = connect("server:9000").await;
        c.write_all(b"hello").await.unwrap();
        let mut buf = [0u8; 5];
        c.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");
    });
    sim.run();
}, 1000);
```

Each of the 1000 iterations explores a different task interleaving and
packet delivery order. Failures are reported with a replayable schedule.

Note: client connect uses a retry loop because Shuttle may schedule
the client before the server has bound its listener.

---

## Configuration

```rust
use std::time::Duration;

let mut sim = Sim::new();
sim.tick(Duration::from_millis(5));      // 5ms per poll (default: 1ms)
sim.max_skew(Duration::from_millis(50)); // 50ms max divergence (default: 100ms)
sim.rule(Latency::fixed(Duration::from_millis(10))); // 10ms latency on all packets
sim.host("server", || async { /* ... */ });
sim.client("client", async { /* ... */ });
sim.run();
```

---

## Known Limitations

| Limitation | Notes |
|-----------|-------|
| Host crash/restart | Requires aborting task groups |
| Time/timers | Sim clock doesn't drive tokio timers |
| Send bound | Shuttle's executor requires `Send` |
| Packet reordering (TCP) | TCP delivery is FIFO per-flow |
| UDP reordering | Could add random insertion at enqueue |

---

## Future Work

1. **Cross-flow reordering** — when multiple flows have due packets in
   the same tick, shuffle inter-flow delivery order using `shuttle::rand`
   while preserving intra-flow FIFO. Explores more delivery interleavings
   independently of task scheduling.

2. **UDP reordering** — for UDP packets within a flow, randomly reorder
   at enqueue time (or delivery time) using `shuttle::rand`. TCP must
   remain FIFO.

3. **Latency ranges** — extend `Latency` to support
   `Latency::range(min, max)`. The scheduler samples uniformly within
   the range using Shuttle's deterministic RNG.

4. **Host crash/restart** — abort all tasks in a host group, optionally
   reinitialize kernel, spawn fresh host task.

5. **Timer integration** — connect sim clock to a mock time source so
   `tokio::time::sleep` works within the simulation.
