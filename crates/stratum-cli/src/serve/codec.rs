//! CONTRACTS.md §10 — the engine side of the framed-MessagePack transport.
//!
//! The desktop's half is `apps/desktop/src-tauri/src/transport.rs`; both are
//! W07's, and both call `stratum_proto::frame` rather than laying out bytes
//! themselves. `to_vec_named` is mandatory (§10): a positional encoding turns
//! any struct change into a silent wire break.

use std::io::{Read, Write};

use serde::Serialize;
use stratum_proto::engine::{EngineEvent, EngineRequest, EngineResponse};
use stratum_proto::frame::{
    encode_frame, FrameError, FrameKind, FrameReader, Ping, CORR_UNSOLICITED,
};

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("framing: {0}")]
    Frame(#[from] FrameError),
    #[error("encoding: {0}")]
    Encode(#[from] rmp_serde::encode::Error),
    #[error("decoding: {0}")]
    Decode(#[from] rmp_serde::decode::Error),
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("the desktop sent a {0:?} frame; the engine only accepts requests and pings")]
    UnexpectedKind(FrameKind),
}

/// What arrived from the desktop.
#[derive(Clone, PartialEq, Debug)]
pub enum Incoming {
    Request { corr: u32, req: Box<EngineRequest> },
    Ping(Ping),
}

/// Blocking reader over the engine's stdin. Blocking on purpose: the engine's
/// control thread (ARCHITECTURE §4) is a thread, not a task, and an async
/// runtime here would buy nothing and cost stack traces.
pub struct FrameSource<R: Read> {
    src: R,
    reader: FrameReader,
    chunk: Vec<u8>,
}

impl<R: Read> FrameSource<R> {
    pub fn new(src: R) -> Self {
        Self {
            src,
            reader: FrameReader::new(),
            chunk: vec![0_u8; 64 * 1024],
        }
    }

    /// The next message, or `Ok(None)` at a clean end of stream.
    ///
    /// # Errors
    /// A framing or decode error, or [`FrameError::Truncated`] if the desktop
    /// died mid-write — which the engine answers by exiting, not by guessing.
    pub fn next_message(&mut self) -> Result<Option<Incoming>, CodecError> {
        loop {
            if let Some(frame) = self.reader.next_frame()? {
                return Ok(Some(match frame.kind {
                    FrameKind::Request => Incoming::Request {
                        corr: frame.corr,
                        req: Box::new(rmp_serde::from_slice(&frame.payload)?),
                    },
                    FrameKind::Ping => Incoming::Ping(rmp_serde::from_slice(&frame.payload)?),
                    other => return Err(CodecError::UnexpectedKind(other)),
                }));
            }
            let n = self.src.read(&mut self.chunk)?;
            if n == 0 {
                self.reader.end_of_stream()?;
                return Ok(None);
            }
            self.reader.feed(&self.chunk[..n]);
        }
    }
}

/// Writer over the engine's stdout. One flush per frame: an `Output` event that
/// waits for the next write is a UI that looks hung.
pub struct FrameSink<W: Write> {
    out: W,
    buf: Vec<u8>,
}

impl<W: Write> FrameSink<W> {
    pub fn new(out: W) -> Self {
        Self {
            out,
            buf: Vec::with_capacity(64 * 1024),
        }
    }

    /// # Errors
    /// Encoding or i/o failure.
    pub fn response(&mut self, corr: u32, resp: &EngineResponse) -> Result<(), CodecError> {
        self.send(FrameKind::Response, corr, resp)
    }

    /// # Errors
    /// Encoding or i/o failure.
    pub fn event(&mut self, ev: &EngineEvent) -> Result<(), CodecError> {
        self.send(FrameKind::Event, CORR_UNSOLICITED, ev)
    }

    /// # Errors
    /// Encoding or i/o failure.
    pub fn pong(&mut self, corr: u32, nonce: u64) -> Result<(), CodecError> {
        self.send(FrameKind::Ping, corr, &Ping { nonce, pong: true })
    }

    fn send<T: Serialize>(
        &mut self,
        kind: FrameKind,
        corr: u32,
        body: &T,
    ) -> Result<(), CodecError> {
        let payload = rmp_serde::to_vec_named(body)?;
        self.buf.clear();
        encode_frame(kind, corr, &payload, &mut self.buf)?;
        self.out.write_all(&self.buf)?;
        self.out.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use stratum_proto::engine::{EngineHealth, STREAM_SCHEMA};
    use stratum_proto::ids::SessionId;

    use super::*;

    #[test]
    fn requests_pings_and_responses_cross_the_same_wire() {
        // Desktop → engine.
        let mut wire = Vec::new();
        let req = EngineRequest::Hello {
            client: "stratum-desktop".to_owned(),
            schema: STREAM_SCHEMA,
        };
        encode_frame(
            FrameKind::Request,
            41,
            &rmp_serde::to_vec_named(&req).unwrap(),
            &mut wire,
        )
        .unwrap();
        encode_frame(
            FrameKind::Ping,
            0,
            &rmp_serde::to_vec_named(&Ping {
                nonce: 9,
                pong: false,
            })
            .unwrap(),
            &mut wire,
        )
        .unwrap();

        let mut src = FrameSource::new(std::io::Cursor::new(wire));
        assert_eq!(
            src.next_message().unwrap().unwrap(),
            Incoming::Request {
                corr: 41,
                req: Box::new(req)
            }
        );
        assert_eq!(
            src.next_message().unwrap().unwrap(),
            Incoming::Ping(Ping {
                nonce: 9,
                pong: false
            })
        );
        assert!(src.next_message().unwrap().is_none());

        // Engine → desktop.
        let mut out = Vec::new();
        {
            let mut sink = FrameSink::new(&mut out);
            sink.response(41, &EngineResponse::Ok).unwrap();
            sink.event(&EngineEvent::EngineHealth {
                seq: 1,
                health: EngineHealth::Ready,
            })
            .unwrap();
            sink.pong(0, 9).unwrap();
        }
        let mut rd = FrameReader::new();
        rd.feed(&out);
        let kinds: Vec<_> = std::iter::from_fn(|| rd.next_frame().unwrap())
            .map(|f| f.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![FrameKind::Response, FrameKind::Event, FrameKind::Ping]
        );
        rd.end_of_stream().unwrap();
    }

    #[test]
    fn an_event_arriving_at_the_engine_is_a_desync_not_a_message() {
        let mut wire = Vec::new();
        encode_frame(FrameKind::Event, 0, b"\x90", &mut wire).unwrap();
        let mut src = FrameSource::new(std::io::Cursor::new(wire));
        assert!(matches!(
            src.next_message(),
            Err(CodecError::UnexpectedKind(FrameKind::Event))
        ));
    }

    #[test]
    fn a_desktop_that_dies_mid_frame_is_an_error() {
        let mut wire = Vec::new();
        encode_frame(
            FrameKind::Request,
            1,
            &rmp_serde::to_vec_named(&EngineRequest::Status {
                session: SessionId(1),
            })
            .unwrap(),
            &mut wire,
        )
        .unwrap();
        wire.truncate(wire.len() - 2);
        let mut src = FrameSource::new(std::io::Cursor::new(wire));
        assert!(matches!(
            src.next_message(),
            Err(CodecError::Frame(FrameError::Truncated { .. }))
        ));
    }
}
