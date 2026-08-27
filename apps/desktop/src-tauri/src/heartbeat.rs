//! The 5-second heartbeat (ARCHITECTURE §3, CONTRACTS §11's `Heartbeat` host
//! event).
//!
//! Two consumers, one clock:
//!
//! * **Engine liveness.** Each tick pings the transport (`kind 3`, W07's
//!   `Ping`). The pong count is read from `TransportStats`; a stalled engine
//!   shows up as pings outrunning pongs, which the pump's crash path then
//!   reports properly when the pipe actually dies.
//! * **Webview liveness.** Each tick fans a `HostEvent::Heartbeat { n }` to
//!   every subscribed window. A webview that has missed three beats is due a
//!   `webview.reload()` — Tauri v2 exposes no portable crash callback, so the
//!   beat is the only cross-platform signal (tracked as an open question in
//!   ARCHITECTURE §3). The reload half lands with the webview-recovery work;
//!   what ships here is the beat itself and its counters.
//!
//! ADR-017: tests drive [`Heartbeat::tick`] directly and assert COUNTERS.
//! Nothing here sleeps in a test, and no wall-clock duration is asserted.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::engine_host::EngineHost;
use crate::windows::{HostEvent, SessionRegistry};

/// Beat period. A constant, not configuration: three missed beats = 15 s,
/// which is the reload threshold ARCHITECTURE §3 records.
pub const PERIOD: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct HeartbeatCounters {
    /// Ticks driven (by the interval task or by a test).
    pub ticks: AtomicU64,
    /// Pings actually written to the engine transport.
    pub engine_pings: AtomicU64,
    /// Heartbeat events fanned to windows (one per tick, delivered per window
    /// by the registry, whose own counter tracks deliveries).
    pub beats: AtomicU64,
}

pub struct Heartbeat {
    engine: Arc<EngineHost>,
    registry: Arc<SessionRegistry>,
    pub counters: HeartbeatCounters,
}

impl Heartbeat {
    #[must_use]
    pub fn new(engine: Arc<EngineHost>, registry: Arc<SessionRegistry>) -> Self {
        Self {
            engine,
            registry,
            counters: HeartbeatCounters::default(),
        }
    }

    /// One beat: ping the engine, beat every window. Public so tests can drive
    /// N beats without a clock.
    pub async fn tick(&self) {
        let n = self.counters.ticks.fetch_add(1, Ordering::Relaxed) + 1;
        let tx = self.engine.transport().await;
        if tx.ping(n).is_ok() {
            self.counters.engine_pings.fetch_add(1, Ordering::Relaxed);
        }
        self.registry.host_event_all(&HostEvent::Heartbeat { n });
        self.counters.beats.fetch_add(1, Ordering::Relaxed);
    }

    /// The production loop: `tick` every [`PERIOD`] until the runtime drops it.
    pub fn spawn(self: Arc<Self>) {
        tauri::async_runtime::spawn(async move {
            let mut interval = tokio::time::interval(PERIOD);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                self.tick().await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_host::{CancelLadder, EngineSource};
    use crate::mock_engine::MockOptions;

    /// Counters, not clocks (ADR-017): three ticks are three pings and three
    /// beats, driven directly with no interval and no sleep.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn three_ticks_are_three_pings_and_three_beats() {
        let engine = EngineHost::spawn(
            EngineSource::Mock(MockOptions::default()),
            CancelLadder::default(),
        )
        .await
        .expect("the mock engine spawns");
        let registry = Arc::new(SessionRegistry::new());
        let hb = Heartbeat::new(Arc::clone(&engine), registry);

        for _ in 0..3 {
            hb.tick().await;
        }
        assert_eq!(hb.counters.ticks.load(Ordering::Relaxed), 3);
        assert_eq!(hb.counters.engine_pings.load(Ordering::Relaxed), 3);
        assert_eq!(hb.counters.beats.load(Ordering::Relaxed), 3);
        engine.shutdown().await;
    }
}
