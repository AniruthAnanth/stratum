//! `stratum serve` — wiring W07's engine protocol to a backend.
//!
//! W07 built `crate::serve` complete and tested and then had nothing to plug
//! into it: `serve()` takes an `EngineBackend`, and the only real backend is
//! `stratum-exec`'s session worker. Until this file existed, every item in that
//! module was unreachable and `main.rs` carried an `#[allow(dead_code)]` on
//! `mod serve;` to keep `-D warnings` green. **That allow is gone**, and it is
//! gone because this file consumes the module rather than because the lint was
//! waived: `--protocol json` reaches `Protocol::Json`, [`CliBackend`] calls
//! `EventSink::emit`, and `--print-schema` uses `NdjsonWriter::{response,
//! lines_written}` — the exact four items W07's manifest note listed.
//!
//! # What the backend can honestly answer today
//!
//! `Hello` and `Shutdown` are answered by `serve::dispatch` itself, on purpose:
//! the schema check is a property of the protocol, and an engine that let a
//! backend get it wrong would fail version-skew detection. Everything else needs
//! the engine, which is not linked (see `cmd/mod.rs`), so it is answered with
//! `EngineError::Internal` naming the missing crate — a *protocol-level* answer
//! the desktop can render, rather than a closed pipe it has to guess about.
//!
//! `Blocks` is deliberately NOT answered from `stratum_parse` even though the
//! segmentation is right there: `EngineResponse::BlockMap` carries `BlockId`s,
//! and CONTRACTS §2 says "**`BlockId` is allocated only by `stratum-exec`**". A
//! CLI that minted ids the engine would later mint differently would corrupt
//! exactly the id-drift comparison §7.2 exists to catch.

use std::io::Write;

use serde::de::DeserializeOwned;
use serde::Serialize;
use stratum_proto::engine::{
    EngineError, EngineEvent, EngineHealth, EngineRequest, EngineResponse, STREAM_SCHEMA,
};

use crate::cli::{ExitCode, Protocol, ServeArgs};
use crate::cmd::{CmdError, ENGINE_ABSENT};
use crate::serve::ndjson::NdjsonWriter;
use crate::serve::{EngineBackend, EventSink, ServeOptions};

/// The backend `stratum serve` installs while no engine is linked.
pub struct CliBackend {
    seq: u64,
    announced: bool,
}

impl CliBackend {
    /// A fresh backend.
    #[must_use]
    pub fn new() -> Self {
        Self {
            seq: 0,
            announced: false,
        }
    }
}

impl Default for CliBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl EngineBackend for CliBackend {
    fn handle(&mut self, _req: EngineRequest, events: &EventSink) -> EngineResponse {
        if !self.announced {
            self.announced = true;
            // `Stopped`, not `Ready` and not `Crashed`: nothing is running and
            // nothing died. A client that renders the health banner gets the
            // truth on its first request instead of a silence.
            events.emit(EngineEvent::EngineHealth {
                seq: self.seq,
                health: EngineHealth::Stopped,
            });
            self.seq += 1;
        }
        EngineResponse::Error(EngineError::Internal {
            message: ENGINE_ABSENT.to_owned(),
        })
    }

    fn hello(&self) -> (String, String) {
        (
            format!("stratum {} (no engine linked)", env!("CARGO_PKG_VERSION")),
            std::env::consts::ARCH.to_owned(),
        )
    }
}

/// The §7.1 method registry, as `--print-schema` emits it.
#[derive(Clone, Debug, Serialize)]
pub struct SchemaDoc {
    /// `STREAM_SCHEMA`.
    pub schema: u32,
    /// The envelope, spelled out, because a client that has this document should
    /// not also need the prose.
    pub envelope: &'static str,
    /// Request tags, i.e. the values of the `req` field.
    pub requests: Vec<String>,
    /// Response tags.
    pub responses: Vec<String>,
    /// Event tags.
    pub events: Vec<String>,
}

impl SchemaDoc {
    /// Build it.
    ///
    /// # Errors
    /// [`CmdError::Internal`] if the names cannot be recovered from the types —
    /// see [`variant_tags`] for why that is the failure mode rather than a
    /// silently short list.
    pub fn gather() -> Result<Self, CmdError> {
        Ok(SchemaDoc {
            schema: STREAM_SCHEMA,
            envelope: r#"{"v":1,"t":"req"|"resp"|"event","corr":u32?,"body":{…}}"#,
            requests: variant_tags::<EngineRequest>("req")?,
            responses: variant_tags::<EngineResponse>("resp")?,
            events: variant_tags::<EngineEvent>("event")?,
        })
    }
}

/// The variant tags of an internally-tagged enum, **recovered from the type**.
///
/// CONTRACTS §7.1: "there is no `method` field … and no separate name registry
/// to keep in sync: **the Rust variant name IS the method name**." A hand-written
/// list here would be exactly the registry that sentence says does not exist,
/// and it would be wrong the first time somebody adds a request.
///
/// So the list is asked of `serde`: deserialising an impossible tag makes the
/// generated `Deserialize` report `unknown variant …, expected one of …`, and
/// that enumeration is generated from the enum itself. It is a narrow trick and
/// it is fenced by `tests::the_registry_is_recovered_from_the_types`: if
/// serde's message shape ever changes, this returns an error and `--print-schema`
/// fails loudly rather than emitting a plausible short list.
///
/// # Errors
/// [`CmdError::Internal`] if no names could be recovered.
pub fn variant_tags<T: DeserializeOwned>(tag: &str) -> Result<Vec<String>, CmdError> {
    // `\u0000` as a JSON *escape*, not as a raw byte: a literal control
    // character in a string is a parse error, and a parse error is not the
    // unknown-variant error this depends on. NUL is chosen because no variant
    // name can contain it.
    let probe = format!(r#"{{"{tag}":"\u0000stratum-probe"}}"#);
    let Err(e) = serde_json::from_str::<T>(&probe) else {
        return Err(CmdError::Internal(format!(
            "the probe tag was accepted as a `{tag}` variant"
        )));
    };
    let message = e.to_string();
    let Some(list) = message.split("expected one of ").nth(1) else {
        return Err(CmdError::Internal(format!(
            "serde no longer enumerates variants in its error; \
             `--print-schema` cannot derive the {tag} registry from the type. \
             Message was: {message}"
        )));
    };
    // The names are quoted, and the message does NOT end with the last one:
    // serde appends " at line 1 column N". Splitting on ", " therefore glues
    // that tail onto the final variant and a `contains(' ')` filter silently
    // drops it — which is how `shutdown`, the last variant of `EngineRequest`,
    // went missing from a registry that otherwise looked complete. Taking the
    // odd fields of a split on the quote character extracts exactly the quoted
    // spans and nothing between them.
    let quote = if list.contains('`') { '`' } else { '\'' };
    let names: Vec<String> = list
        .split(quote)
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .filter(|s| !s.is_empty() && !s.contains(' '))
        .collect();
    if names.is_empty() {
        return Err(CmdError::Internal(format!(
            "recovered no `{tag}` variant names from: {message}"
        )));
    }
    Ok(names)
}

/// `stratum serve`.
///
/// # Errors
/// [`CmdError::Io`] on a transport failure, [`CmdError::Internal`] if
/// `--print-schema` cannot build the registry.
pub fn serve(
    args: &ServeArgs,
    out: &mut impl Write,
    _err: &mut impl Write,
) -> Result<ExitCode, CmdError> {
    if args.print_schema {
        let doc = SchemaDoc::gather()?;
        let mut w = NdjsonWriter::new(out);
        // A §7.1 response with `corr` 0: it answers the command line rather than
        // a request that came in over the wire, and 0 is not a correlation id
        // any client will have issued.
        w.response(0, &doc).map_err(|source| CmdError::Io {
            path: camino::Utf8PathBuf::from("<stdout>"),
            source,
        })?;
        tracing::debug!(lines = w.lines_written(), "schema written");
        return Ok(ExitCode::Success);
    }

    let opts = ServeOptions {
        protocol: match args.protocol {
            Protocol::Msgpack => crate::serve::Protocol::MessagePack,
            Protocol::Json => crate::serve::Protocol::Json,
        },
        // On by default: an engine that outlives the IDE holds a redb lock and a
        // dataset in RAM forever (design 08 §5.6).
        watch_parent: !args.no_watch_parent,
    };
    // stdin/stdout are taken directly, not through `out`: `serve` owns the
    // transport for the whole process lifetime, and CONTRACTS §7 g4's "stdout
    // carries only the stream" is that ownership.
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    crate::serve::serve(stdin.lock(), stdout.lock(), opts, CliBackend::new()).map_err(|e| {
        CmdError::Io {
            path: camino::Utf8PathBuf::from("<stdio>"),
            source: std::io::Error::other(e.to_string()),
        }
    })?;
    Ok(ExitCode::Success)
}

#[cfg(test)]
mod tests {
    use stratum_proto::frame::{Envelope, WireTag};
    use stratum_proto::ids::SessionId;

    use super::*;
    use crate::serve::Protocol as WireProtocol;

    /// The four items W07's manifest note said would stop being dead the day
    /// this file wired the backend. If any of them stops being used, `-D
    /// warnings` fails on `mod serve;` again and the allow is the only fix — so
    /// this test names them.
    #[test]
    fn the_four_items_w07_left_unreachable_are_reachable() {
        // Protocol::Json
        assert_eq!(
            ServeOptions {
                protocol: WireProtocol::Json,
                watch_parent: false,
            }
            .protocol,
            WireProtocol::Json
        );
        // NdjsonWriter::{response, lines_written}
        let mut buf = Vec::new();
        let mut w = NdjsonWriter::new(&mut buf);
        w.response(0, EngineResponse::Ok).unwrap();
        assert_eq!(w.lines_written(), 1);
        // EventSink::emit — exercised by CliBackend below.
    }

    /// §7.1's registry, derived from the types rather than transcribed. If serde
    /// stops enumerating variants, this fails here rather than shipping a
    /// plausible short list to a client that trusts it.
    #[test]
    fn the_registry_is_recovered_from_the_types() {
        let doc = SchemaDoc::gather().expect("serde enumerates its variants");
        assert_eq!(doc.schema, STREAM_SCHEMA);
        for want in [
            "hello",
            "shutdown",
            "exec_submit",
            "data_page",
            "ai_context",
        ] {
            assert!(
                doc.requests.contains(&want.to_owned()),
                "{:?}",
                doc.requests
            );
        }
        for want in [
            "run_started",
            "block_started",
            "block_finished",
            "run_finished",
        ] {
            assert!(doc.events.contains(&want.to_owned()), "{:?}", doc.events);
        }
        assert!(doc.responses.contains(&"ok".to_owned()));
        assert!(doc.requests.len() > 20, "{}", doc.requests.len());
        // The registry has no JSON-RPC vocabulary in it (A9).
        for banned in ["jsonrpc", "method", "params"] {
            assert!(!doc.requests.iter().any(|r| r == banned));
        }
    }

    #[test]
    fn print_schema_writes_one_section_7_1_response_and_exits_zero() {
        let mut out = Vec::new();
        let args = ServeArgs {
            protocol: Protocol::Msgpack,
            stdio: false,
            print_schema: true,
            no_watch_parent: true,
        };
        assert_eq!(
            serve(&args, &mut out, &mut Vec::new()).unwrap(),
            ExitCode::Success
        );
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().count(), 1);
        let v: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
        assert_eq!(v["v"], 1);
        assert_eq!(v["t"], "resp");
        assert_eq!(v["corr"], 0);
        assert!(v["body"]["requests"].as_array().unwrap().len() > 20);
    }

    /// The backend answers at the PROTOCOL level rather than closing the pipe,
    /// so the desktop can render "the engine is not in this build" instead of
    /// guessing at an EOF. It also emits exactly one health event.
    #[test]
    fn the_backend_answers_every_request_it_cannot_serve() {
        let mut input = Vec::new();
        {
            let mut w = NdjsonWriter::new(&mut input);
            w.write(&Envelope::req(
                1,
                EngineRequest::Hello {
                    client: "test".to_owned(),
                    schema: STREAM_SCHEMA,
                },
            ))
            .unwrap();
            w.write(&Envelope::req(
                2,
                EngineRequest::Status {
                    session: SessionId(1),
                },
            ))
            .unwrap();
            w.write(&Envelope::req(
                3,
                EngineRequest::Status {
                    session: SessionId(1),
                },
            ))
            .unwrap();
            w.write(&Envelope::req(4, EngineRequest::Shutdown)).unwrap();
        }

        let mut out = Vec::new();
        crate::serve::serve(
            std::io::Cursor::new(input),
            &mut out,
            ServeOptions {
                protocol: WireProtocol::Json,
                watch_parent: false,
            },
            CliBackend::new(),
        )
        .expect("a clean stdio session");

        let lines: Vec<serde_json::Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        // Hello is answered by the protocol, not the backend.
        assert_eq!(lines[0]["body"]["resp"], "hello");
        assert_eq!(lines[0]["body"]["schema"], STREAM_SCHEMA);
        // The first Status: an error naming the missing crate, then one health
        // event.
        assert_eq!(lines[1]["body"]["resp"], "error");
        assert!(lines[1]["body"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("stratum-exec"));
        assert_eq!(lines[2]["t"], "event");
        // `EngineHealth` is itself an internally-tagged enum, so the event's
        // `health` field is an object and not a bare string.
        assert_eq!(lines[2]["body"]["health"]["health"], "stopped");
        // The second Status: answered, and NOT announced again.
        assert_eq!(lines[3]["body"]["resp"], "error");
        assert_eq!(lines[4]["body"]["resp"], "ok", "Shutdown");
        assert_eq!(lines.len(), 5, "no stray health events");
    }

    /// Version skew is the protocol's job, not the backend's.
    #[test]
    fn a_client_on_another_schema_is_told_so() {
        let mut input = Vec::new();
        {
            let mut w = NdjsonWriter::new(&mut input);
            w.write(&Envelope::req(
                1,
                EngineRequest::Hello {
                    client: "future".to_owned(),
                    schema: STREAM_SCHEMA + 9,
                },
            ))
            .unwrap();
        }
        let mut out = Vec::new();
        crate::serve::serve(
            std::io::Cursor::new(input),
            &mut out,
            ServeOptions {
                protocol: WireProtocol::Json,
                watch_parent: false,
            },
            CliBackend::new(),
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("schema_mismatch"), "{text}");
        assert_eq!(
            WireTag::Resp,
            serde_json::from_value(
                serde_json::from_str::<serde_json::Value>(text.lines().next().unwrap()).unwrap()
                    ["t"]
                    .clone()
            )
            .unwrap()
        );
    }
}
