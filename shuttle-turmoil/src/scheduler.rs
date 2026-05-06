use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::time::Duration;

use turmoil_net::{HostId, Packet, Verdict};

const DEFAULT_TICK_US: u64 = 1_000; // 1ms per poll
const DEFAULT_MAX_SKEW_US: u64 = 100_000; // 100ms

/// Clock-based packet scheduler. Packets are enqueued with a delivery
/// time determined by rules; on each tick, due packets are delivered
/// in enqueue order (preserving per-flow TCP ordering). Cross-flow
/// nondeterminism comes from Shuttle's task scheduling — which host
/// gets polled (and thus egresses packets) first varies per iteration.
pub(crate) struct Scheduler {
    clock: u64,
    tick_us: u64,
    max_skew_us: u64,
    /// Per-host clock tracking for skew enforcement.
    host_clocks: Vec<(HostId, u64)>,
    /// Priority queue ordered by delivery time.
    queue: BinaryHeap<Reverse<Scheduled>>,
    egress_buf: Vec<Packet>,
}

struct Scheduled {
    deliver_at: u64,
    pkt: Packet,
}

impl PartialEq for Scheduled {
    fn eq(&self, other: &Self) -> bool {
        self.deliver_at == other.deliver_at
    }
}
impl Eq for Scheduled {}

impl PartialOrd for Scheduled {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Scheduled {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.deliver_at.cmp(&other.deliver_at)
    }
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            clock: 0,
            tick_us: DEFAULT_TICK_US,
            max_skew_us: DEFAULT_MAX_SKEW_US,
            host_clocks: Vec::new(),
            queue: BinaryHeap::new(),
            egress_buf: Vec::with_capacity(64),
        }
    }

    pub fn with_tick(mut self, tick: Duration) -> Self {
        self.tick_us = tick.as_micros() as u64;
        self
    }

    pub fn with_max_skew(mut self, skew: Duration) -> Self {
        self.max_skew_us = skew.as_micros() as u64;
        self
    }

    pub fn register_host(&mut self, id: HostId) {
        self.host_clocks.push((id, 0));
    }

    /// Advance the clock for `host` by one tick, then deliver due
    /// packets and collect egress. Loops until no immediately-
    /// deliverable packets remain (handles chains like SYN → SYN-ACK
    /// → ACK within a single step).
    ///
    /// Returns false if the host is too far ahead (skew limit) and
    /// should yield to let other hosts catch up.
    pub fn step(&mut self, host: HostId) -> bool {
        let (my_clock, min_other) = self.host_clock_info(host);

        if my_clock.saturating_sub(min_other) >= self.max_skew_us {
            return false;
        }

        let new_clock = my_clock + self.tick_us;
        self.set_host_clock(host, new_clock);
        if new_clock > self.clock {
            self.clock = new_clock;
        }

        turmoil_net::check_retx_for_host(host);
        self.drain();
        true
    }

    /// Collect egress and deliver due packets without advancing the
    /// clock. Used after a host poll to flush packets produced during
    /// the poll (e.g., FIN from shutdown, data from write_all).
    pub fn flush(&mut self) {
        self.drain();
    }

    fn drain(&mut self) {
        loop {
            self.deliver_due();
            self.collect_egress();
            let has_due = self.queue.peek()
                .map(|Reverse(s)| s.deliver_at <= self.clock)
                .unwrap_or(false);
            if !has_due {
                break;
            }
        }
    }

    fn host_clock_info(&self, host: HostId) -> (u64, u64) {
        let mut my_clock = 0;
        let mut min_other = u64::MAX;
        for &(id, clock) in &self.host_clocks {
            if id == host {
                my_clock = clock;
            } else {
                min_other = min_other.min(clock);
            }
        }
        if min_other == u64::MAX {
            min_other = my_clock;
        }
        (my_clock, min_other)
    }

    fn set_host_clock(&mut self, host: HostId, value: u64) {
        for entry in &mut self.host_clocks {
            if entry.0 == host {
                entry.1 = value;
                return;
            }
        }
    }

    fn deliver_due(&mut self) {
        while let Some(Reverse(scheduled)) = self.queue.peek() {
            if scheduled.deliver_at <= self.clock {
                let Reverse(scheduled) = self.queue.pop().unwrap();
                turmoil_net::deliver(scheduled.pkt);
            } else {
                break;
            }
        }
    }

    fn collect_egress(&mut self) {
        turmoil_net::egress_all(&mut self.egress_buf);

        for pkt in self.egress_buf.drain(..) {
            let verdict = turmoil_net::evaluate(&pkt);
            match verdict {
                Verdict::Drop => {}
                Verdict::Pass => {
                    self.queue.push(Reverse(Scheduled {
                        deliver_at: self.clock,
                        pkt,
                    }));
                }
                Verdict::Deliver(delay) => {
                    let delay_us = delay.as_micros() as u64;
                    self.queue.push(Reverse(Scheduled {
                        deliver_at: self.clock + delay_us,
                        pkt,
                    }));
                }
            }
        }
    }
}
