//! CONTRACTS.md §10 — desktop ↔ engine framing — and §7.1 — the NDJSON envelope.
//!
//! W07's one file inside the frozen crate (`docs/ownership.toml`, the single
//! declared exception in the partition): the codec and the wire envelope are one
//! unit of work, and putting the frame header in a crate that cannot test it
//! against the types it frames is how the two drift apart.
//!
//! Two encodings, one type set. Everything here is **shape and byte layout
//! only** — no `rmp_serde`, no `serde_json`, no I/O — because this crate is
//! reachable from `stratum-parse` and must build for `wasm32-unknown-unknown`
//! (ARCHITECTURE §8.4). The serializers live at the two call sites that own a
//! transport: `stratum-cli`'s `serve` module and the desktop's `transport.rs`.
//!
//! # What a reader here guarantees
//!
//! * A malformed length prefix or an unknown `kind` byte **poisons** the reader.
//!   It does not attempt to resynchronise: a stdio pipe carries no self-syncing
//!   marker, so "skip a byte and try again" turns one bad frame into an
//!   arbitrarily long stream of plausible-looking garbage. The supervisor's
//!   answer to a framing error is to kill and respawn the engine, which is a
//!   decision it can only make if the error reaches it.
//! * A frame that is merely *incomplete* is `Ok(None)` — feed more bytes. The
//!   distinction between "not yet" and "never" is the whole job of this file.

use serde::{Deserialize, Serialize};

pub use crate::engine::BulkRef;
use crate::engine::STREAM_SCHEMA;

// ---------------------------------------------------------------------------
// §10 — framed MessagePack over the child's stdin/stdout pipes
// ---------------------------------------------------------------------------

/// Bytes of the little-endian `len` prefix, which is NOT counted by `len`.
pub const FRAME_LEN_PREFIX: usize = 4;

/// Bytes `len` counts before the payload starts: `kind:u8` + `corr:u32LE`.
pub const FRAME_HEADER_LEN: usize = 5;

/// The smallest legal `len`: a frame whose payload is empty.
pub const MIN_FRAME_LEN: u32 = FRAME_HEADER_LEN as u32;

/// Default ceiling on `len`, checked **before** any allocation.
///
/// Bulk never travels in a frame — `DataPage`, raw classic text and graph images
/// go through the mmap segment ring and arrive as a [`BulkRef`] (§10) — so a
/// control frame this large is a desynced or hostile peer, and a peer that can
/// make us allocate its choice of 4 GiB is a trivial denial of service.
pub const MAX_FRAME_LEN: u32 = 64 * 1024 * 1024;

/// `corr` on an unsolicited frame: every `EngineEvent`, and a `Ping` that is not
/// answering anything.
pub const CORR_UNSOLICITED: u32 = 0;

/// The `kind` byte. §10's table, transcribed.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum FrameKind {
    Request = 0,
    Response = 1,
    Event = 2,
    /// §10 spells this slot "Ping/Pong" — one kind, two directions. Which of the
    /// two a frame is lives in the payload ([`Ping::pong`]), because a fifth
    /// `kind` value would have been a wire change and §10 froze four.
    Ping = 3,
}

impl FrameKind {
    #[inline]
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// The only way a `kind` byte off the wire becomes a `FrameKind`.
    ///
    /// Deliberately NOT `TryFrom` with a permissive fallback: an unknown kind is
    /// not a forward-compatible extension point. §15's additive-only rule adds
    /// *variants to the enums inside `body`*, never new frame kinds — a reader
    /// that skipped an unknown kind would still have to trust the same peer's
    /// `len` to find the next frame.
    #[inline]
    pub const fn from_u8(b: u8) -> Result<Self, FrameError> {
        match b {
            0 => Ok(Self::Request),
            1 => Ok(Self::Response),
            2 => Ok(Self::Event),
            3 => Ok(Self::Ping),
            other => Err(FrameError::UnknownKind { kind: other }),
        }
    }
}

/// The payload of a [`FrameKind::Ping`] frame.
///
/// **Filling a gap, not deviating from one.** §10 names the kind and says
/// nothing about its payload; the supervisor's 5 s liveness check (ARCHITECTURE
/// §3) needs to tell its own ping's echo from a fresh ping by the peer, so the
/// payload is a nonce plus a direction bit. Reported in W07's return.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Ping {
    pub nonce: u64,
    /// `false` = ping, `true` = the echo of a ping with this `nonce`.
    pub pong: bool,
}

/// One decoded frame. The payload is owned: the reader's buffer is reused for
/// the bytes still in flight behind it, so a borrow would pin the whole stream.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Frame {
    pub kind: FrameKind,
    /// The request's correlation id; [`CORR_UNSOLICITED`] on events.
    pub corr: u32,
    pub payload: Vec<u8>,
}

/// Every way a frame stream can be wrong. `Clone` because a poisoned reader
/// hands the same error to every later call rather than inventing a new one.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame length {len} is below the {MIN_FRAME_LEN}-byte header")]
    Undersized { len: u32 },
    #[error("frame length {len} exceeds the {max}-byte limit")]
    Oversized { len: u32, max: u32 },
    #[error("unknown frame kind {kind}")]
    UnknownKind { kind: u8 },
    #[error("payload of {len} bytes exceeds the {max}-byte frame limit")]
    PayloadTooLarge { len: usize, max: u32 },
    #[error("stream ended {have} bytes into a frame that needs {need}")]
    Truncated { have: usize, need: usize },
    #[error("stream ended {have} bytes into a line with no terminator")]
    TruncatedLine { have: usize },
    #[error("line of {have} bytes exceeds the {max}-byte limit")]
    LineTooLong { have: usize, max: usize },
}

/// Append one frame to `out`. The only sanctioned writer of the §10 layout.
///
/// # Errors
/// [`FrameError::PayloadTooLarge`] if the payload would push `len` past
/// [`MAX_FRAME_LEN`]; the frame is not appended and `out` is untouched.
pub fn encode_frame(
    kind: FrameKind,
    corr: u32,
    payload: &[u8],
    out: &mut Vec<u8>,
) -> Result<(), FrameError> {
    let len = frame_len(payload.len())?;
    out.reserve(FRAME_LEN_PREFIX + len as usize);
    out.extend_from_slice(&len.to_le_bytes());
    out.push(kind.as_u8());
    out.extend_from_slice(&corr.to_le_bytes());
    out.extend_from_slice(payload);
    Ok(())
}

/// [`encode_frame`] into a fresh `Vec`, for callers that write one frame at a
/// time.
///
/// # Errors
/// As [`encode_frame`].
pub fn frame_bytes(kind: FrameKind, corr: u32, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
    let mut out = Vec::with_capacity(FRAME_LEN_PREFIX + FRAME_HEADER_LEN + payload.len());
    encode_frame(kind, corr, payload, &mut out)?;
    Ok(out)
}

/// The `len` field for a payload of `payload_len` bytes: header + payload.
///
/// # Errors
/// [`FrameError::PayloadTooLarge`] past [`MAX_FRAME_LEN`].
pub fn frame_len(payload_len: usize) -> Result<u32, FrameError> {
    let too_large = FrameError::PayloadTooLarge {
        len: payload_len,
        max: MAX_FRAME_LEN,
    };
    let len = payload_len
        .checked_add(FRAME_HEADER_LEN)
        .ok_or_else(|| too_large.clone())?;
    let len = u32::try_from(len).map_err(|_| too_large.clone())?;
    if len > MAX_FRAME_LEN {
        return Err(too_large);
    }
    Ok(len)
}

/// Incremental decoder for the §10 stream. Feed it whatever a read syscall
/// returned; take whole frames out until it asks for more.
///
/// It owns one growable buffer and copies each payload out on delivery. There is
/// no zero-copy variant on purpose: the alternative pins the read buffer for as
/// long as the consumer holds the frame, which for a broadcast fan-out to N
/// windows means the slowest window throttles the pipe.
#[derive(Debug)]
pub struct FrameReader {
    buf: Vec<u8>,
    /// Bytes of `buf` already delivered; reclaimed by [`Self::compact`].
    head: usize,
    max_len: u32,
    /// Set by the first unrecoverable framing error and never cleared.
    poison: Option<FrameError>,
}

impl Default for FrameReader {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameReader {
    #[must_use]
    pub fn new() -> Self {
        Self::with_max_len(MAX_FRAME_LEN)
    }

    /// A tighter ceiling than [`MAX_FRAME_LEN`], for a peer that has no business
    /// sending large control frames.
    #[must_use]
    pub fn with_max_len(max_len: u32) -> Self {
        Self {
            buf: Vec::new(),
            head: 0,
            max_len: max_len.max(MIN_FRAME_LEN),
            poison: None,
        }
    }

    /// Hand the reader bytes off the pipe. Cheap; does no parsing.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Bytes held but not yet formed into a frame.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.buf.len() - self.head
    }

    /// True once a framing error has been reported; every later call repeats it.
    #[must_use]
    pub fn is_poisoned(&self) -> bool {
        self.poison.is_some()
    }

    /// The next complete frame, or `Ok(None)` when more bytes are needed.
    ///
    /// # Errors
    /// Any [`FrameError`] variant except the `Truncated*` ones, which belong to
    /// [`Self::end_of_stream`]: mid-stream, a short buffer is not an error.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, FrameError> {
        if let Some(err) = &self.poison {
            return Err(err.clone());
        }
        if self.pending() < FRAME_LEN_PREFIX {
            return Ok(None);
        }
        let at = self.head;
        let mut len_bytes = [0_u8; FRAME_LEN_PREFIX];
        len_bytes.copy_from_slice(&self.buf[at..at + FRAME_LEN_PREFIX]);
        let len = u32::from_le_bytes(len_bytes);

        if len < MIN_FRAME_LEN {
            return Err(self.poisoned(FrameError::Undersized { len }));
        }
        if len > self.max_len {
            return Err(self.poisoned(FrameError::Oversized {
                len,
                max: self.max_len,
            }));
        }
        // Checked before the payload is touched, so an oversized `len` never
        // reserves anything.
        let need = FRAME_LEN_PREFIX + len as usize;
        if self.pending() < need {
            return Ok(None);
        }

        let kind = match FrameKind::from_u8(self.buf[at + FRAME_LEN_PREFIX]) {
            Ok(k) => k,
            Err(e) => return Err(self.poisoned(e)),
        };
        let corr_at = at + FRAME_LEN_PREFIX + 1;
        let mut corr_bytes = [0_u8; 4];
        corr_bytes.copy_from_slice(&self.buf[corr_at..corr_at + 4]);
        let corr = u32::from_le_bytes(corr_bytes);

        let body_at = at + FRAME_LEN_PREFIX + FRAME_HEADER_LEN;
        let payload = self.buf[body_at..at + need].to_vec();
        self.head += need;
        self.compact();
        Ok(Some(Frame {
            kind,
            corr,
            payload,
        }))
    }

    /// Assert the peer stopped on a frame boundary. Call once EOF is observed.
    ///
    /// # Errors
    /// [`FrameError::Truncated`] when a partial frame is still buffered — the
    /// engine died mid-write, which the supervisor must see as a crash and not
    /// as a clean shutdown.
    pub fn end_of_stream(&self) -> Result<(), FrameError> {
        if let Some(err) = &self.poison {
            return Err(err.clone());
        }
        let have = self.pending();
        if have == 0 {
            return Ok(());
        }
        // With fewer than 4 bytes the length prefix itself is incomplete, so all
        // we can honestly say is "at least one more than we have".
        let need = if have < FRAME_LEN_PREFIX {
            FRAME_LEN_PREFIX
        } else {
            let mut len_bytes = [0_u8; FRAME_LEN_PREFIX];
            len_bytes.copy_from_slice(&self.buf[self.head..self.head + FRAME_LEN_PREFIX]);
            FRAME_LEN_PREFIX + u32::from_le_bytes(len_bytes) as usize
        };
        Err(FrameError::Truncated { have, need })
    }

    fn poisoned(&mut self, err: FrameError) -> FrameError {
        self.poison = Some(err.clone());
        err
    }

    /// Reclaim delivered bytes. Amortised: a memmove per 64 KiB consumed, not
    /// per frame.
    fn compact(&mut self) {
        const COMPACT_AT: usize = 64 * 1024;
        if self.head == self.buf.len() {
            self.buf.clear();
            self.head = 0;
        } else if self.head >= COMPACT_AT {
            self.buf.drain(..self.head);
            self.head = 0;
        }
    }
}

// ---------------------------------------------------------------------------
// §7.1 — the NDJSON envelope (normative — A9)
// ---------------------------------------------------------------------------

/// `t` — which of the three enums `body` holds. **Not** a method name: §7.1's
/// whole point is that the Rust variant name inside `body` is the method name,
/// so there is no registry to keep in sync.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum WireTag {
    Req,
    Resp,
    Event,
}

/// One NDJSON line: `{v, t, corr, body}`, in that order, and nothing else.
///
/// There is no `jsonrpc`, no `id`, no `method` and no `params` (A9). `corr` is
/// present on `req`/`resp` and absent on `event`, which `skip_serializing_if`
/// enforces on the way out and `Option` tolerates on the way in.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "specta", derive(specta::Type))]
pub struct Envelope<B> {
    /// Always [`STREAM_SCHEMA`] for a frame this build wrote.
    pub v: u32,
    pub t: WireTag,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corr: Option<u32>,
    pub body: B,
}

impl<B> Envelope<B> {
    #[must_use]
    pub fn req(corr: u32, body: B) -> Self {
        Self {
            v: STREAM_SCHEMA,
            t: WireTag::Req,
            corr: Some(corr),
            body,
        }
    }

    #[must_use]
    pub fn resp(corr: u32, body: B) -> Self {
        Self {
            v: STREAM_SCHEMA,
            t: WireTag::Resp,
            corr: Some(corr),
            body,
        }
    }

    /// Events carry no `corr` (§7.1) — they answer nothing.
    #[must_use]
    pub fn event(body: B) -> Self {
        Self {
            v: STREAM_SCHEMA,
            t: WireTag::Event,
            corr: None,
            body,
        }
    }
}

/// Ceiling on one NDJSON line. Generous — a `BlockMapChanged` for a 5 000-block
/// document is legitimately large — but finite, for the same reason
/// [`MAX_FRAME_LEN`] is.
pub const MAX_NDJSON_LINE: usize = 32 * 1024 * 1024;

/// Splits an NDJSON byte stream into lines. The half of §7.1 that does not need
/// a JSON parser, so it can live here beside the framing it parallels.
///
/// Empty lines are skipped, a UTF-8 BOM at the very start is dropped and a
/// trailing `\r` is trimmed. §7.1 forbids all three; a reader that dies on them
/// makes a third-party producer's minor sloppiness look like our bug, and
/// tolerating them cannot mask a real error because the JSON parse still has to
/// succeed afterwards.
#[derive(Debug)]
pub struct LineReader {
    buf: Vec<u8>,
    head: usize,
    max_line: usize,
    started: bool,
    poison: Option<FrameError>,
}

impl Default for LineReader {
    fn default() -> Self {
        Self::new()
    }
}

impl LineReader {
    #[must_use]
    pub fn new() -> Self {
        Self::with_max_line(MAX_NDJSON_LINE)
    }

    #[must_use]
    pub fn with_max_line(max_line: usize) -> Self {
        Self {
            buf: Vec::new(),
            head: 0,
            max_line: max_line.max(2),
            started: false,
            poison: None,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    #[must_use]
    pub fn pending(&self) -> usize {
        self.buf.len() - self.head
    }

    /// The next complete line, without its terminator, or `Ok(None)` for "feed
    /// me more". The caller parses it; an unparseable line is skipped by the
    /// caller, per §7.1's forward-compatibility rule.
    ///
    /// # Errors
    /// [`FrameError::LineTooLong`] once the unterminated tail passes the limit —
    /// reported before the line completes, so a producer that never emits `\n`
    /// cannot grow the buffer without bound.
    pub fn next_line(&mut self) -> Result<Option<Vec<u8>>, FrameError> {
        loop {
            if let Some(err) = &self.poison {
                return Err(err.clone());
            }
            let tail = &self.buf[self.head..];
            let Some(nl) = tail.iter().position(|&b| b == b'\n') else {
                if tail.len() > self.max_line {
                    let err = FrameError::LineTooLong {
                        have: tail.len(),
                        max: self.max_line,
                    };
                    self.poison = Some(err.clone());
                    return Err(err);
                }
                return Ok(None);
            };
            if nl > self.max_line {
                let err = FrameError::LineTooLong {
                    have: nl,
                    max: self.max_line,
                };
                self.poison = Some(err.clone());
                return Err(err);
            }
            let mut line = &tail[..nl];
            self.head += nl + 1;
            if !self.started {
                self.started = true;
                line = line.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(line);
            }
            if let Some(trimmed) = line.strip_suffix(b"\r") {
                line = trimmed;
            }
            let out = line.to_vec();
            self.compact();
            if out.is_empty() {
                continue;
            }
            return Ok(Some(out));
        }
    }

    /// Assert the producer stopped after a terminator.
    ///
    /// # Errors
    /// [`FrameError::TruncatedLine`] when a partial line remains — an engine
    /// killed mid-write, which `stratum run --json | jq` would otherwise report
    /// as our malformed JSON.
    pub fn end_of_stream(&self) -> Result<(), FrameError> {
        if let Some(err) = &self.poison {
            return Err(err.clone());
        }
        match self.pending() {
            0 => Ok(()),
            have => Err(FrameError::TruncatedLine { have }),
        }
    }

    fn compact(&mut self) {
        const COMPACT_AT: usize = 64 * 1024;
        if self.head == self.buf.len() {
            self.buf.clear();
            self.head = 0;
        } else if self.head >= COMPACT_AT {
            self.buf.drain(..self.head);
            self.head = 0;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — W07's first two acceptance bullets.
//
// They live in the file rather than in `tests/`, because `crates/stratum-proto/
// tests/roundtrip.rs` is W00's (R0) and because a codec's tests want the private
// buffer state the public API deliberately hides.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    use super::*;
    use crate::block::{
        BlockMap, CellMarker, Delimiter, RegionKind, RegionSummary, SectionSpan, Unterminated,
    };
    use crate::complete::CompletionEnv;
    use crate::data::{DataEvent, RenderMode, StorageType};
    use crate::diagnostic::{Confidence, Diagnostic, Severity};
    use crate::engine::{
        AiContextWant, EngineEvent, EngineHealth, EngineRequest, GraphFormat, InlineResultsMode,
        OrderSpec, OutputStream, SessionMode, SortDir,
    };
    use crate::exec::{CancelLevel, ExecStatus, ForwardScope, Isolation, RunIntent, SkipReason};
    use crate::ids::{
        BlockId, CodeHash, DatasetStateId, DocumentId, Edit, ExecutionId, LineRange, OrderId,
        ResultId, RunId, SectionId, SessionId, Span, StateId, VarIdx,
    };
    use crate::result::{
        AssetRef, Cell, DataChangeSummary, GenericTable, LayoutHint, LogPayload, RawRef,
        ResultEnvelope, ResultPayload, ScalarValue, StyleId, StyledRun,
    };
    use crate::session::{LogSearchOpts, SessionConfigWire};
    use crate::status::{BlockStatus, BrokenReason, DepKey, StaleReason, Taint};

    /// Values per encoding in the acceptance round-trip.
    const ROUND_TRIPS: usize = 100_000;

    // -----------------------------------------------------------------------
    // A deterministic generator.
    //
    // Not proptest: proptest shrinks, and there is nothing to shrink here — a
    // framing bug reproduces from the seed alone. What we want instead is 10^5
    // values for the price of 10^5 `wrapping_mul`s, and a failure that anyone
    // can replay by hard-coding the printed index.
    // -----------------------------------------------------------------------

    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            Self(seed ^ 0x9E37_79B9_7F4A_7C15)
        }

        /// splitmix64.
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        fn below(&mut self, n: u32) -> u32 {
            (self.next_u64() % u64::from(n)) as u32
        }

        fn flag(&mut self) -> bool {
            self.next_u64() & 1 == 1
        }

        fn u32(&mut self) -> u32 {
            self.next_u64() as u32
        }

        /// Finite only, on purpose: NaN and ±∞ have no JSON representation, and
        /// no Stata value needs one — the missing sentinels (`.`, `.a`..`.z`)
        /// are large FINITE doubles near 8.988e307, which this generator emits.
        fn f64(&mut self) -> f64 {
            match self.below(5) {
                0 => 0.0,
                1 => 0.1 + 0.2,
                2 => 8.988_465_674_311_58e307,
                3 => -f64::from(self.u32() % 100_000) / 7.0,
                _ => f64::from(self.u32()) * 1.234_567_890_123_456_7,
            }
        }

        /// Deliberately awkward: multibyte, a quote, a backslash, a raw LF and a
        /// C0 control, because each one is a different escape path in JSON and a
        /// different length prefix in MessagePack.
        fn text(&mut self) -> String {
            const PIECES: [&str; 8] = [
                "",
                "summarize price mpg",
                "réplique \u{00e9}\u{4e2d}\u{6587}",
                "quote\"backslash\\",
                "line\nbreak",
                "\u{0007}bell",
                "gen ln_income = ln(income)",
                "\u{1F600}",
            ];
            let n = self.below(3) + 1;
            let mut s = String::new();
            for _ in 0..n {
                s.push_str(PIECES[self.below(PIECES.len() as u32) as usize]);
            }
            s
        }

        fn path(&mut self) -> Utf8PathBuf {
            Utf8PathBuf::from(match self.below(3) {
                0 => "/Users/ana/proj/01 clean.do",
                1 => "analysis/regress.do",
                _ => "C:\\Users\\ana\\auto.dta",
            })
        }

        /// `Some` half the time. Takes a closure rather than a value because
        /// every interesting `Option` here is itself drawn from this Rng, and
        /// `r.opt(r.next())` cannot borrow `r` twice.
        fn opt<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> Option<T> {
            if self.flag() {
                Some(f(self))
            } else {
                None
            }
        }

        fn span(&mut self) -> Span {
            let start = self.u32();
            Span {
                start,
                end: start.saturating_add(self.below(4096)),
            }
        }

        fn code_hash(&mut self) -> CodeHash {
            let mut b = [0_u8; 16];
            b[..8].copy_from_slice(&self.next_u64().to_le_bytes());
            b[8..].copy_from_slice(&self.next_u64().to_le_bytes());
            CodeHash(b)
        }

        fn styled(&mut self) -> Vec<StyledRun> {
            (0..self.below(4))
                .map(|_| StyledRun {
                    text: self.text(),
                    style: match self.below(10) {
                        0 => StyleId::Text,
                        1 => StyleId::Input,
                        2 => StyleId::Result,
                        3 => StyleId::Error,
                        4 => StyleId::ErrorToken,
                        5 => StyleId::Hilite,
                        6 => StyleId::Comment,
                        7 => StyleId::Heading,
                        8 => StyleId::Rule,
                        _ => StyleId::Link {
                            target_index: self.u32(),
                        },
                    },
                })
                .collect()
        }

        fn diagnostic(&mut self) -> Diagnostic {
            Diagnostic {
                severity: match self.below(4) {
                    0 => Severity::Error,
                    1 => Severity::Warning,
                    2 => Severity::Note,
                    _ => Severity::Help,
                },
                code: "STATA0111".into(),
                stata_rc: self.opt(|_| 111),
                message: self.text(),
                file: self.opt(Self::path),
                span: self.opt(Self::span),
                offending_token: self.opt(Self::text),
                block: self.opt(|r| BlockId(r.next_u64())),
                related: Vec::new(),
                suggestions: Vec::new(),
                notes: vec![self.text()],
                confidence: match self.below(3) {
                    0 => Confidence::Exact,
                    1 => Confidence::Probable,
                    _ => Confidence::Speculative,
                },
            }
        }

        fn envelope(&mut self) -> ResultEnvelope {
            let payload = match self.below(6) {
                0 => ResultPayload::Log(LogPayload {
                    runs: self.styled(),
                    lines: self.below(400),
                }),
                1 => ResultPayload::Scalars {
                    values: vec![
                        (
                            "r(N)".into(),
                            ScalarValue::Num {
                                value: self.f64(),
                                display: self.text(),
                            },
                        ),
                        ("r(cmd)".into(), ScalarValue::Str { value: self.text() }),
                    ],
                },
                2 => ResultPayload::Table(GenericTable {
                    title: self.opt(Self::text),
                    colnames: vec![self.text()],
                    rownames: vec![self.text()],
                    cells: vec![
                        Some(Cell::Num {
                            value: self.f64(),
                            display: self.text(),
                        }),
                        None,
                        Some(Cell::Str { value: self.text() }),
                    ],
                    col_align: vec![crate::result::Align::Decimal],
                }),
                3 => ResultPayload::DataChanged(DataChangeSummary {
                    frame: "default".into(),
                    obs_before: self.next_u64(),
                    obs_after: self.next_u64(),
                    vars_before: self.u32(),
                    vars_after: self.u32(),
                    created: vec![self.text()],
                    modified: Vec::new(),
                    dropped: Vec::new(),
                    renamed: vec![(self.text(), self.text())],
                    notes: vec!["(1 missing value generated)".into()],
                }),
                4 => ResultPayload::Error(self.diagnostic()),
                _ => ResultPayload::Unknown,
            };
            ResultEnvelope {
                result: ResultId(self.next_u64()),
                revision: self.u32(),
                exec: ExecutionId(self.next_u64()),
                block: self.opt(|r| BlockId(r.next_u64())),
                dataset_state: DatasetStateId(self.next_u64()),
                code_hash: self.code_hash(),
                cmdline: self.text(),
                started_at_ms: self.next_u64(),
                duration_us: self.next_u64(),
                rc: self.below(1000),
                payloads: vec![payload],
                raw: RawRef {
                    bytes: self.next_u64(),
                    lines: self.u32(),
                    head: self.text(),
                    truncated: self.flag(),
                    asset: AssetRef {
                        path: "result/S1/R41/raw".into(),
                        mime: "text/plain; charset=utf-8".into(),
                        bytes: self.next_u64(),
                    },
                },
                layout_hint: LayoutHint {
                    rows: self.below(64),
                    cols: self.below(16),
                    est_px: self.below(2000),
                },
                actions: vec![crate::result::CardAction::RawOutput],
            }
        }

        fn block_status(&mut self) -> BlockStatus {
            match self.below(9) {
                0 => BlockStatus::NeverRun,
                1 => BlockStatus::Queued {
                    position: self.below(32),
                },
                2 => BlockStatus::Running {
                    exec: ExecutionId(self.next_u64()),
                    started_ms: self.next_u64(),
                },
                3 => BlockStatus::Current {
                    exec: ExecutionId(self.next_u64()),
                    dataset: DatasetStateId(self.next_u64()),
                    duration_us: self.next_u64(),
                },
                4 => BlockStatus::CurrentUnverifiable {
                    exec: ExecutionId(self.next_u64()),
                    dataset: DatasetStateId(self.next_u64()),
                    duration_us: self.next_u64(),
                    taint: Taint::from_bits_retain(self.next_u64() as u16),
                },
                5 => BlockStatus::Stale {
                    reason: match self.below(4) {
                        0 => StaleReason::CodeChanged,
                        1 => StaleReason::InputChanged {
                            key: DepKey::Var {
                                frame: "default".into(),
                                name: self.text(),
                            },
                            at: self.opt(|r| ExecutionId(r.next_u64())),
                        },
                        2 => StaleReason::UpstreamPending {
                            block: BlockId(self.next_u64()),
                            via: DepKey::File { path: self.path() },
                        },
                        _ => StaleReason::RngShifted,
                    },
                    since: self.opt(|r| ExecutionId(r.next_u64())),
                },
                6 => BlockStatus::Failed {
                    exec: ExecutionId(self.next_u64()),
                    rc: self.below(1000),
                },
                7 => BlockStatus::Interrupted {
                    exec: ExecutionId(self.next_u64()),
                    rolled_back: self.flag(),
                },
                _ => BlockStatus::Broken {
                    reason: BrokenReason::UnresolvedName {
                        name: self.text(),
                        suggestion: self.opt(Self::text),
                    },
                },
            }
        }

        fn block_map(&mut self) -> BlockMap {
            let n = self.below(3) + 1;
            let regions = (0..n)
                .map(|i| RegionSummary {
                    index: i,
                    span: self.span(),
                    outer_span: self.span(),
                    lines: LineRange {
                        start: self.below(1000),
                        end: self.below(1000),
                    },
                    code_lines: LineRange {
                        start: self.below(1000),
                        end: self.below(1000),
                    },
                    kind: match self.below(3) {
                        0 => RegionKind::Simple,
                        1 => RegionKind::Trivia {
                            has_marker: self.flag(),
                        },
                        _ => RegionKind::Unterminated {
                            expected: Unterminated::CloseBrace,
                        },
                    },
                    entry_delimiter: Delimiter::Cr,
                    exit_delimiter: if self.flag() {
                        Delimiter::Cr
                    } else {
                        Delimiter::Semi
                    },
                    code_hash: self.code_hash(),
                    hash_ordinal: self.below(4),
                    canonical: self.opt(Self::text),
                    is_estimation: self.flag(),
                    has_macro_in_head: self.flag(),
                    section: self.opt(|r| SectionId(r.u32())),
                })
                .collect::<Vec<_>>();
            BlockMap {
                doc: DocumentId(self.u32()),
                generation: self.next_u64(),
                doc_version: self.next_u64(),
                blocks: (0..n).map(|_| BlockId(self.next_u64())).collect(),
                regions,
                markers: vec![CellMarker {
                    span: self.span(),
                    line: self.below(500),
                    title: self.text(),
                    section: SectionId(self.u32()),
                }],
                sections: vec![SectionSpan {
                    id: SectionId(self.u32()),
                    span: self.span(),
                    title: self.text(),
                    lines: LineRange {
                        start: 1,
                        end: self.below(400),
                    },
                }],
                retired: vec![BlockId(self.next_u64())],
                diagnostics: vec![self.diagnostic()],
                end_delimiter: Delimiter::Cr,
            }
        }

        fn completion_env(&mut self) -> CompletionEnv {
            CompletionEnv {
                generation: self.next_u64(),
                frame: "default".into(),
                frames: vec!["default".into(), self.text()],
                varnames: (0..self.below(8)).map(|_| self.text()).collect(),
                var_total: self.u32(),
                truncated: self.flag(),
                locals: vec![self.text()],
                globals: Vec::new(),
                scalars: vec![self.text()],
                matrices: Vec::new(),
                programs: Vec::new(),
                e_names: vec!["e(N)".into(), "e(r2)".into()],
                r_names: vec!["r(mean)".into()],
                value_labels: Vec::new(),
                stored_estimates: vec![self.text()],
                cwd: self.path(),
            }
        }
    }

    fn arb_request(r: &mut Rng) -> EngineRequest {
        let s = SessionId(r.u32());
        let doc = DocumentId(r.u32());
        match r.below(26) {
            0 => EngineRequest::Hello {
                client: r.text(),
                schema: r.u32(),
            },
            1 => EngineRequest::SessionOpen {
                project_root: r.path(),
                mode: if r.flag() {
                    SessionMode::Interactive
                } else {
                    SessionMode::Clean
                },
                config: SessionConfigWire {
                    cwd: r.opt(Rng::path),
                    seed: r.opt(Rng::next_u64),
                    linesize: 80,
                    level: 95.0,
                    varabbrev: r.flag(),
                    more: r.flag(),
                    max_memory_bytes: r.opt(Rng::next_u64),
                    ado_personal: r.flag(),
                    write_sandbox: r.opt(Rng::path),
                },
            },
            2 => EngineRequest::SessionClose { session: s },
            3 => EngineRequest::Status { session: s },
            4 => EngineRequest::DocOpen {
                session: s,
                doc,
                path: r.opt(Rng::path),
                text: r.text(),
            },
            5 => EngineRequest::DocChange {
                session: s,
                doc,
                version: r.next_u64(),
                edits: (0..r.below(4))
                    .map(|_| Edit {
                        span: r.span(),
                        text: r.text(),
                    })
                    .collect(),
            },
            6 => EngineRequest::DocClose { session: s, doc },
            7 => EngineRequest::ExecSubmit {
                session: s,
                intent: match r.below(6) {
                    0 => RunIntent::CurrentBlock {
                        doc,
                        cursor: r.u32(),
                    },
                    1 => RunIntent::Selection {
                        doc,
                        span: r.span(),
                    },
                    2 => RunIntent::FromHere {
                        doc,
                        block: BlockId(r.next_u64()),
                        scope: if r.flag() {
                            ForwardScope::Dependents
                        } else {
                            ForwardScope::AllBelow
                        },
                    },
                    3 => RunIntent::CleanRun {
                        entry: doc,
                        isolation: if r.flag() {
                            Isolation::InProcess
                        } else {
                            Isolation::Subprocess
                        },
                    },
                    4 => RunIntent::ProjectEntryPoint {
                        project_root: r.path(),
                        isolation: Isolation::Subprocess,
                    },
                    _ => RunIntent::CommandBar { text: r.text() },
                },
                inline_mode: match r.below(4) {
                    0 => InlineResultsMode::Always,
                    1 => InlineResultsMode::EditorRun,
                    2 => InlineResultsMode::Compact,
                    _ => InlineResultsMode::Off,
                },
            },
            8 => EngineRequest::ExecCancel {
                session: s,
                run: RunId(r.next_u64()),
                level: if r.flag() {
                    CancelLevel::Interrupt
                } else {
                    CancelLevel::Abort
                },
            },
            9 => EngineRequest::Blocks { session: s, doc },
            10 => EngineRequest::Statuses { session: s, doc },
            11 => EngineRequest::Ledger {
                session: s,
                from_seq: r.next_u64(),
                limit: r.u32(),
            },
            12 => EngineRequest::Variables {
                session: s,
                frame: r.text(),
            },
            13 => EngineRequest::VarStats {
                session: s,
                frame: r.text(),
                var: r.text(),
            },
            14 => EngineRequest::Frames { session: s },
            15 => EngineRequest::DataPage {
                session: s,
                request: crate::data::PageRequest {
                    frame: r.text(),
                    state: DatasetStateId(r.next_u64()),
                    row0: r.next_u64(),
                    nrows: r.below(4096),
                    cols: (0..r.below(8)).map(|_| VarIdx(r.u32())).collect(),
                    order: r.opt(|r| OrderId(r.u32())),
                    render: if r.flag() {
                        RenderMode::Display
                    } else {
                        RenderMode::Edit
                    },
                    seq: r.u32(),
                },
            },
            16 => EngineRequest::DataOrderSet {
                session: s,
                frame: r.text(),
                spec: OrderSpec {
                    keys: vec![(
                        VarIdx(r.u32()),
                        if r.flag() {
                            SortDir::Asc
                        } else {
                            SortDir::Desc
                        },
                    )],
                    filter: r.opt(Rng::text),
                    state: DatasetStateId(r.next_u64()),
                },
            },
            17 => EngineRequest::DataOrderDrop {
                session: s,
                order: OrderId(r.u32()),
            },
            18 => EngineRequest::GraphRender {
                session: s,
                result: ResultId(r.next_u64()),
                format: match r.below(3) {
                    0 => GraphFormat::Svg,
                    1 => GraphFormat::Png,
                    _ => GraphFormat::Pdf,
                },
                width_pt: f64::from(r.below(2000)) as f32,
            },
            19 => EngineRequest::LogRange {
                session: s,
                from_line: r.next_u64(),
                to_line: r.next_u64(),
            },
            20 => EngineRequest::LogSearch {
                session: s,
                query: r.text(),
                opts: LogSearchOpts {
                    regex: r.flag(),
                    case_sensitive: r.flag(),
                    max_hits: r.below(5000),
                },
            },
            21 => EngineRequest::ReproReport {
                session: s,
                doc,
                verify: r.flag(),
            },
            22 => EngineRequest::DefUse { session: s },
            23 => EngineRequest::CompletionEnv { session: s },
            24 => EngineRequest::CompletionEnvPage {
                session: s,
                from: r.u32(),
                count: r.below(1024),
            },
            25 => EngineRequest::AiContext {
                session: s,
                want: AiContextWant::from_bits_truncate(r.next_u64() as u16),
            },
            _ => EngineRequest::Shutdown,
        }
    }

    fn arb_event(r: &mut Rng) -> EngineEvent {
        let seq = r.next_u64();
        let exec = ExecutionId(r.next_u64());
        let run = RunId(r.next_u64());
        match r.below(14) {
            0 => EngineEvent::RunStarted {
                seq,
                schema: STREAM_SCHEMA,
                run,
                session: SessionId(r.u32()),
                stratum_version: "0.1.0".into(),
                source: r.opt(Rng::path),
                clean_state: r.flag(),
                cwd: r.path(),
                started_at_ms: r.next_u64(),
                seed: r.opt(Rng::next_u64),
                plan_len: r.below(4096),
            },
            1 => EngineEvent::BlockStarted {
                seq,
                run,
                exec,
                block: BlockId(r.next_u64()),
                doc: r.opt(|r| DocumentId(r.u32())),
                span: r.span(),
                code_hash: r.code_hash(),
                dataset_state_in: DatasetStateId(r.next_u64()),
                text: r.text(),
            },
            2 => EngineEvent::Output {
                seq,
                exec,
                stream: match r.below(3) {
                    0 => OutputStream::Results,
                    1 => OutputStream::Error,
                    _ => OutputStream::Trace,
                },
                runs: r.styled(),
            },
            3 => EngineEvent::OutputTruncated {
                seq,
                exec,
                dropped_bytes: r.next_u64(),
            },
            4 => EngineEvent::Result {
                seq,
                exec,
                envelope: r.envelope(),
            },
            5 => EngineEvent::Diagnostic {
                seq,
                exec: r.opt(|_| exec),
                diagnostic: r.diagnostic(),
            },
            6 => EngineEvent::Progress {
                seq,
                exec,
                done: r.next_u64(),
                total: r.opt(Rng::next_u64),
                label: r.text(),
            },
            7 => EngineEvent::StateChanged {
                seq,
                exec,
                dataset_state: DatasetStateId(r.next_u64()),
                state: StateId(r.next_u64()),
                frame: r.text(),
                n_obs: r.next_u64(),
                n_vars: r.u32(),
                events: vec![
                    DataEvent::ObsCountChanged {
                        frame: "default".into(),
                        n_obs: r.next_u64(),
                    },
                    DataEvent::TypeChanged {
                        frame: "default".into(),
                        name: r.text(),
                        from: StorageType::Float,
                        to: StorageType::Str { width: 24 },
                    },
                ],
            },
            8 => EngineEvent::BlockFinished {
                seq,
                run,
                exec,
                block: BlockId(r.next_u64()),
                result: r.opt(|r| ResultId(r.next_u64())),
                status: match r.below(6) {
                    0 => ExecStatus::Queued,
                    1 => ExecStatus::Running,
                    2 => ExecStatus::Succeeded,
                    3 => ExecStatus::Failed {
                        rc: r.below(1000),
                        message: r.text(),
                        span: r.opt(Rng::span),
                    },
                    4 => ExecStatus::Interrupted {
                        rolled_back: r.flag(),
                        at: r.opt(Rng::span),
                    },
                    _ => ExecStatus::Skipped {
                        reason: SkipReason::Unaffected,
                    },
                },
                rc: r.below(1000),
                duration_us: r.next_u64(),
                dataset_state_out: DatasetStateId(r.next_u64()),
            },
            9 => EngineEvent::StatusChanged {
                seq,
                doc: DocumentId(r.u32()),
                changed: (0..r.below(4))
                    .map(|_| (BlockId(r.next_u64()), r.block_status()))
                    .collect(),
            },
            10 => EngineEvent::BlockMapChanged {
                seq,
                map: r.block_map(),
            },
            11 => EngineEvent::RunFinished {
                seq,
                run,
                rc: r.below(1000),
                blocks_run: r.below(4096),
                blocks_failed: r.below(64),
                duration_us: r.next_u64(),
                finished_at_ms: r.next_u64(),
            },
            12 => EngineEvent::CompletionEnvChanged {
                seq,
                env: r.completion_env(),
            },
            _ => EngineEvent::EngineHealth {
                seq,
                health: match r.below(5) {
                    0 => EngineHealth::Starting,
                    1 => EngineHealth::Ready,
                    2 => EngineHealth::Busy { exec },
                    3 => EngineHealth::Crashed {
                        signal: r.opt(|_| 9),
                        last_statement: r.opt(Rng::text),
                        log_tail: r.text(),
                    },
                    _ => EngineHealth::Stopped,
                },
            },
        }
    }

    // -----------------------------------------------------------------------
    // §10 layout
    // -----------------------------------------------------------------------

    #[test]
    fn header_layout_is_byte_for_byte_contracts_10() {
        let bytes = frame_bytes(FrameKind::Event, 0, b"\x91\x2a").unwrap();
        // len = 5 + 2, LE; kind; corr LE; payload.
        assert_eq!(
            bytes,
            vec![7, 0, 0, 0, 2, 0, 0, 0, 0, 0x91, 0x2a],
            "the §10 header is len:u32LE | kind:u8 | corr:u32LE | payload"
        );
        assert_eq!(FrameKind::Request.as_u8(), 0);
        assert_eq!(FrameKind::Response.as_u8(), 1);
        assert_eq!(FrameKind::Event.as_u8(), 2);
        assert_eq!(FrameKind::Ping.as_u8(), 3);

        let mut rd = FrameReader::new();
        rd.feed(&bytes);
        let f = rd.next_frame().unwrap().expect("one whole frame");
        assert_eq!(f.kind, FrameKind::Event);
        assert_eq!(f.corr, CORR_UNSOLICITED);
        assert_eq!(f.payload, b"\x91\x2a");
        assert_eq!(rd.pending(), 0);
        rd.end_of_stream().unwrap();
    }

    #[test]
    fn empty_payload_is_a_legal_frame() {
        let bytes = frame_bytes(FrameKind::Ping, 7, &[]).unwrap();
        assert_eq!(bytes.len(), FRAME_LEN_PREFIX + FRAME_HEADER_LEN);
        let mut rd = FrameReader::new();
        rd.feed(&bytes);
        let f = rd.next_frame().unwrap().unwrap();
        assert_eq!((f.kind, f.corr, f.payload.len()), (FrameKind::Ping, 7, 0));
    }

    #[test]
    fn ping_payload_round_trips() {
        let ping = Ping {
            nonce: 0xDEAD_BEEF_FEED_FACE,
            pong: true,
        };
        let mp = rmp_serde::to_vec_named(&ping).unwrap();
        let bytes = frame_bytes(FrameKind::Ping, CORR_UNSOLICITED, &mp).unwrap();
        let mut rd = FrameReader::new();
        rd.feed(&bytes);
        let f = rd.next_frame().unwrap().unwrap();
        let back: Ping = rmp_serde::from_slice(&f.payload).unwrap();
        assert_eq!(back, ping);
    }

    // -----------------------------------------------------------------------
    // ACCEPTANCE 1 — 10^5 random values through both encodings.
    // -----------------------------------------------------------------------

    #[test]
    fn codec_round_trips_100k_random_values_through_both_encodings() {
        let mut rng = Rng::new(0x5732_4154_554D_0001);
        let mut reader = FrameReader::new();
        let mut wire = Vec::new();

        for i in 0..ROUND_TRIPS {
            let request = i % 2 == 0;

            // --- MessagePack, framed (§10) -------------------------------
            wire.clear();
            let (kind, corr, mp) = if request {
                let v = arb_request(&mut rng);
                (
                    FrameKind::Request,
                    rng.u32() | 1,
                    rmp_serde::to_vec_named(&v).unwrap_or_else(|e| panic!("#{i} mp encode: {e}")),
                )
            } else {
                let v = arb_event(&mut rng);
                (
                    FrameKind::Event,
                    CORR_UNSOLICITED,
                    rmp_serde::to_vec_named(&v).unwrap_or_else(|e| panic!("#{i} mp encode: {e}")),
                )
            };
            encode_frame(kind, corr, &mp, &mut wire).unwrap();

            // Feed in adversarial slices: the split lands inside the length
            // prefix, inside the header and inside the payload across the run,
            // which is exactly what a real pipe does and what a naive reader
            // that assumes "one read == one frame" fails.
            let cut = 1 + (rng.next_u64() as usize) % wire.len();
            reader.feed(&wire[..cut]);
            if cut < wire.len() {
                assert!(
                    reader.next_frame().unwrap().is_none(),
                    "#{i}: a partial frame must be Ok(None), never a frame"
                );
                reader.feed(&wire[cut..]);
            }
            let f = reader
                .next_frame()
                .unwrap_or_else(|e| panic!("#{i} decode: {e}"))
                .unwrap_or_else(|| panic!("#{i}: whole frame fed, none produced"));
            assert_eq!(f.kind, kind, "#{i}");
            assert_eq!(f.corr, corr, "#{i}");
            assert_eq!(f.payload, mp, "#{i}: payload changed in the frame");
            assert!(reader.next_frame().unwrap().is_none(), "#{i}: extra frame");

            // --- both encodings decode back to the same value -------------
            if request {
                let from_mp: EngineRequest = rmp_serde::from_slice(&f.payload)
                    .unwrap_or_else(|e| panic!("#{i} mp decode: {e}"));
                let json = serde_json::to_string(&from_mp).unwrap();
                let from_json: EngineRequest = serde_json::from_str(&json)
                    .unwrap_or_else(|e| panic!("#{i} json decode: {e}\n  {json}"));
                assert_eq!(from_mp, from_json, "#{i}: encodings disagree");
                assert!(!json.contains('\n'), "#{i}: a raw LF would split the line");
            } else {
                let from_mp: EngineEvent = rmp_serde::from_slice(&f.payload)
                    .unwrap_or_else(|e| panic!("#{i} mp decode: {e}"));
                let line = serde_json::to_string(&Envelope::event(&from_mp)).unwrap();
                let back: Envelope<EngineEvent> = serde_json::from_str(&line)
                    .unwrap_or_else(|e| panic!("#{i} json decode: {e}\n  {line}"));
                assert_eq!(back.body, from_mp, "#{i}: encodings disagree");
                assert!(!line.contains('\n'), "#{i}: a raw LF would split the line");
            }
        }
        reader.end_of_stream().unwrap();
    }

    // -----------------------------------------------------------------------
    // ACCEPTANCE 1 — truncation is a clean error, never a panic or a desync.
    // -----------------------------------------------------------------------

    #[test]
    fn every_truncation_point_is_ok_none_then_a_clean_eof_error() {
        let mut rng = Rng::new(9);
        let payload = rmp_serde::to_vec_named(&arb_event(&mut rng)).unwrap();
        let whole = frame_bytes(FrameKind::Event, CORR_UNSOLICITED, &payload).unwrap();

        // From 1: zero bytes fed is a clean EOF, not a truncation.
        for cut in 1..whole.len() {
            let mut rd = FrameReader::new();
            rd.feed(&whole[..cut]);
            assert!(
                rd.next_frame().unwrap().is_none(),
                "cut at {cut} of {}: a short frame is not a frame",
                whole.len()
            );
            let err = rd.end_of_stream().unwrap_err();
            assert!(
                matches!(err, FrameError::Truncated { have, .. } if have == cut),
                "cut at {cut}: {err}"
            );
            // The reader is still usable: feeding the rest completes the frame.
            rd.feed(&whole[cut..]);
            assert!(rd.next_frame().unwrap().is_some(), "cut at {cut}");
            rd.end_of_stream().unwrap();
        }
    }

    #[test]
    fn undersized_length_is_rejected_and_poisons_the_reader() {
        for len in 0_u32..MIN_FRAME_LEN {
            let mut rd = FrameReader::new();
            let mut bytes = len.to_le_bytes().to_vec();
            bytes.extend_from_slice(&[0; 8]);
            rd.feed(&bytes);
            assert_eq!(rd.next_frame().unwrap_err(), FrameError::Undersized { len });
            assert!(rd.is_poisoned());
            // Poison is permanent: no resync, because a stdio pipe has no
            // marker to resync ON. The supervisor's answer is to respawn.
            assert_eq!(rd.next_frame().unwrap_err(), FrameError::Undersized { len });
            assert!(rd.end_of_stream().is_err());
        }
    }

    #[test]
    fn oversized_length_is_rejected_before_anything_is_allocated() {
        let mut rd = FrameReader::with_max_len(1024);
        rd.feed(&u32::MAX.to_le_bytes());
        assert_eq!(
            rd.next_frame().unwrap_err(),
            FrameError::Oversized {
                len: u32::MAX,
                max: 1024,
            }
        );
        assert_eq!(rd.pending(), FRAME_LEN_PREFIX, "no payload was buffered");
    }

    #[test]
    fn unknown_kind_byte_poisons_rather_than_resyncing() {
        let mut bytes = frame_bytes(FrameKind::Event, 0, b"xy").unwrap();
        bytes[FRAME_LEN_PREFIX] = 9;
        let mut rd = FrameReader::new();
        rd.feed(&bytes);
        assert_eq!(
            rd.next_frame().unwrap_err(),
            FrameError::UnknownKind { kind: 9 }
        );
        assert!(rd.is_poisoned());
    }

    #[test]
    fn a_payload_past_the_limit_is_refused_by_the_encoder() {
        let max_payload = MAX_FRAME_LEN as usize - FRAME_HEADER_LEN;
        assert_eq!(frame_len(max_payload).unwrap(), MAX_FRAME_LEN);
        for over in [max_payload + 1, MAX_FRAME_LEN as usize, usize::MAX] {
            let err = frame_len(over).unwrap_err();
            assert!(
                matches!(err, FrameError::PayloadTooLarge { .. }),
                "{over}: {err}"
            );
        }
        // One byte over the largest legal payload, allocated for real: the
        // encoder must refuse it *and* leave the caller's buffer alone, because
        // a half-written frame is exactly the desync this file exists to
        // prevent.
        let too_big = vec![0_u8; max_payload + 1];
        let mut out = vec![0xAA];
        assert!(encode_frame(FrameKind::Event, 0, &too_big, &mut out).is_err());
        assert_eq!(out, vec![0xAA], "a refused frame must not touch the buffer");
    }

    #[test]
    fn back_to_back_frames_decode_in_order_from_one_buffer() {
        let mut rng = Rng::new(31);
        let mut wire = Vec::new();
        let mut sent = Vec::new();
        for i in 0..64_u32 {
            let ev = arb_event(&mut rng);
            let mp = rmp_serde::to_vec_named(&ev).unwrap();
            encode_frame(FrameKind::Event, i, &mp, &mut wire).unwrap();
            sent.push((i, mp));
        }
        let mut rd = FrameReader::new();
        rd.feed(&wire);
        for (corr, mp) in sent {
            let f = rd.next_frame().unwrap().unwrap();
            assert_eq!((f.corr, f.payload), (corr, mp));
        }
        assert!(rd.next_frame().unwrap().is_none());
        rd.end_of_stream().unwrap();
    }

    // -----------------------------------------------------------------------
    // ACCEPTANCE 2 — the NDJSON envelope is exactly §7.1.
    // -----------------------------------------------------------------------

    #[test]
    fn envelope_is_exactly_v_t_corr_body_in_that_order() {
        let req = Envelope::req(
            41,
            EngineRequest::Status {
                session: SessionId(1),
            },
        );
        let line = serde_json::to_string(&req).unwrap();
        assert_eq!(
            line,
            r#"{"v":1,"t":"req","corr":41,"body":{"req":"status","session":1}}"#
        );

        let resp = Envelope::resp(41, crate::engine::EngineResponse::Ok);
        assert_eq!(
            serde_json::to_string(&resp).unwrap(),
            r#"{"v":1,"t":"resp","corr":41,"body":{"resp":"ok"}}"#
        );

        // An event carries NO `corr` (§7.1), not `"corr":null`.
        let ev = Envelope::event(EngineEvent::RunFinished {
            seq: 7,
            run: RunId(2),
            rc: 0,
            blocks_run: 3,
            blocks_failed: 0,
            duration_us: 1234,
            finished_at_ms: 1_700_000_000_000,
        });
        let line = serde_json::to_string(&ev).unwrap();
        assert!(
            line.starts_with(r#"{"v":1,"t":"event","body":{"event":"run_finished","#),
            "{line}"
        );
        assert!(!line.contains("corr"), "{line}");

        // A9: no JSON-RPC anywhere, no method registry, no params wrapper.
        for banned in ["jsonrpc", "\"id\"", "method", "params", "\"result\""] {
            assert!(!line.contains(banned), "{banned} must not appear: {line}");
        }
    }

    #[test]
    fn envelope_field_names_are_the_wire_and_survive_msgpack_too() {
        // `--protocol json` is §7.1, but the envelope type is shared, and
        // `to_vec_named` must keep the same four keys.
        let ev = Envelope::event(EngineEvent::Progress {
            seq: 1,
            exec: ExecutionId(2),
            done: 3,
            total: None,
            label: "bootstrapping".into(),
        });
        let mp = rmp_serde::to_vec_named(&ev).unwrap();
        let back: Envelope<EngineEvent> = rmp_serde::from_slice(&mp).unwrap();
        assert_eq!(back, ev);
        let as_text = String::from_utf8_lossy(&mp);
        for key in ["v", "t", "body"] {
            assert!(as_text.contains(key), "msgpack lost the field name {key}");
        }
    }

    /// The three lines `serve/ndjson.rs` ships, kept here so the property is
    /// asserted by a test that runs today: a line whose `t` or whose `body` tag
    /// this build does not know is SKIPPED, and the reader keeps going.
    fn read_events(stream: &[u8]) -> Result<Vec<EngineEvent>, FrameError> {
        let mut lines = LineReader::new();
        lines.feed(stream);
        let mut out = Vec::new();
        while let Some(line) = lines.next_line()? {
            match serde_json::from_slice::<Envelope<EngineEvent>>(&line) {
                Ok(env) if env.t == WireTag::Event => out.push(env.body),
                // Unknown `t`, unknown body tag, or a body this schema cannot
                // shape: skip the line. §15's additive-only rule is only usable
                // by a third party if this is what a reader does.
                _ => continue,
            }
        }
        lines.end_of_stream()?;
        Ok(out)
    }

    #[test]
    fn an_unknown_body_tag_is_skipped_and_the_reader_continues() {
        let known_a = Envelope::event(EngineEvent::OutputTruncated {
            seq: 1,
            exec: ExecutionId(9),
            dropped_bytes: 4096,
        });
        let known_b = Envelope::event(EngineEvent::RunFinished {
            seq: 3,
            run: RunId(1),
            rc: 0,
            blocks_run: 1,
            blocks_failed: 0,
            duration_us: 10,
            finished_at_ms: 1,
        });
        let mut stream = String::new();
        stream.push_str(&serde_json::to_string(&known_a).unwrap());
        stream.push('\n');
        // A variant from a future schema minor, additive per §15.
        stream.push_str(
            r#"{"v":1,"t":"event","body":{"event":"telemetry_ping","payload":{"a":[1,2,3]}}}"#,
        );
        stream.push('\n');
        // An unknown envelope kind.
        stream.push_str(r#"{"v":1,"t":"trace","corr":4,"body":{"anything":true}}"#);
        stream.push('\n');
        // A known variant carrying an unknown FIELD — also additive, and here
        // serde must accept it rather than skip the line.
        stream.push_str(
            r#"{"v":1,"t":"event","body":{"event":"output_truncated","seq":2,"exec":9,"dropped_bytes":8,"future_field":"x"}}"#,
        );
        stream.push('\n');
        stream.push_str(&serde_json::to_string(&known_b).unwrap());
        stream.push('\n');

        let events = read_events(stream.as_bytes()).unwrap();
        assert_eq!(events.len(), 3, "{events:#?}");
        assert_eq!(events[0], known_a.body);
        assert!(matches!(
            events[1],
            EngineEvent::OutputTruncated {
                seq: 2,
                dropped_bytes: 8,
                ..
            }
        ));
        assert_eq!(events[2], known_b.body);
    }

    #[test]
    fn line_reader_tolerates_a_bom_crlf_and_blank_lines() {
        let mut stream = vec![0xEF, 0xBB, 0xBF];
        stream.extend_from_slice(b"{\"v\":1}\r\n\n{\"v\":2}\n");
        let mut rd = LineReader::new();
        rd.feed(&stream);
        assert_eq!(rd.next_line().unwrap().unwrap(), b"{\"v\":1}");
        assert_eq!(rd.next_line().unwrap().unwrap(), b"{\"v\":2}");
        assert!(rd.next_line().unwrap().is_none());
        rd.end_of_stream().unwrap();
    }

    #[test]
    fn a_line_with_no_terminator_at_eof_is_an_error_not_a_record() {
        let mut rd = LineReader::new();
        rd.feed(b"{\"v\":1,\"t\":\"event\"");
        assert!(rd.next_line().unwrap().is_none());
        assert!(matches!(
            rd.end_of_stream().unwrap_err(),
            FrameError::TruncatedLine { have: 18 }
        ));
    }

    #[test]
    fn an_unterminated_line_cannot_grow_without_bound() {
        let mut rd = LineReader::with_max_line(64);
        rd.feed(&[b'x'; 65]);
        assert!(matches!(
            rd.next_line().unwrap_err(),
            FrameError::LineTooLong { max: 64, .. }
        ));
    }

    #[test]
    fn ndjson_round_trips_every_line_of_a_realistic_stream() {
        let mut rng = Rng::new(0xFEED);
        let mut stream = Vec::new();
        let mut sent = Vec::new();
        for _ in 0..2_000 {
            let ev = arb_event(&mut rng);
            let line = serde_json::to_string(&Envelope::event(&ev)).unwrap();
            assert!(!line.contains('\n'), "serde_json escaped nothing: {line}");
            stream.extend_from_slice(line.as_bytes());
            stream.push(b'\n');
            sent.push(ev);
        }
        assert_eq!(read_events(&stream).unwrap(), sent);
    }

    // -----------------------------------------------------------------------
    // The committed mock stream — for now, the only in-tree check on it.
    //
    // `--mock` replaying `tests/fixtures/mock/scenario_a.msgpack` over the real
    // transport is asserted in `apps/desktop/src-tauri/src/engine_host.rs`, and
    // no cargo target compiles that file: the crate manifest that would give it
    // a home is W17's, and W17 is wave 5. Until it lands, this is what stops
    // the fixture rotting — it decodes the committed bytes with the same
    // `FrameReader` the desktop points at a real `stratum serve`, and parses
    // every payload as the frozen `EngineEvent`.
    //
    // A compile-time include, not a runtime read: §8.4 bans the filesystem from
    // every line of this crate's source — `xtask layering` greps for the path
    // literally, comments included — and a wasm build has no filesystem to read
    // a fixture from anyway.
    // -----------------------------------------------------------------------

    const SCENARIO_A: &[u8] = include_bytes!("../../../tests/fixtures/mock/scenario_a.msgpack");

    /// Reads the two stream-level fields back through the *wire* shape rather
    /// than a fourteen-arm `match`: `EngineEvent` is internally tagged on
    /// `event`, so a payload that decodes here is one whose discriminant and
    /// `seq` are where every consumer of §7 expects them, independently of
    /// whether the variant body still parses.
    #[derive(Deserialize)]
    struct EventProbe {
        event: String,
        seq: u64,
    }

    #[test]
    fn the_committed_mock_stream_is_a_section_10_event_stream() {
        let mut rd = FrameReader::new();
        let mut events: Vec<EngineEvent> = Vec::new();
        let mut tags: Vec<String> = Vec::new();

        // Seven bytes at a time: the fixture must decode identically when a
        // read lands mid-header and when it lands on a frame boundary.
        for chunk in SCENARIO_A.chunks(7) {
            rd.feed(chunk);
            while let Some(f) = rd.next_frame().expect("the fixture is well framed") {
                assert_eq!(f.kind, FrameKind::Event, "the mock sends only events");
                assert_eq!(f.corr, CORR_UNSOLICITED, "an event answers nothing");

                let probe: EventProbe =
                    rmp_serde::from_slice(&f.payload).expect("an `event` tag and a `seq`");
                assert_eq!(
                    probe.seq as usize,
                    events.len() + 1,
                    "§7: seq is strictly increasing by one across the stream"
                );

                events.push(
                    rmp_serde::from_slice(&f.payload).expect("a frozen EngineEvent, field-named"),
                );
                tags.push(probe.event);
            }
        }
        rd.end_of_stream()
            .expect("the fixture ends on a frame boundary, not mid-frame");

        // Scenario A is `sysuse auto`, `summarize`, `regress` (the fixture's
        // README): three runs, each opened and closed exactly once.
        let started = tags.iter().filter(|t| *t == "run_started").count();
        let finished = tags.iter().filter(|t| *t == "run_finished").count();
        assert_eq!((started, finished), (3, 3), "three runs, opened and closed");
        // The engine announces itself before any run, and the stream ends on a
        // closed run — a mock that ended mid-run would teach W12-W16 that a
        // dangling `run_started` is normal.
        assert_eq!(tags.first().map(String::as_str), Some("engine_health"));
        assert_eq!(tags.last().map(String::as_str), Some("run_finished"));

        for ev in &events {
            if let EngineEvent::RunStarted { schema, .. } = ev {
                assert_eq!(
                    *schema, STREAM_SCHEMA,
                    "a fixture from an older schema would teach the frontend a dead shape"
                );
            }
        }
    }
}
