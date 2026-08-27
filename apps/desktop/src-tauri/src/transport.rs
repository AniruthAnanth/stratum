//! CONTRACTS.md §10 — the desktop half of the framed-MessagePack transport, and
//! the read-only view onto the engine's mmap bulk segments.
//!
//! Deliberately **Tauri-free**. Nothing here knows about windows, webviews or
//! commands: `main.rs`/`ipc.rs` (W17) adapt [`Transport::subscribe`] to
//! `emit_to`, and `asset.rs` (W17) adapts [`BulkSegments::resolve`] to the
//! `stratum-asset://` handler. Keeping the boundary here is what lets W07's
//! tests drive the real transport over a `tokio::io::duplex` pipe with no app.
//!
//! Two tasks per engine, exactly as ARCHITECTURE §4 specifies: a reader that
//! decodes frames, stamps sequence bookkeeping and publishes to a broadcast, and
//! a writer that serialises requests onto the pipe. They share nothing but
//! channels.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Serialize;
use stratum_proto::engine::{BulkRef, EngineEvent, EngineRequest, EngineResponse, STREAM_SCHEMA};
use stratum_proto::frame::{
    encode_frame, Frame, FrameError, FrameKind, FrameReader, Ping, CORR_UNSOLICITED,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;

/// Broadcast depth. §7's `OutputTruncated` contract is written against exactly
/// this number: "a window >256 frames behind gets this instead".
pub const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Read size for the engine pipe. One `Output` event is coalesced at 64 KB
/// (§7), so this is a whole event's worth per syscall in the common case.
const READ_CHUNK: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("framing: {0}")]
    Frame(#[from] FrameError),
    #[error("encoding a {what}: {source}")]
    Encode {
        what: &'static str,
        #[source]
        source: rmp_serde::encode::Error,
    },
    #[error("decoding a {what}: {source}")]
    Decode {
        what: &'static str,
        #[source]
        source: rmp_serde::decode::Error,
    },
    #[error("engine i/o: {0}")]
    Io(#[from] std::io::Error),
    /// The engine went away with the request still outstanding. The supervisor
    /// turns this into `EngineHealth::Crashed`.
    #[error("engine closed the connection")]
    Closed,
    #[error("schema mismatch: engine {engine}, client {client}")]
    SchemaMismatch { engine: u32, client: u32 },
    #[error("engine answered a request with an event")]
    UnexpectedKind,
}

/// `rmp_serde::to_vec_named`, and nowhere else in the desktop.
///
/// §10 makes field-name encoding mandatory: a positional encoding turns any
/// struct change into a silent wire break, and we will one day run a
/// version-skewed engine against this build.
pub fn encode_body<T: Serialize>(what: &'static str, value: &T) -> Result<Vec<u8>, TransportError> {
    rmp_serde::to_vec_named(value).map_err(|source| TransportError::Encode { what, source })
}

fn decode_body<T: DeserializeOwned>(what: &'static str, bytes: &[u8]) -> Result<T, TransportError> {
    rmp_serde::from_slice(bytes).map_err(|source| TransportError::Decode { what, source })
}

/// What the reader task observed. Read by the supervisor for its health model;
/// `gaps` is the number the UI's "re-snapshot rather than diverge" rule keys on.
#[derive(Debug, Default)]
pub struct TransportStats {
    pub frames_in: AtomicU64,
    pub frames_out: AtomicU64,
    pub events: AtomicU64,
    /// Highest `seq` seen. §7: strictly increasing per session, stamped before
    /// fan-out.
    pub last_seq: AtomicU64,
    /// Times a `seq` arrived that was not `last_seq + 1`.
    pub seq_gaps: AtomicU64,
    /// Events published while no window was subscribed — normal during
    /// startup and after the last window closes, and a bug at any other time.
    pub dropped_no_subscriber: AtomicU64,
}

/// The desktop's handle on one engine connection.
///
/// Cloneable and `Send`: every window shares one of these. Requests are
/// pipelined — §7.1's `corr` is what lets responses come back out of order.
#[derive(Clone)]
pub struct Transport {
    tx: mpsc::UnboundedSender<Outgoing>,
    events: broadcast::Sender<Arc<EngineEvent>>,
    pending: Arc<Mutex<HashMap<u32, oneshot::Sender<EngineResponse>>>>,
    next_corr: Arc<AtomicU32>,
    stats: Arc<TransportStats>,
}

enum Outgoing {
    Frame(Vec<u8>),
    Shutdown,
}

/// The two tasks, so a supervisor can await or abort them.
pub struct TransportTasks {
    pub reader: JoinHandle<Result<(), TransportError>>,
    pub writer: JoinHandle<Result<(), TransportError>>,
}

impl Transport {
    /// Wire a transport onto a duplex pair — a child's stdin/stdout, or a
    /// `tokio::io::duplex` when the peer is the in-process mock. The mock path
    /// exists precisely so that `--mock` exercises this code and not a stub.
    pub fn spawn<R, W>(reader: R, writer: W) -> (Self, TransportTasks)
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (tx, rx) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let pending: Arc<Mutex<HashMap<u32, oneshot::Sender<EngineResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let stats = Arc::new(TransportStats::default());

        let me = Self {
            tx,
            events,
            pending,
            next_corr: Arc::new(AtomicU32::new(1)),
            stats,
        };
        let tasks = TransportTasks {
            reader: tokio::spawn(me.clone().read_loop(reader)),
            writer: tokio::spawn(me.clone().write_loop(writer, rx)),
        };
        (me, tasks)
    }

    #[must_use]
    pub fn stats(&self) -> Arc<TransportStats> {
        Arc::clone(&self.stats)
    }

    /// Every window subscribes; the payload is an `Arc`, so fan-out to N
    /// windows clones a pointer and not a `BlockMapChanged`.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<EngineEvent>> {
        self.events.subscribe()
    }

    /// Send a request and await its response. Cancelling this future drops the
    /// pending slot; a late response is then discarded by the reader rather
    /// than delivered to the wrong caller.
    pub async fn request(&self, req: EngineRequest) -> Result<EngineResponse, TransportError> {
        // `corr` 0 means "unsolicited" (§10), so the counter never yields it.
        let corr = loop {
            let c = self.next_corr.fetch_add(1, Ordering::Relaxed);
            if c != CORR_UNSOLICITED {
                break c;
            }
        };
        let body = encode_body("EngineRequest", &req)?;
        let (done, wait) = oneshot::channel();
        self.pending.lock().await.insert(corr, done);
        if let Err(e) = self.send_frame(FrameKind::Request, corr, &body) {
            self.pending.lock().await.remove(&corr);
            return Err(e);
        }
        wait.await.map_err(|_| TransportError::Closed)
    }

    /// Fire-and-forget. `DocChange` is the one §7 marks as answered by an event
    /// rather than a response; sending it through [`Self::request`] would leave
    /// a pending slot that nothing ever fills.
    pub fn notify(&self, req: EngineRequest) -> Result<(), TransportError> {
        let body = encode_body("EngineRequest", &req)?;
        self.send_frame(FrameKind::Request, CORR_UNSOLICITED, &body)
    }

    /// Liveness probe (ARCHITECTURE §3's 5 s heartbeat).
    pub fn ping(&self, nonce: u64) -> Result<(), TransportError> {
        let body = encode_body("Ping", &Ping { nonce, pong: false })?;
        self.send_frame(FrameKind::Ping, CORR_UNSOLICITED, &body)
    }

    /// Close the writer. The engine sees EOF on stdin and exits; the supervisor
    /// still reaps and still kills the process group if it does not.
    pub fn close(&self) {
        let _ = self.tx.send(Outgoing::Shutdown);
    }

    fn send_frame(&self, kind: FrameKind, corr: u32, body: &[u8]) -> Result<(), TransportError> {
        let mut buf = Vec::with_capacity(body.len() + 16);
        encode_frame(kind, corr, body, &mut buf)?;
        self.tx
            .send(Outgoing::Frame(buf))
            .map_err(|_| TransportError::Closed)
    }

    async fn read_loop<R>(self, mut src: R) -> Result<(), TransportError>
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let mut reader = FrameReader::new();
        let mut chunk = vec![0_u8; READ_CHUNK];
        loop {
            let n = src.read(&mut chunk).await?;
            if n == 0 {
                // EOF. A partial frame here is a crash, not a clean exit, and
                // the supervisor must be able to tell the difference.
                let clean = reader.end_of_stream();
                self.fail_all_pending().await;
                return clean.map_err(TransportError::from);
            }
            reader.feed(&chunk[..n]);
            while let Some(frame) = reader.next_frame()? {
                self.stats.frames_in.fetch_add(1, Ordering::Relaxed);
                self.dispatch(frame).await?;
            }
        }
    }

    async fn dispatch(&self, frame: Frame) -> Result<(), TransportError> {
        match frame.kind {
            FrameKind::Response => {
                let resp: EngineResponse = decode_body("EngineResponse", &frame.payload)?;
                if let Some(slot) = self.pending.lock().await.remove(&frame.corr) {
                    // A dropped receiver means the caller gave up; discarding is
                    // correct and must not tear down the connection.
                    let _ = slot.send(resp);
                }
            }
            FrameKind::Event => {
                let event: EngineEvent = decode_body("EngineEvent", &frame.payload)?;
                self.track_seq(&event);
                self.stats.events.fetch_add(1, Ordering::Relaxed);
                // Err means "no subscribers", which is normal at startup and
                // after the last window closes.
                if self.events.send(Arc::new(event)).is_err() {
                    self.stats
                        .dropped_no_subscriber
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
            FrameKind::Ping => {
                let ping: Ping = decode_body("Ping", &frame.payload)?;
                if !ping.pong {
                    let body = encode_body(
                        "Ping",
                        &Ping {
                            nonce: ping.nonce,
                            pong: true,
                        },
                    )?;
                    self.send_frame(FrameKind::Ping, frame.corr, &body)?;
                }
            }
            // The desktop is a client. An engine that sends us a request is
            // desynced or is not an engine.
            FrameKind::Request => return Err(TransportError::UnexpectedKind),
        }
        Ok(())
    }

    /// §7 guarantee 5: `seq` is strictly increasing per session. A gap means a
    /// window must re-snapshot rather than apply a partial history, so it is
    /// counted rather than ignored.
    fn track_seq(&self, event: &EngineEvent) {
        let seq = event_seq(event);
        let prev = self.stats.last_seq.swap(seq, Ordering::Relaxed);
        if prev != 0 && seq != prev + 1 {
            self.stats.seq_gaps.fetch_add(1, Ordering::Relaxed);
        }
    }

    async fn fail_all_pending(&self) {
        self.pending.lock().await.clear();
    }

    async fn write_loop<W>(
        self,
        mut sink: W,
        mut rx: mpsc::UnboundedReceiver<Outgoing>,
    ) -> Result<(), TransportError>
    where
        W: AsyncWrite + Unpin + Send + 'static,
    {
        while let Some(msg) = rx.recv().await {
            match msg {
                Outgoing::Frame(bytes) => {
                    sink.write_all(&bytes).await?;
                    // Flush per frame on purpose: a buffered `ExecCancel` that
                    // waits for the next write is a cancel ladder that fails its
                    // 50 ms budget for reasons the user cannot see.
                    sink.flush().await?;
                    self.stats.frames_out.fetch_add(1, Ordering::Relaxed);
                }
                Outgoing::Shutdown => break,
            }
        }
        sink.flush().await?;
        Ok(())
    }
}

/// `seq` off any event. §7 puts it on every variant; this is the one place that
/// has to know that.
#[must_use]
pub fn event_seq(event: &EngineEvent) -> u64 {
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
        | EngineEvent::EngineHealth { seq, .. } => *seq,
    }
}

/// The client half of the §7 `Hello` handshake. Called once per connection,
/// before anything else is sent.
pub async fn handshake(tx: &Transport, client: &str) -> Result<String, TransportError> {
    let resp = tx
        .request(EngineRequest::Hello {
            client: client.to_owned(),
            schema: STREAM_SCHEMA,
        })
        .await?;
    match resp {
        EngineResponse::Hello { engine, schema, .. } if schema == STREAM_SCHEMA => Ok(engine),
        EngineResponse::Hello { schema, .. } => Err(TransportError::SchemaMismatch {
            engine: schema,
            client: STREAM_SCHEMA,
        }),
        _ => Err(TransportError::UnexpectedKind),
    }
}

// ---------------------------------------------------------------------------
// Bulk — §10's mmap segment ring, read-only on this side
// ---------------------------------------------------------------------------

/// How many bytes were copied, and where.
///
/// §10 budgets **two** copies on the bulk path: engine builder → mmap, and
/// mmap → webview response body. The counters make that budget a test rather
/// than a comment (W07 acceptance: "asserted by instrumentation").
#[derive(Debug, Default)]
pub struct BulkCopyLedger {
    /// Copies made by the engine writing a payload into its segment.
    pub engine_to_mmap: AtomicU64,
    /// Copies made by the desktop turning a mapped slice into a response body.
    pub mmap_to_response: AtomicU64,
    pub bytes_engine_to_mmap: AtomicU64,
    pub bytes_mmap_to_response: AtomicU64,
}

impl BulkCopyLedger {
    pub fn record_engine_to_mmap(&self, bytes: u64) {
        self.engine_to_mmap.fetch_add(1, Ordering::Relaxed);
        self.bytes_engine_to_mmap
            .fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_mmap_to_response(&self, bytes: u64) {
        self.mmap_to_response.fetch_add(1, Ordering::Relaxed);
        self.bytes_mmap_to_response
            .fetch_add(bytes, Ordering::Relaxed);
    }

    #[must_use]
    pub fn total_copies(&self) -> u64 {
        self.engine_to_mmap.load(Ordering::Relaxed) + self.mmap_to_response.load(Ordering::Relaxed)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BulkError {
    #[error("segment {0} is not attached")]
    UnknownSegment(u32),
    #[error("segment {segment} is at epoch {have}, the reference wants {want}")]
    StaleEpoch { segment: u32, have: u64, want: u64 },
    #[error("bulk ref [{offset}, {end}) is outside segment {segment} of {len} bytes")]
    OutOfBounds {
        segment: u32,
        offset: u64,
        end: u64,
        len: u64,
    },
    #[error("mapping segment {segment}: {source}")]
    Map {
        segment: u32,
        #[source]
        source: std::io::Error,
    },
}

struct Segment {
    map: memmap2::Mmap,
    epoch: u64,
}

/// The desktop's read-only view of the engine's bulk segments.
///
/// **A gap in §10, reported by W07.** §10 says the engine creates segment files
/// in the temp dir and `unlink`s them *immediately*; it also says the desktop
/// maps them. Both cannot be true over a stdio transport — an unlinked path
/// cannot be opened, and stdio carries no file descriptor. This implements the
/// only ordering that works: the engine creates the file, the desktop
/// [`attach`](Self::attach)es it, and the engine unlinks once the segment is
/// attached (it stays mapped, so the bytes survive the unlink). The window
/// where the path exists is the one thing that must be closed by the engine
/// promptly, not by this side.
#[derive(Default)]
pub struct BulkSegments {
    segments: std::sync::Mutex<HashMap<u32, Arc<Segment>>>,
    pub ledger: Arc<BulkCopyLedger>,
}

impl BulkSegments {
    #[must_use]
    pub fn new(ledger: Arc<BulkCopyLedger>) -> Self {
        Self {
            segments: std::sync::Mutex::new(HashMap::new()),
            ledger,
        }
    }

    /// The naming convention the engine and the desktop share. Not a wire type:
    /// `BulkRef` carries only the segment number, so both sides derive the path.
    #[must_use]
    pub fn segment_path(dir: &std::path::Path, session: u32, segment: u32) -> std::path::PathBuf {
        dir.join(format!("stratum-s{session}-seg{segment}.bulk"))
    }

    /// Map a segment read-only. Safety of `Mmap::map` is the usual mmap
    /// contract: the file must not be truncated under us, which is why the
    /// engine only ever appends within a segment and retires whole segments by
    /// bumping `epoch`.
    pub fn attach(
        &self,
        segment: u32,
        path: &std::path::Path,
        epoch: u64,
    ) -> Result<(), BulkError> {
        let file =
            std::fs::File::open(path).map_err(|source| BulkError::Map { segment, source })?;
        // SAFETY: read-only mapping of a file the engine appends to and never
        // truncates; retirement bumps `epoch` and attaches a new file.
        let map = unsafe { memmap2::Mmap::map(&file) }
            .map_err(|source| BulkError::Map { segment, source })?;
        self.segments
            .lock()
            .expect("bulk segment table poisoned")
            .insert(segment, Arc::new(Segment { map, epoch }));
        Ok(())
    }

    pub fn detach(&self, segment: u32) {
        self.segments
            .lock()
            .expect("bulk segment table poisoned")
            .remove(&segment);
    }

    /// Resolve a `BulkRef` to a mapped slice. **No copy happens here** — that is
    /// the point of the segment ring.
    pub fn resolve(&self, r: &BulkRef) -> Result<BulkSlice, BulkError> {
        let seg = self
            .segments
            .lock()
            .expect("bulk segment table poisoned")
            .get(&r.segment)
            .map(Arc::clone)
            .ok_or(BulkError::UnknownSegment(r.segment))?;
        if seg.epoch != r.epoch {
            return Err(BulkError::StaleEpoch {
                segment: r.segment,
                have: seg.epoch,
                want: r.epoch,
            });
        }
        let end = r.offset.saturating_add(r.len);
        let len = seg.map.len() as u64;
        if end > len {
            return Err(BulkError::OutOfBounds {
                segment: r.segment,
                offset: r.offset,
                end,
                len,
            });
        }
        Ok(BulkSlice {
            seg,
            offset: r.offset as usize,
            len: r.len as usize,
            ledger: Arc::clone(&self.ledger),
        })
    }
}

/// A borrowed window into a mapped segment. Holds the mapping alive.
pub struct BulkSlice {
    seg: Arc<Segment>,
    offset: usize,
    len: usize,
    ledger: Arc<BulkCopyLedger>,
}

impl BulkSlice {
    /// Zero copies. What the asset handler streams from.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.seg.map[self.offset..self.offset + self.len]
    }

    /// The ONE copy the webview response body costs (§10's second copy). Named
    /// so that a second call site shows up in the ledger as a third copy and
    /// fails the budget test.
    #[must_use]
    pub fn into_response_body(self) -> Vec<u8> {
        self.ledger.record_mmap_to_response(self.len as u64);
        self.as_bytes().to_vec()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use stratum_proto::engine::EngineHealth;
    use stratum_proto::ids::{ExecutionId, SessionId};
    use tokio::io::{AsyncReadExt, DuplexStream};

    use super::*;

    fn health_event(seq: u64) -> EngineEvent {
        EngineEvent::EngineHealth {
            seq,
            health: EngineHealth::Ready,
        }
    }

    async fn write_frame<T: Serialize>(w: &mut DuplexStream, kind: FrameKind, corr: u32, body: &T) {
        let payload = encode_body("test", body).unwrap();
        let mut buf = Vec::new();
        encode_frame(kind, corr, &payload, &mut buf).unwrap();
        w.write_all(&buf).await.unwrap();
        w.flush().await.unwrap();
    }

    /// §7 guarantee 5 plus ARCHITECTURE §26: two windows on one session both see
    /// every event, in the same order, from one broadcast.
    #[tokio::test]
    async fn every_subscriber_sees_every_event_in_seq_order() {
        let (desktop, mut engine) = tokio::io::duplex(4096);
        let (rx_half, tx_half) = tokio::io::split(desktop);
        let (transport, _tasks) = Transport::spawn(rx_half, tx_half);
        let mut w1 = transport.subscribe();
        let mut w2 = transport.subscribe();

        for seq in 1..=5 {
            write_frame(
                &mut engine,
                FrameKind::Event,
                CORR_UNSOLICITED,
                &health_event(seq),
            )
            .await;
        }
        for window in [&mut w1, &mut w2] {
            for seq in 1..=5 {
                let ev = tokio::time::timeout(std::time::Duration::from_secs(2), window.recv())
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(event_seq(&ev), seq);
            }
        }
        assert_eq!(transport.stats().seq_gaps.load(Ordering::Relaxed), 0);
    }

    /// A gap is what tells a window to re-snapshot instead of applying a partial
    /// history, so it must be observed rather than smoothed over.
    #[tokio::test]
    async fn a_seq_gap_is_counted() {
        let (desktop, mut engine) = tokio::io::duplex(4096);
        let (rx_half, tx_half) = tokio::io::split(desktop);
        let (transport, _tasks) = Transport::spawn(rx_half, tx_half);
        let mut sub = transport.subscribe();
        for seq in [1_u64, 2, 7] {
            write_frame(
                &mut engine,
                FrameKind::Event,
                CORR_UNSOLICITED,
                &health_event(seq),
            )
            .await;
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), sub.recv())
                .await
                .unwrap()
                .unwrap();
        }
        assert_eq!(transport.stats().seq_gaps.load(Ordering::Relaxed), 1);
    }

    /// §7.1: "Requests may be pipelined; responses carry the request's `corr`
    /// and may arrive out of order."
    #[tokio::test]
    async fn pipelined_requests_are_matched_by_corr_out_of_order() {
        let (desktop, engine) = tokio::io::duplex(64 * 1024);
        let (rx_half, tx_half) = tokio::io::split(desktop);
        let (transport, _tasks) = Transport::spawn(rx_half, tx_half);

        // A peer that collects two requests and answers them in reverse.
        let peer = tokio::spawn(async move {
            let mut engine = engine;
            let mut reader = FrameReader::new();
            let mut corrs = Vec::new();
            let mut chunk = [0_u8; 4096];
            while corrs.len() < 2 {
                let n = engine.read(&mut chunk).await.unwrap();
                reader.feed(&chunk[..n]);
                while let Some(f) = reader.next_frame().unwrap() {
                    corrs.push(f.corr);
                }
            }
            for (i, corr) in corrs.iter().rev().enumerate() {
                let body = EngineResponse::Status {
                    status: stratum_proto::session::SessionStatus {
                        session: SessionId(1),
                        epoch: stratum_proto::ids::SessionEpoch(1),
                        health: EngineHealth::Busy {
                            exec: ExecutionId(i as u64),
                        },
                        current: None,
                        queued: 0,
                        state: stratum_proto::ids::StateId(1),
                        dataset_state: stratum_proto::ids::DatasetStateId(17),
                        frame: "default".to_owned(),
                        n_obs: 74,
                        n_vars: 12,
                        mode: stratum_proto::engine::SessionMode::Interactive,
                    },
                };
                write_frame(&mut engine, FrameKind::Response, *corr, &body).await;
            }
            corrs
        });

        let a = transport.request(EngineRequest::Status {
            session: SessionId(1),
        });
        let b = transport.request(EngineRequest::Status {
            session: SessionId(2),
        });
        let (ra, rb) = tokio::join!(a, b);
        // Both resolved, and neither got the other's answer: the second request
        // is answered FIRST by the peer, so a reader that matched by arrival
        // order would swap them.
        let corrs = peer.await.unwrap();
        assert_ne!(corrs[0], corrs[1]);
        for r in [ra.unwrap(), rb.unwrap()] {
            assert!(matches!(r, EngineResponse::Status { .. }));
        }
    }

    /// An engine killed mid-write must surface as a framing error, which is what
    /// the supervisor turns into `EngineHealth::Crashed`. Reporting EOF as a
    /// clean shutdown here is how a crash becomes a silent hang.
    #[tokio::test]
    async fn a_half_written_frame_at_eof_is_an_error() {
        let (desktop, mut engine) = tokio::io::duplex(4096);
        let (rx_half, tx_half) = tokio::io::split(desktop);
        let (_transport, tasks) = Transport::spawn(rx_half, tx_half);
        let payload = encode_body("test", &health_event(1)).unwrap();
        let mut buf = Vec::new();
        encode_frame(FrameKind::Event, CORR_UNSOLICITED, &payload, &mut buf).unwrap();
        buf.truncate(buf.len() - 3);
        engine.write_all(&buf).await.unwrap();
        engine.flush().await.unwrap();
        drop(engine);

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), tasks.reader)
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(
                outcome,
                Err(TransportError::Frame(
                    stratum_proto::frame::FrameError::Truncated { .. }
                ))
            ),
            "{outcome:?}"
        );
    }

    /// A clean EOF is not an error: the engine exited after `Shutdown`.
    #[tokio::test]
    async fn a_frame_boundary_eof_is_clean() {
        let (desktop, engine) = tokio::io::duplex(4096);
        let (rx_half, tx_half) = tokio::io::split(desktop);
        let (_transport, tasks) = Transport::spawn(rx_half, tx_half);
        drop(engine);
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), tasks.reader)
            .await
            .unwrap()
            .unwrap();
        assert!(outcome.is_ok(), "{outcome:?}");
    }
}
