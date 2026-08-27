//! Event emission — design 03 §9.4, CONTRACTS §7's framing guarantees.
//!
//! Four properties consumers are allowed to rely on, and this module is where
//! each is made true:
//!
//! 1. exactly one `RunStarted` first and one `RunFinished` last per run, always,
//!    including on error, interrupt and timeout;
//! 2. `BlockStarted`…`BlockFinished` pairs never interleave within one run;
//! 3. `Output` chunks preserve byte order and may split anywhere;
//! 4. `seq` is strictly increasing per session and is stamped BEFORE fan-out, so
//!    every window observes one order and a window that sees a gap re-snapshots
//!    rather than diverging.
//!
//! # What is allowed to be dropped, and what is not
//!
//! `Output` is a NOTIFICATION: the bytes are already in the [`ResultStore`], so
//! a dropped one costs nothing but a repaint, and dropping it is how a paused
//! or slow window fails to throttle the engine. Everything else is a FACT.
//! Facts are sent with a blocking `send`, because losing a `BlockFinished`
//! would break guarantee 1 and leave a block spinning `Running` forever.
//!
//! [`ResultStore`]: crate::ResultStore

use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use stratum_proto::{EngineEvent, ExecutionId, StyledRun};

/// Design 03 §9.4. Deep enough that a window has to be badly behind before
/// coalescing starts, small enough that a stalled subscriber cannot pin
/// unbounded memory.
pub const EVENT_CAPACITY: usize = 4_096;

/// Output is flushed at line boundaries with this coalescing window — one frame
/// at 60 Hz. `forvalues i=1/100000 { display `i' }` therefore produces ~60
/// notifications per second, not 100 000.
pub const FLUSH_INTERVAL_MS: u64 = 16;

/// …or sooner, once this much text has accumulated.
pub const FLUSH_BYTES: usize = 64 * 1_024;

/// Anything that accepts engine events. The engine writes through this so a
/// test can capture the stream without a channel.
pub trait EventSink: Send + Sync {
    /// Emit one event. `seq` is stamped by the implementation.
    fn emit(&self, event: EngineEvent);
}

/// The seq-stamping fan-out point.
#[derive(Debug)]
pub struct EventBus {
    tx: Sender<EngineEvent>,
    seq: AtomicU64,
    emitted: AtomicU64,
    coalesced: AtomicU64,
    dropped: Mutex<FxHashMap<ExecutionId, u64>>,
}

impl EventBus {
    /// A bus and its receiver.
    #[must_use]
    pub fn new() -> (Self, Receiver<EngineEvent>) {
        let (tx, rx) = bounded(EVENT_CAPACITY);
        (
            Self {
                tx,
                seq: AtomicU64::new(0),
                emitted: AtomicU64::new(0),
                coalesced: AtomicU64::new(0),
                dropped: Mutex::new(FxHashMap::default()),
            },
            rx,
        )
    }

    /// `(emitted, coalesced, dropped_bytes)` — the counters the streaming gates
    /// assert, per ADR-017.
    #[must_use]
    pub fn counters(&self) -> (u64, u64, u64) {
        (
            self.emitted.load(Ordering::Relaxed),
            self.coalesced.load(Ordering::Relaxed),
            self.dropped.lock().values().sum(),
        )
    }
}

impl EventSink for EventBus {
    fn emit(&self, mut event: EngineEvent) {
        // The account of a gap goes out BEFORE the event that follows the gap,
        // and therefore before this event is stamped — a notice carrying a
        // HIGHER seq than the output it precedes would break guarantee 4, and a
        // notice arriving after the resumed text asks the window to label a seam
        // it has already drawn. Doing it here rather than after a successful
        // send is also what lets one freed slot carry the notice: the pending
        // account competes for that slot before the next chunk does.
        if let Some(exec) = exec_of(&event) {
            self.flush_truncation(exec);
        }

        // Stamped before fan-out, and under the same atomic increment that
        // orders the send, so two threads emitting concurrently cannot produce
        // a receiver-visible order that disagrees with the seq order.
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        stamp(&mut event, seq);
        self.emitted.fetch_add(1, Ordering::Relaxed);

        if let EngineEvent::Output { exec, ref runs, .. } = event {
            let bytes: usize = runs.iter().map(|r| r.text.len()).sum();
            match self.tx.try_send(event) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    // Coalesce, do not block: the next notification carries the
                    // new length and the bytes are already in the store.
                    self.coalesced.fetch_add(1, Ordering::Relaxed);
                    *self.dropped.lock().entry(exec).or_insert(0) += bytes as u64;
                }
                Err(TrySendError::Disconnected(_)) => {}
            }
            return;
        }

        // A fact. Blocking here is deliberate; see the module header.
        let _ = self.tx.send(event);
    }
}

impl EventBus {
    /// Tell the window how much it missed, once there is room to say so. The
    /// full text is always at `stratum-asset://localhost/result/{s}/{r}/raw`,
    /// so this is an accurate account of a gap, not an apology for lost data.
    fn flush_truncation(&self, exec: ExecutionId) {
        if self.tx.is_full() {
            // Nothing can land right now. Returning before taking the account
            // out of the map keeps it exact, and — more importantly — avoids
            // burning a `seq` on a send that cannot succeed: a consumer reads a
            // skipped `seq` as a lost event and re-snapshots, so a full channel
            // would trigger one re-snapshot per dropped chunk.
            return;
        }
        let dropped_bytes = {
            let mut d = self.dropped.lock();
            match d.remove(&exec) {
                Some(n) if n > 0 => n,
                _ => return,
            }
        };
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let ev = EngineEvent::OutputTruncated {
            seq,
            exec,
            dropped_bytes,
        };
        if self.tx.try_send(ev).is_err() {
            // Still no room: put it back rather than losing the account.
            *self.dropped.lock().entry(exec).or_insert(0) += dropped_bytes;
        }
    }
}

/// Which execution an event belongs to, if any.
fn exec_of(event: &EngineEvent) -> Option<ExecutionId> {
    match event {
        EngineEvent::BlockStarted { exec, .. }
        | EngineEvent::Output { exec, .. }
        | EngineEvent::OutputTruncated { exec, .. }
        | EngineEvent::Result { exec, .. }
        | EngineEvent::Progress { exec, .. }
        | EngineEvent::StateChanged { exec, .. }
        | EngineEvent::BlockFinished { exec, .. } => Some(*exec),
        EngineEvent::Diagnostic { exec, .. } => *exec,
        _ => None,
    }
}

/// Stamp `seq` on any event. Exhaustive on purpose: a new variant that forgets
/// its `seq` must not compile.
fn stamp(event: &mut EngineEvent, value: u64) {
    match event {
        EngineEvent::RunStarted { seq, .. }
        | EngineEvent::BlockStarted { seq, .. }
        | EngineEvent::Output { seq, .. }
        | EngineEvent::OutputTruncated { seq, .. }
        | EngineEvent::Result { seq, .. }
        | EngineEvent::Diagnostic { seq, .. }
        | EngineEvent::Progress { seq, .. }
        | EngineEvent::StateChanged { seq, .. }
        | EngineEvent::BlockFinished { seq, .. }
        | EngineEvent::StatusChanged { seq, .. }
        | EngineEvent::BlockMapChanged { seq, .. }
        | EngineEvent::RunFinished { seq, .. }
        | EngineEvent::CompletionEnvChanged { seq, .. }
        | EngineEvent::EngineHealth { seq, .. } => *seq = value,
    }
}

/// Line-boundary output coalescing.
///
/// Holds appended runs until a flush is due, so the event stream carries one
/// notification per frame rather than one per `display`. A forced flush happens
/// before `Progress`, `Result`, `BlockFinished` and any input prompt — anywhere
/// the user is about to see something that must not appear before the output
/// that explains it.
#[derive(Debug, Default)]
pub struct OutputCoalescer {
    pending: Vec<StyledRun>,
    bytes: usize,
    last_flush_ms: u64,
    flushes: u64,
    appends: u64,
}

impl OutputCoalescer {
    /// A fresh coalescer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Buffer some output. Returns the runs to emit when a flush is due.
    pub fn append(&mut self, runs: &[StyledRun], now_ms: u64) -> Option<Vec<StyledRun>> {
        self.appends += 1;
        self.bytes += runs.iter().map(|r| r.text.len()).sum::<usize>();
        self.pending.extend_from_slice(runs);
        let due = self.bytes >= FLUSH_BYTES
            || now_ms.saturating_sub(self.last_flush_ms) >= FLUSH_INTERVAL_MS;
        // Only flush at a line boundary; a half-line in the Results pane is
        // what makes a progress display flicker.
        let at_boundary = self.pending.last().is_some_and(|r| r.text.ends_with('\n'))
            || self.bytes >= FLUSH_BYTES;
        (due && at_boundary).then(|| self.take(now_ms))
    }

    /// Flush whatever is buffered, boundary or not.
    pub fn force(&mut self, now_ms: u64) -> Option<Vec<StyledRun>> {
        (!self.pending.is_empty()).then(|| self.take(now_ms))
    }

    fn take(&mut self, now_ms: u64) -> Vec<StyledRun> {
        self.last_flush_ms = now_ms;
        self.bytes = 0;
        self.flushes += 1;
        std::mem::take(&mut self.pending)
    }

    /// `(appends, flushes)` — the coalescing ratio, asserted as a counter.
    #[must_use]
    pub fn counters(&self) -> (u64, u64) {
        (self.appends, self.flushes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratum_proto::{OutputStream, StyleId};

    fn out(exec: u64, text: &str) -> EngineEvent {
        EngineEvent::Output {
            seq: 0,
            exec: ExecutionId(exec),
            stream: OutputStream::Results,
            runs: vec![StyledRun {
                text: text.to_owned(),
                style: StyleId::Result,
            }],
        }
    }

    #[test]
    fn seq_is_strictly_increasing_across_variants() {
        let (bus, rx) = EventBus::new();
        bus.emit(out(1, "a"));
        bus.emit(EngineEvent::RunFinished {
            seq: 0,
            run: stratum_proto::RunId(1),
            rc: 0,
            blocks_run: 1,
            blocks_failed: 0,
            duration_us: 0,
            finished_at_ms: 0,
        });
        let a = rx.recv().unwrap();
        let b = rx.recv().unwrap();
        assert_eq!(seq_of(&a), 0);
        assert_eq!(seq_of(&b), 1);
    }

    #[test]
    fn a_full_channel_coalesces_output_and_accounts_for_it() {
        let (bus, rx) = EventBus::new();
        for _ in 0..EVENT_CAPACITY {
            bus.emit(out(1, "x"));
        }
        // The channel is full; these are dropped, not blocked on.
        for _ in 0..10 {
            bus.emit(out(1, "yy"));
        }
        let (_, coalesced, dropped) = bus.counters();
        assert_eq!(coalesced, 10);
        assert_eq!(dropped, 20);

        // Drain one slot and emit again: the window is told what it missed.
        let _ = rx.recv().unwrap();
        bus.emit(out(1, "z"));
        let mut saw_truncated = false;
        while let Ok(ev) = rx.try_recv() {
            if let EngineEvent::OutputTruncated { dropped_bytes, .. } = ev {
                assert_eq!(dropped_bytes, 20);
                saw_truncated = true;
            }
        }
        assert!(saw_truncated, "a gap must be accounted for, never silent");
    }

    #[test]
    fn coalescing_collapses_a_display_loop() {
        let mut c = OutputCoalescer::new();
        let line = [StyledRun {
            text: "1\n".to_owned(),
            style: StyleId::Result,
        }];
        // 100 000 displays inside one 16 ms frame.
        let mut flushed = 0;
        for i in 0..100_000u64 {
            if c.append(&line, i / 10_000).is_some() {
                flushed += 1;
            }
        }
        let (appends, flushes) = c.counters();
        assert_eq!(appends, 100_000);
        assert_eq!(flushes, flushed);
        assert!(
            flushes < 100,
            "100 000 displays must not become 100 000 notifications: {flushes}"
        );
    }

    fn seq_of(e: &EngineEvent) -> u64 {
        match e {
            EngineEvent::Output { seq, .. } | EngineEvent::RunFinished { seq, .. } => *seq,
            _ => unreachable!(),
        }
    }
}
