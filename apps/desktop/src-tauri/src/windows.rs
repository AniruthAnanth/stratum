//! Windows, sessions and the seq-ordered event fan-out (spec §26, plan W17).
//!
//! One [`SessionRegistry`] per process. It owns three things:
//!
//! 1. **The fan-out.** Every `EngineEvent` is encoded ONCE (`to_vec_named`, the
//!    wire form of CONTRACTS §10) and pushed to every subscribed window in the
//!    order the engine's reader produced it. Two windows on one session
//!    therefore observe identical `seq` sequences — not because they sort, but
//!    because there is exactly one producer and the fan-out happens under the
//!    hub's lock.
//! 2. **The late-joiner snapshot.** `session_subscribe` returns
//!    `SubscribeAck { from_seq, snapshot }` built from state this registry
//!    accumulated as events passed through it, and registers the channel under
//!    the same lock — so the first live event a joiner sees is the first event
//!    after its snapshot, with no gap and **no history replay through IPC**.
//! 3. **The webview-label ↔ session binding** the `stratum-asset://` handler
//!    checks: a webview cannot read another project's data (CONTRACTS §10.2).
//!
//! Result envelopes and raw classic text are also cached here — "result cards
//! are host state", the invariant that keeps them on screen when the engine
//! crashes (ARCHITECTURE §3), and the store `result_get` and the asset
//! handler's `result/{s}/{r}/raw` route serve from.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use stratum_proto::block::BlockMap;
use stratum_proto::complete::CompletionEnv;
use stratum_proto::engine::{EngineEvent, EngineHealth, SessionMode};
use stratum_proto::ids::{BlockId, DocumentId, ResultId, SessionEpoch, SessionId};
use stratum_proto::result::ResultEnvelope;
use stratum_proto::session::{SessionSnapshot, SessionStatus};
use stratum_proto::status::BlockStatus;
use tauri::ipc::{Channel, InvokeResponseBody};

use crate::transport::event_seq;

/// ADR-017 counters. Asserted by tests; durations are never asserted.
#[derive(Default, Debug)]
pub struct FanoutCounters {
    /// Engine events fanned out (one per event per subscriber).
    pub deliveries: AtomicU64,
    /// Engine events encoded. One per event, regardless of window count —
    /// the "encode once" property as a number.
    pub encodes: AtomicU64,
    /// Snapshots served to late joiners.
    pub snapshots: AtomicU64,
    /// Events replayed through IPC to a late joiner. The contract says this
    /// stays ZERO — a joiner gets snapshot + tail, never history.
    pub replays: AtomicU64,
}

/// One subscribed window.
struct Subscriber {
    label: String,
    chan: Channel<InvokeResponseBody>,
}

/// Everything the registry knows about one session.
struct Hub {
    subscribers: Vec<Subscriber>,
    /// Webview labels allowed to read this session's assets.
    labels: Vec<String>,
    last_seq: u64,
    status: SessionStatus,
    docs: HashMap<DocumentId, BlockMap>,
    statuses: HashMap<DocumentId, Vec<(BlockId, BlockStatus)>>,
    recent_results: Vec<(BlockId, ResultId)>,
    completion_env: Option<CompletionEnv>,
    log_lines: u64,
    envelopes: HashMap<ResultId, ResultEnvelope>,
    /// Result → full classic text, accumulated from `Output` events.
    raw_text: HashMap<ResultId, String>,
    /// Execution → classic text so far (moved into `raw_text` on finish).
    exec_out: HashMap<u64, String>,
}

impl Hub {
    fn new(session: SessionId, epoch: SessionEpoch, mode: SessionMode) -> Self {
        Self {
            subscribers: Vec::new(),
            labels: Vec::new(),
            last_seq: 0,
            status: SessionStatus {
                session,
                epoch,
                health: EngineHealth::Ready,
                current: None,
                queued: 0,
                state: stratum_proto::ids::StateId(0),
                dataset_state: stratum_proto::ids::DatasetStateId(0),
                frame: "default".to_owned(),
                n_obs: 0,
                n_vars: 0,
                mode,
            },
            docs: HashMap::new(),
            statuses: HashMap::new(),
            recent_results: Vec::new(),
            completion_env: None,
            log_lines: 0,
            envelopes: HashMap::new(),
            raw_text: HashMap::new(),
            exec_out: HashMap::new(),
        }
    }

    fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            status: self.status.clone(),
            docs: self.docs.values().cloned().collect(),
            statuses: self
                .statuses
                .iter()
                .map(|(doc, s)| (*doc, s.clone()))
                .collect(),
            recent_results: self.recent_results.clone(),
            completion_env: self.completion_env.clone().unwrap_or_default(),
            log_lines: self.log_lines,
            from_seq: self.last_seq,
        }
    }

    /// Fold one engine event into the snapshot state.
    fn absorb(&mut self, ev: &EngineEvent) {
        let seq = event_seq(ev);
        if seq > 0 {
            self.last_seq = seq;
        }
        match ev {
            EngineEvent::BlockMapChanged { map, .. } => {
                self.docs.insert(map.doc, map.clone());
            }
            EngineEvent::StatusChanged { doc, changed, .. } => {
                let entry = self.statuses.entry(*doc).or_default();
                for (block, status) in changed {
                    if let Some(slot) = entry.iter_mut().find(|(b, _)| b == block) {
                        slot.1 = status.clone();
                    } else {
                        entry.push((*block, status.clone()));
                    }
                }
            }
            EngineEvent::Output { exec, runs, .. } => {
                let text = stratum_proto::styled::to_plain(runs);
                self.log_lines += text.matches('\n').count() as u64;
                self.exec_out.entry(exec.0).or_default().push_str(&text);
            }
            EngineEvent::Result { envelope, .. } => {
                let result = envelope.result;
                if let Some(block) = envelope.block {
                    self.recent_results.push((block, result));
                }
                // Until the execution finishes, the head is the best raw text.
                self.raw_text
                    .entry(result)
                    .or_insert_with(|| envelope.raw.head.clone());
                self.envelopes.insert(result, envelope.clone());
            }
            EngineEvent::BlockStarted { exec, .. } => {
                self.status.current = Some(*exec);
            }
            EngineEvent::BlockFinished {
                exec,
                result,
                dataset_state_out,
                ..
            } => {
                self.status.current = None;
                self.status.dataset_state = *dataset_state_out;
                if let (Some(result), Some(full)) = (result, self.exec_out.remove(&exec.0)) {
                    if !full.is_empty() {
                        self.raw_text.insert(*result, full);
                    }
                }
            }
            EngineEvent::StateChanged {
                dataset_state,
                state,
                frame,
                n_obs,
                n_vars,
                ..
            } => {
                self.status.dataset_state = *dataset_state;
                self.status.state = *state;
                self.status.frame = frame.clone();
                self.status.n_obs = *n_obs;
                self.status.n_vars = *n_vars;
            }
            EngineEvent::RunStarted { plan_len, .. } => {
                self.status.queued = *plan_len;
            }
            EngineEvent::RunFinished { .. } => {
                self.status.queued = 0;
                self.status.current = None;
            }
            EngineEvent::EngineHealth { health, .. } => {
                self.status.health = health.clone();
            }
            _ => {}
        }
    }
}

/// `session_subscribe`'s reply (CONTRACTS §11).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeAck {
    pub from_seq: u64,
    pub snapshot: SessionSnapshot,
}

/// Host → webview events that are not engine events (CONTRACTS §11). Encoded
/// with `to_vec_named` onto the same channel; the `event` tag namespace is
/// disjoint from `EngineEvent`'s.
#[derive(Clone, PartialEq, Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum HostEvent {
    DocumentSaved { doc: DocumentId, path: String },
    // `DocumentChangedOnDisk` (CONTRACTS §11) is deliberately absent until the
    // host grows a file watcher: an event variant nothing can ever emit is a
    // promise the wire cannot keep.
    LayoutChanged { id: String },
    SettingsChanged,
    EngineHealth { health: EngineHealth },
    Heartbeat { n: u64 },
}

/// The process-wide session/window registry.
#[derive(Default)]
pub struct SessionRegistry {
    hubs: Mutex<HashMap<SessionId, Hub>>,
    pub counters: FanoutCounters,
}

impl SessionRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create (or refresh) the hub for a session the engine just opened.
    pub fn open(&self, session: SessionId, epoch: SessionEpoch, mode: SessionMode) {
        let mut hubs = self.hubs.lock().expect("session registry poisoned");
        hubs.entry(session)
            .or_insert_with(|| Hub::new(session, epoch, mode));
    }

    pub fn close(&self, session: SessionId) {
        self.hubs
            .lock()
            .expect("session registry poisoned")
            .remove(&session);
    }

    /// The one session, when exactly one is open — the frontend omits
    /// `session` on several commands (`variables_list { frame }`).
    #[must_use]
    pub fn only_session(&self) -> Option<SessionId> {
        let hubs = self.hubs.lock().expect("session registry poisoned");
        let mut it = hubs.keys();
        match (it.next(), it.next()) {
            (Some(s), None) => Some(*s),
            _ => hubs.keys().min().copied(),
        }
    }

    /// Bind a webview label to a session. The asset handler refuses a request
    /// whose label is not bound to the session in its URL.
    pub fn bind_label(&self, session: SessionId, label: &str) {
        let mut hubs = self.hubs.lock().expect("session registry poisoned");
        if let Some(hub) = hubs.get_mut(&session) {
            if !hub.labels.iter().any(|l| l == label) {
                hub.labels.push(label.to_owned());
            }
        }
    }

    #[must_use]
    pub fn label_bound(&self, session: SessionId, label: &str) -> bool {
        let hubs = self.hubs.lock().expect("session registry poisoned");
        hubs.get(&session)
            .is_some_and(|hub| hub.labels.iter().any(|l| l == label))
    }

    /// Subscribe a window. Snapshot construction and channel registration
    /// happen under ONE lock, which is the no-gap guarantee: nothing can fan
    /// out between the snapshot's `from_seq` and the registration.
    pub fn subscribe(
        &self,
        session: SessionId,
        label: &str,
        chan: Channel<InvokeResponseBody>,
    ) -> Option<SubscribeAck> {
        let mut hubs = self.hubs.lock().expect("session registry poisoned");
        let hub = hubs.get_mut(&session)?;
        let snapshot = hub.snapshot();
        let from_seq = snapshot.from_seq;
        hub.subscribers.push(Subscriber {
            label: label.to_owned(),
            chan,
        });
        if !hub.labels.iter().any(|l| l == label) {
            hub.labels.push(label.to_owned());
        }
        self.counters.snapshots.fetch_add(1, Ordering::Relaxed);
        Some(SubscribeAck { from_seq, snapshot })
    }

    /// Remove a window's subscriptions (windows going away).
    pub fn unsubscribe_label(&self, label: &str) {
        let mut hubs = self.hubs.lock().expect("session registry poisoned");
        for hub in hubs.values_mut() {
            hub.subscribers.retain(|s| s.label != label);
        }
    }

    /// Fan one engine event out to every window on the session, in order.
    pub fn apply_engine_event(&self, session: SessionId, ev: &EngineEvent) {
        let bytes = match rmp_serde::to_vec_named(ev) {
            Ok(b) => b,
            Err(_) => return, // an unencodable event is a proto bug, not a UI crash
        };
        self.counters.encodes.fetch_add(1, Ordering::Relaxed);
        let mut hubs = self.hubs.lock().expect("session registry poisoned");
        let Some(hub) = hubs.get_mut(&session) else {
            return;
        };
        hub.absorb(ev);
        Self::deliver(&self.counters, hub, &bytes);
    }

    /// Fan a host-level event out (CONTRACTS §11's second list).
    pub fn host_event(&self, session: SessionId, ev: &HostEvent) {
        let Ok(bytes) = rmp_serde::to_vec_named(ev) else {
            return;
        };
        let mut hubs = self.hubs.lock().expect("session registry poisoned");
        let Some(hub) = hubs.get_mut(&session) else {
            return;
        };
        Self::deliver(&self.counters, hub, &bytes);
    }

    /// Broadcast a host event to every session (engine health, heartbeat).
    pub fn host_event_all(&self, ev: &HostEvent) {
        let Ok(bytes) = rmp_serde::to_vec_named(ev) else {
            return;
        };
        let mut hubs = self.hubs.lock().expect("session registry poisoned");
        for hub in hubs.values_mut() {
            Self::deliver(&self.counters, hub, &bytes);
        }
    }

    fn deliver(counters: &FanoutCounters, hub: &mut Hub, bytes: &[u8]) {
        hub.subscribers.retain(|sub| {
            let ok = sub
                .chan
                .send(InvokeResponseBody::Raw(bytes.to_vec()))
                .is_ok();
            if ok {
                counters.deliveries.fetch_add(1, Ordering::Relaxed);
            }
            ok
        });
    }

    /// A cached envelope, for `result_get`.
    #[must_use]
    pub fn envelope(&self, session: SessionId, result: ResultId) -> Option<ResultEnvelope> {
        let hubs = self.hubs.lock().expect("session registry poisoned");
        hubs.get(&session)?.envelopes.get(&result).cloned()
    }

    /// Cached raw classic text, for the `result/{s}/{r}/raw` asset route.
    #[must_use]
    pub fn raw_text(&self, session: SessionId, result: ResultId) -> Option<String> {
        let hubs = self.hubs.lock().expect("session registry poisoned");
        hubs.get(&session)?.raw_text.get(&result).cloned()
    }

    /// Current status (updated from events), for `session_status`.
    #[must_use]
    pub fn status(&self, session: SessionId) -> Option<SessionStatus> {
        let hubs = self.hubs.lock().expect("session registry poisoned");
        hubs.get(&session).map(|hub| hub.status.clone())
    }

    /// Record engine health into every hub's status (the pump calls this
    /// alongside the `HostEvent::EngineHealth` fan-out).
    pub fn set_health(&self, health: &EngineHealth) {
        let mut hubs = self.hubs.lock().expect("session registry poisoned");
        for hub in hubs.values_mut() {
            hub.status.health = health.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn collecting_channel() -> (Channel<InvokeResponseBody>, Arc<Mutex<Vec<Vec<u8>>>>) {
        let seen: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let chan = Channel::new(move |body| {
            if let InvokeResponseBody::Raw(bytes) = body {
                sink.lock().expect("sink").push(bytes);
            }
            Ok(())
        });
        (chan, seen)
    }

    fn seqs(frames: &[Vec<u8>]) -> Vec<u64> {
        frames
            .iter()
            .filter_map(|bytes| {
                let ev: EngineEvent = rmp_serde::from_slice(bytes).ok()?;
                Some(event_seq(&ev))
            })
            .collect()
    }

    fn event(seq: u64) -> EngineEvent {
        EngineEvent::StatusChanged {
            seq,
            doc: DocumentId(1),
            changed: Vec::new(),
        }
    }

    const S: SessionId = SessionId(1);

    fn registry() -> SessionRegistry {
        let r = SessionRegistry::new();
        r.open(S, SessionEpoch(1), SessionMode::Interactive);
        r
    }

    /// Acceptance: two windows on one session both receive every `EngineEvent`
    /// in the same `seq` order.
    #[test]
    fn two_windows_observe_the_same_seq_order() {
        let r = registry();
        let (a_chan, a_seen) = collecting_channel();
        let (b_chan, b_seen) = collecting_channel();
        r.subscribe(S, "main", a_chan).expect("hub exists");
        r.subscribe(S, "p1:pane:1", b_chan).expect("hub exists");

        for seq in 1..=64 {
            r.apply_engine_event(S, &event(seq));
        }

        let a = seqs(&a_seen.lock().expect("a"));
        let b = seqs(&b_seen.lock().expect("b"));
        assert_eq!(a, (1..=64).collect::<Vec<_>>());
        assert_eq!(a, b, "both windows must observe one order");
        // Encode once per event, regardless of window count (ADR-017 counter).
        assert_eq!(r.counters.encodes.load(Ordering::Relaxed), 64);
        assert_eq!(r.counters.deliveries.load(Ordering::Relaxed), 128);
    }

    /// Acceptance: a late joiner gets snapshot + tail with NO history replay
    /// through IPC — its first live event is the first event after its
    /// snapshot's `from_seq`.
    #[test]
    fn a_late_joiner_gets_snapshot_plus_tail_and_no_replay() {
        let r = registry();
        let (early_chan, _early_seen) = collecting_channel();
        r.subscribe(S, "main", early_chan).expect("hub");

        for seq in 1..=10 {
            r.apply_engine_event(S, &event(seq));
        }

        let (late_chan, late_seen) = collecting_channel();
        let ack = r.subscribe(S, "p1:data:1", late_chan).expect("hub");
        assert_eq!(ack.from_seq, 10, "the snapshot names where the tail starts");

        for seq in 11..=13 {
            r.apply_engine_event(S, &event(seq));
        }

        let late = seqs(&late_seen.lock().expect("late"));
        assert_eq!(late, vec![11, 12, 13], "tail only — no gap, no replay");
        assert_eq!(
            r.counters.replays.load(Ordering::Relaxed),
            0,
            "history is never replayed through IPC"
        );
    }

    /// The snapshot accumulates result envelopes and raw text — host state
    /// that must survive an engine crash (result cards stay on screen).
    #[test]
    fn results_are_host_state_and_survive_health_transitions() {
        use camino::Utf8PathBuf;
        use stratum_proto::result::{AssetRef, LayoutHint, RawRef, ResultEnvelope};

        let r = registry();
        let envelope = ResultEnvelope {
            result: ResultId(41),
            revision: 0,
            exec: stratum_proto::ids::ExecutionId(7),
            block: Some(BlockId(2)),
            dataset_state: stratum_proto::ids::DatasetStateId(1),
            code_hash: stratum_proto::ids::CodeHash([0; 16]),
            cmdline: "summarize price".to_owned(),
            started_at_ms: 0,
            duration_us: 1,
            rc: 0,
            payloads: Vec::new(),
            raw: RawRef {
                bytes: 5,
                lines: 1,
                head: "hello".to_owned(),
                truncated: false,
                asset: AssetRef {
                    path: "result/1/41/raw".to_owned(),
                    mime: "text/plain; charset=utf-8".to_owned(),
                    bytes: 5,
                },
            },
            layout_hint: LayoutHint::default(),
            actions: Vec::new(),
        };
        r.apply_engine_event(
            S,
            &EngineEvent::Result {
                seq: 1,
                exec: stratum_proto::ids::ExecutionId(7),
                envelope: envelope.clone(),
            },
        );

        // The engine dies. The envelope and the raw text are still served.
        r.set_health(&EngineHealth::Crashed {
            signal: Some(9),
            last_statement: None,
            log_tail: String::new(),
        });
        assert_eq!(r.envelope(S, ResultId(41)), Some(envelope));
        assert_eq!(r.raw_text(S, ResultId(41)).as_deref(), Some("hello"));
        let _ = Utf8PathBuf::new(); // keep the import shape stable
    }

    /// The label ↔ session binding the asset handler enforces.
    #[test]
    fn labels_bind_per_session_not_globally() {
        let r = registry();
        r.open(SessionId(2), SessionEpoch(1), SessionMode::Interactive);
        r.bind_label(S, "p1:main");
        assert!(r.label_bound(S, "p1:main"));
        assert!(!r.label_bound(SessionId(2), "p1:main"));
        assert!(!r.label_bound(S, "p2:main"));
    }
}
