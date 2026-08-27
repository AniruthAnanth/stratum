//! The CONTRACTS §11 command surface, and the `stratum-asset://` handler glue.
//!
//! Every command is `async` — there is no synchronous IPC anywhere in the
//! product — and every one is a thin translation: parse the webview's
//! arguments, forward to W07's [`crate::engine_host`]/[`crate::transport`], and
//! shape the reply the way §11 spells it (camelCase field names). No statistics
//! happen here; `cargo xtask layering` proves this crate cannot even link the
//! engine (ARCHITECTURE §8.2).
//!
//! **Session defaulting.** Several frontend call sites omit `session`
//! (`variables_list { frame }`), because a single-project window has exactly
//! one. Commands therefore take `Option<SessionId>` and resolve through
//! [`HostState::session_or_current`]; a genuinely ambiguous host (two open
//! sessions, no explicit id) answers with an error rather than a guess.
//!
//! **What is deliberately not here.** The `ai_*` commands (W21's surface) and
//! `tauri-specta` binding generation are documented gaps — see this unit's
//! return. Nothing in this file pretends: an absent command fails the invoke
//! with "unknown command", which the frontend already treats as "no host".

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use stratum_platform::menus::{ActionId, KeymapPreset};
use stratum_proto::block::BlockMap;
use stratum_proto::complete::CompletionEnv;
use stratum_proto::data::{QuickSummary, VariableInfo};
use stratum_proto::engine::{
    EngineRequest, EngineResponse, GraphFormat, InlineResultsMode, OrderSpec, SessionMode,
};
use stratum_proto::exec::{CancelLevel, RunIntent, RunPlan};
use stratum_proto::ids::{DocumentId, Edit, OrderId, ResultId, RunId, SessionId};
use stratum_proto::repro::ReproReport;
use stratum_proto::result::{AssetRef, ResultEnvelope, StyledRun};
use stratum_proto::session::{LogSearchOpts, SessionStatus};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{Manager, Runtime, State};

use crate::asset::{self, AssetKind};
use crate::engine_host::EngineHost;
use crate::transport::TransportError;
use crate::windows::{HostEvent, SessionRegistry, SubscribeAck};

/// One bounded engine round trip. Not a performance budget (ADR-017): the
/// deadline that turns a wedged engine into an error the card can show.
const REQUEST_DEADLINE: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Host state
// ---------------------------------------------------------------------------

/// A document the host has open — the text authority for `doc_save`.
pub struct HostDoc {
    pub path: Option<Utf8PathBuf>,
    pub text: String,
    pub version: u64,
    pub eol: &'static str,
    pub bom: bool,
    pub owner_label: String,
}

pub struct HostState {
    pub engine: Arc<EngineHost>,
    pub registry: Arc<SessionRegistry>,
    /// Lowercase hex of the §10.2 `OnceLock<[u8; 32]>` token.
    pub token_hex: String,
    pub engine_version: Mutex<String>,
    pub docs: Mutex<HashMap<DocumentId, HostDoc>>,
    next_doc: AtomicU32,
    /// Where layout/keymap overlays and workspace state live.
    pub config_dir: std::path::PathBuf,
    /// `--selftest`: `app_ready` flips this; the watchdog exits 0 on it.
    pub ready: Arc<AtomicBool>,
    /// `--selftest`'s wakeup — a blocking recv with a deadline, not a poll.
    pub ready_tx: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    /// Segments this host has mapped, so `session_close` can detach them.
    pub attached_segments: Mutex<Vec<u32>>,
}

impl HostState {
    pub fn new(
        engine: Arc<EngineHost>,
        registry: Arc<SessionRegistry>,
        config_dir: std::path::PathBuf,
    ) -> Self {
        Self {
            engine,
            registry,
            token_hex: token_hex().to_owned(),
            engine_version: Mutex::new(String::new()),
            docs: Mutex::new(HashMap::new()),
            next_doc: AtomicU32::new(1),
            config_dir,
            ready: Arc::new(AtomicBool::new(false)),
            ready_tx: Mutex::new(None),
            attached_segments: Mutex::new(Vec::new()),
        }
    }

    fn session_or_current(&self, session: Option<SessionId>) -> Result<SessionId, String> {
        session
            .or_else(|| self.registry.only_session())
            .ok_or_else(|| "no session is open".to_owned())
    }

    fn alloc_doc(&self) -> DocumentId {
        DocumentId(self.next_doc.fetch_add(1, Ordering::Relaxed))
    }

    // `pub(crate)`: `file_open`'s `use` submit goes through the same bounded
    // round trip as every §11 command, not a second transport path.
    pub(crate) async fn request(&self, req: EngineRequest) -> Result<EngineResponse, String> {
        let tx = self.engine.transport().await;
        match tokio::time::timeout(REQUEST_DEADLINE, tx.request(req)).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(e)) => Err(engine_error(&e)),
            Err(_) => Err("the engine did not answer within 30 s".to_owned()),
        }
    }
}

fn engine_error(e: &TransportError) -> String {
    format!("engine: {e}")
}

/// The process-wide asset token (CONTRACTS §10.2). Generated once, handed to
/// the webview through `app_ready`'s reply and checked by the asset handler.
static ASSET_TOKEN: OnceLock<[u8; 32]> = OnceLock::new();

pub fn token_hex() -> &'static str {
    static HEX: OnceLock<String> = OnceLock::new();
    HEX.get_or_init(|| {
        let token = ASSET_TOKEN.get_or_init(|| {
            let mut bytes = [0u8; 32];
            // Falling back to a zeroed token would silently disable the check;
            // refuse to start instead. getrandom only fails on broken hosts.
            getrandom::fill(&mut bytes).expect("no entropy source for the asset token");
            bytes
        });
        let mut out = String::with_capacity(64);
        for b in token {
            use std::fmt::Write as _;
            let _ = write!(out, "{b:02x}");
        }
        out
    })
}

// ---------------------------------------------------------------------------
// Session commands
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionOpenedReply {
    pub session: SessionId,
    pub epoch: u32,
    pub engine_version: String,
}

#[tauri::command]
pub async fn session_open<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, HostState>,
    project_root: String,
    mode: Option<SessionMode>,
) -> Result<SessionOpenedReply, String> {
    let mode = mode.unwrap_or(SessionMode::Interactive);
    let tx = state.engine.transport().await;
    // The §7 handshake is a round trip like any other and gets the same
    // deadline: an engine that spawned but never speaks (hung, or not a Stratum
    // engine at all) must become an error the window can show, not a
    // `session_open` that never resolves behind a window that says nothing.
    let engine_name = match tokio::time::timeout(
        REQUEST_DEADLINE,
        crate::transport::handshake(&tx, "stratum-desktop"),
    )
    .await
    {
        Ok(Ok(name)) => name,
        Ok(Err(e)) => return Err(engine_error(&e)),
        Err(_elapsed) => {
            return Err("the engine did not answer the handshake within 30 s".to_owned())
        }
    };
    *state.engine_version.lock().expect("engine_version") = engine_name.clone();
    let resp = state
        .request(EngineRequest::SessionOpen {
            project_root: Utf8PathBuf::from(&project_root),
            mode,
            config: stratum_proto::session::SessionConfigWire {
                cwd: None,
                seed: None,
                linesize: 80,
                level: 95.0,
                varabbrev: true,
                more: false,
                max_memory_bytes: None,
                ado_personal: mode == SessionMode::Interactive,
                write_sandbox: None,
            },
        })
        .await?;
    match resp {
        EngineResponse::SessionOpened { session, epoch } => {
            state.registry.open(session, epoch, mode);
            state.registry.bind_label(session, window.label());
            // OS open requests queued before boot were waiting for exactly
            // this moment: a `.dta` needs a session to submit `use` into, a
            // `.do` needs a booted webview to route to — and this window,
            // having just invoked `session_open`, is provably both.
            crate::file_open::drain(window.app_handle(), session, window.label());
            Ok(SessionOpenedReply {
                session,
                epoch: epoch.0,
                engine_version: engine_name,
            })
        }
        EngineResponse::Error(e) => Err(e.to_string()),
        _ => Err("unexpected engine reply to session_open".to_owned()),
    }
}

#[tauri::command]
pub async fn session_subscribe<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, HostState>,
    session: SessionId,
    channel: Channel<InvokeResponseBody>,
) -> Result<SubscribeAck, String> {
    state
        .registry
        .subscribe(session, window.label(), channel)
        .ok_or_else(|| format!("session {session} is not open"))
}

#[tauri::command]
pub async fn session_close(
    state: State<'_, HostState>,
    session: Option<SessionId>,
) -> Result<(), String> {
    let session = state.session_or_current(session)?;
    let _ = state.request(EngineRequest::SessionClose { session }).await;
    state.registry.close(session);
    // Unmap the bulk segments this session's pages and graphs were served
    // from; the engine retires the files on its side.
    let attached: Vec<u32> = state
        .attached_segments
        .lock()
        .map(|mut v| std::mem::take(&mut *v))
        .unwrap_or_default();
    for segment in attached {
        state.engine.bulk.detach(segment);
    }
    Ok(())
}

#[tauri::command]
pub async fn session_status(
    state: State<'_, HostState>,
    session: Option<SessionId>,
) -> Result<SessionStatus, String> {
    let session = state.session_or_current(session)?;
    if let Ok(EngineResponse::Status { status }) =
        state.request(EngineRequest::Status { session }).await
    {
        return Ok(status);
    }
    // The engine could not answer (or is the mock, which acks): the registry's
    // event-fed status is the honest fallback and is what the banner shows.
    state
        .registry
        .status(session)
        .ok_or_else(|| "no such session".to_owned())
}

// ---------------------------------------------------------------------------
// Documents
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentOpenedReply {
    pub doc: DocumentId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub text: String,
    pub version: u64,
    pub block_map: Option<BlockMap>,
    pub sidecar: Value,
    pub eol: &'static str,
    pub bom: bool,
}

fn detect_eol_bom(bytes: &[u8]) -> (&'static str, bool, String) {
    let bom = bytes.starts_with(&[0xEF, 0xBB, 0xBF]);
    let body = if bom { &bytes[3..] } else { bytes };
    let text = String::from_utf8_lossy(body).into_owned();
    let eol = if text.contains("\r\n") { "crlf" } else { "lf" };
    // The engine and the editor speak LF; the recorded EOL is reproduced by
    // `doc_save` byte-for-byte (A24).
    (eol, bom, text.replace("\r\n", "\n"))
}

#[tauri::command]
pub async fn doc_open<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, HostState>,
    session: Option<SessionId>,
    path: Option<String>,
    text: Option<String>,
) -> Result<DocumentOpenedReply, String> {
    let session = state.session_or_current(session)?;
    let (eol, bom, text) = match (&path, text) {
        (_, Some(t)) => ("lf", false, t),
        (Some(p), None) => {
            let bytes = std::fs::read(p).map_err(|e| format!("reading {p}: {e}"))?;
            detect_eol_bom(&bytes)
        }
        (None, None) => ("lf", false, String::new()),
    };
    let doc = state.alloc_doc();
    let resp = state
        .request(EngineRequest::DocOpen {
            session,
            doc,
            path: path.as_deref().map(Utf8PathBuf::from),
            text: text.clone(),
        })
        .await?;
    let block_map = match resp {
        EngineResponse::BlockMap(map) => Some(map),
        _ => None,
    };
    state.docs.lock().expect("docs").insert(
        doc,
        HostDoc {
            path: path.as_deref().map(Utf8PathBuf::from),
            text: text.clone(),
            version: 1,
            eol,
            bom,
            owner_label: window.label().to_owned(),
        },
    );
    Ok(DocumentOpenedReply {
        doc,
        path,
        text,
        version: 1,
        block_map,
        sidecar: Value::Null,
        eol,
        bom,
    })
}

fn apply_edits(text: &mut String, edits: &[Edit]) -> Result<(), String> {
    // Spans are UTF-16-agnostic on this side: CONTRACTS §2 sends byte offsets
    // over the wire. Applied back-to-front so earlier spans stay valid.
    let mut sorted: Vec<&Edit> = edits.iter().collect();
    sorted.sort_by_key(|e| std::cmp::Reverse(e.span.start));
    for edit in sorted {
        let start = edit.span.start as usize;
        let end = edit.span.end as usize;
        if start > end || end > text.len() {
            return Err(format!("edit span {start}..{end} is outside the document"));
        }
        text.replace_range(start..end, &edit.text);
    }
    Ok(())
}

#[tauri::command]
pub async fn doc_change(
    state: State<'_, HostState>,
    session: Option<SessionId>,
    doc: DocumentId,
    version: u64,
    edits: Vec<Edit>,
) -> Result<(), String> {
    let session = state.session_or_current(session)?;
    {
        let mut docs = state.docs.lock().expect("docs");
        let entry = docs
            .get_mut(&doc)
            .ok_or_else(|| format!("document {doc} is not open"))?;
        apply_edits(&mut entry.text, &edits)?;
        entry.version = version;
    }
    // Fire-and-forget: the engine answers with a BlockMapChanged EVENT.
    let tx = state.engine.transport().await;
    tx.notify(EngineRequest::DocChange {
        session,
        doc,
        version,
        edits,
    })
    .map_err(|e| engine_error(&e))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedAck {
    pub path: String,
    pub text_hash: String,
    pub eol: &'static str,
    pub bom: bool,
}

#[tauri::command]
pub async fn doc_save(
    state: State<'_, HostState>,
    session: Option<SessionId>,
    doc: DocumentId,
) -> Result<SavedAck, String> {
    let session = state.session_or_current(session)?;
    let (path, bytes, eol, bom) = {
        let docs = state.docs.lock().expect("docs");
        let entry = docs
            .get(&doc)
            .ok_or_else(|| format!("document {doc} is not open"))?;
        let path = entry
            .path
            .clone()
            .ok_or_else(|| "document has no path; use Save As".to_owned())?;
        // A24: reproduce the recorded EOL/BOM byte-for-byte.
        let body = if entry.eol == "crlf" {
            entry.text.replace('\n', "\r\n")
        } else {
            entry.text.clone()
        };
        let mut bytes = Vec::with_capacity(body.len() + 3);
        if entry.bom {
            bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        }
        bytes.extend_from_slice(body.as_bytes());
        (path, bytes, entry.eol, entry.bom)
    };
    std::fs::write(path.as_std_path(), &bytes).map_err(|e| format!("writing {path}: {e}"))?;
    // A cheap content fingerprint (not a CodeHash — that is token-stream-keyed
    // and engine-owned). FNV-1a over the saved bytes.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in &bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    state.registry.host_event(
        session,
        &HostEvent::DocumentSaved {
            doc,
            path: path.to_string(),
        },
    );
    Ok(SavedAck {
        path: path.to_string(),
        text_hash: format!("{hash:016x}"),
        eol,
        bom,
    })
}

#[tauri::command]
pub async fn doc_close(
    state: State<'_, HostState>,
    session: Option<SessionId>,
    doc: DocumentId,
) -> Result<(), String> {
    let session = state.session_or_current(session)?;
    state.docs.lock().expect("docs").remove(&doc);
    let _ = state
        .request(EngineRequest::DocClose { session, doc })
        .await;
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimReply {
    pub owner_label: String,
}

/// A `.do` is editable in exactly one window (CONTRACTS §11).
#[tauri::command]
pub async fn doc_claim<R: Runtime>(
    window: tauri::Window<R>,
    state: State<'_, HostState>,
    path: String,
) -> Result<ClaimReply, String> {
    let docs = state.docs.lock().expect("docs");
    let owner = docs
        .values()
        .find(|d| d.path.as_ref().is_some_and(|p| p.as_str() == path))
        .map(|d| d.owner_label.clone())
        .unwrap_or_else(|| window.label().to_owned());
    Ok(ClaimReply { owner_label: owner })
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn exec_submit(
    state: State<'_, HostState>,
    session: Option<SessionId>,
    intent: RunIntent,
    inline_mode: Option<InlineResultsMode>,
) -> Result<RunPlan, String> {
    let session = state.session_or_current(session)?;
    match state
        .request(EngineRequest::ExecSubmit {
            session,
            intent,
            inline_mode: inline_mode.unwrap_or(InlineResultsMode::Always),
        })
        .await?
    {
        EngineResponse::Submitted { plan } => Ok(plan),
        EngineResponse::Error(e) => Err(e.to_string()),
        _ => Err("unexpected engine reply to exec_submit".to_owned()),
    }
}

#[tauri::command]
pub async fn exec_cancel(
    state: State<'_, HostState>,
    session: Option<SessionId>,
    run: RunId,
    level: Option<CancelLevel>,
) -> Result<(), String> {
    let session = state.session_or_current(session)?;
    match level {
        // An explicit level is a single rung, exactly as asked.
        Some(level) => {
            // On the Interrupt rung a real child also gets the POSIX signal —
            // SIGINT to the process group, which is what reaches a do-file's
            // `shell` children. The wire request is authoritative either way.
            if level == CancelLevel::Interrupt {
                if let Some(pid) = state.engine.child_pid().await {
                    crate::engine_host::orphan::interrupt(pid);
                }
            }
            let _ = state
                .request(EngineRequest::ExecCancel {
                    session,
                    run,
                    level,
                })
                .await?;
            Ok(())
        }
        // No level: the C21 ladder — interrupt, abort, kill-and-respawn.
        None => {
            let outcome = state.engine.cancel(session, run).await;
            if let crate::engine_host::CancelOutcome::Killed { .. } = outcome {
                state.registry.host_event(
                    session,
                    &HostEvent::EngineHealth {
                        health: stratum_proto::engine::EngineHealth::Crashed {
                            signal: None,
                            last_statement: None,
                            log_tail: "cancel escalated to kill; engine respawned".to_owned(),
                        },
                    },
                );
            }
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Blocks, statuses, ledger, results
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn blocks_get(
    state: State<'_, HostState>,
    session: Option<SessionId>,
    doc: DocumentId,
) -> Result<BlockMap, String> {
    let session = state.session_or_current(session)?;
    match state
        .request(EngineRequest::Blocks { session, doc })
        .await?
    {
        EngineResponse::BlockMap(map) => Ok(map),
        EngineResponse::Error(e) => Err(e.to_string()),
        _ => Err("unexpected engine reply to blocks_get".to_owned()),
    }
}

#[tauri::command]
pub async fn statuses_get(
    state: State<'_, HostState>,
    session: Option<SessionId>,
    doc: DocumentId,
) -> Result<Value, String> {
    let session = state.session_or_current(session)?;
    match state
        .request(EngineRequest::Statuses { session, doc })
        .await?
    {
        EngineResponse::Statuses { statuses, .. } => {
            serde_json::to_value(statuses).map_err(|e| e.to_string())
        }
        EngineResponse::Error(e) => Err(e.to_string()),
        _ => Err("unexpected engine reply to statuses_get".to_owned()),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerReply {
    pub records: Value,
    pub next_seq: u64,
}

#[tauri::command]
pub async fn ledger_get(
    state: State<'_, HostState>,
    session: Option<SessionId>,
    from_seq: u64,
    limit: Option<u32>,
) -> Result<LedgerReply, String> {
    let session = state.session_or_current(session)?;
    match state
        .request(EngineRequest::Ledger {
            session,
            from_seq,
            limit: limit.unwrap_or(256),
        })
        .await?
    {
        EngineResponse::Ledger { records, next_seq } => Ok(LedgerReply {
            records: serde_json::to_value(records).map_err(|e| e.to_string())?,
            next_seq,
        }),
        EngineResponse::Error(e) => Err(e.to_string()),
        _ => Err("unexpected engine reply to ledger_get".to_owned()),
    }
}

#[tauri::command]
pub async fn result_get(
    state: State<'_, HostState>,
    session: Option<SessionId>,
    result: ResultId,
) -> Result<ResultEnvelope, String> {
    let session = state.session_or_current(session)?;
    state
        .registry
        .envelope(session, result)
        .ok_or_else(|| format!("no such result {result}"))
}

// ---------------------------------------------------------------------------
// Variables, frames, data ordering
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VariableRow {
    pub name: String,
    pub storage: String,
    pub format: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_label: Option<String>,
    pub missing: u64,
}

fn variable_row(v: &VariableInfo) -> VariableRow {
    VariableRow {
        name: v.name.clone(),
        storage: format!("{:?}", v.ty).to_lowercase(),
        format: v.format.clone(),
        label: if v.label.is_empty() {
            None
        } else {
            Some(v.label.clone())
        },
        value_label: v.value_label.clone(),
        missing: v.n_missing,
    }
}

#[tauri::command]
pub async fn variables_list(
    state: State<'_, HostState>,
    session: Option<SessionId>,
    frame: Option<String>,
) -> Result<Vec<VariableRow>, String> {
    let session = state.session_or_current(session)?;
    match state
        .request(EngineRequest::Variables {
            session,
            frame: frame.unwrap_or_else(|| "default".to_owned()),
        })
        .await?
    {
        EngineResponse::Variables { vars, .. } => Ok(vars.iter().map(variable_row).collect()),
        EngineResponse::Error(e) => Err(e.to_string()),
        _ => Err("the engine cannot list variables yet".to_owned()),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickStatsReply {
    pub obs: u64,
    pub missing: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mean: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sparkline: Option<Vec<u32>>,
    pub display: Vec<(String, String)>,
    pub deferred: bool,
}

fn quick_stats(q: &QuickSummary) -> QuickStatsReply {
    QuickStatsReply {
        obs: q.n,
        missing: q.n_missing,
        mean: q.mean,
        median: q.median,
        sd: q.sd,
        min: q.min,
        max: q.max,
        sparkline: q.sparkline.clone(),
        display: q.display.clone(),
        deferred: q.deferred,
    }
}

#[tauri::command(rename_all = "snake_case")]
pub async fn variable_stats(
    state: State<'_, HostState>,
    session: Option<SessionId>,
    frame: Option<String>,
    var: String,
) -> Result<QuickStatsReply, String> {
    let session = state.session_or_current(session)?;
    match state
        .request(EngineRequest::VarStats {
            session,
            frame: frame.unwrap_or_else(|| "default".to_owned()),
            var,
        })
        .await?
    {
        EngineResponse::VarStats(q) => Ok(quick_stats(&q)),
        EngineResponse::Error(e) => Err(e.to_string()),
        _ => Err("the engine cannot summarize variables yet".to_owned()),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FramesReply {
    pub frames: Value,
    pub current: String,
}

#[tauri::command]
pub async fn frames_list(
    state: State<'_, HostState>,
    session: Option<SessionId>,
) -> Result<FramesReply, String> {
    let session = state.session_or_current(session)?;
    match state.request(EngineRequest::Frames { session }).await? {
        EngineResponse::Frames { frames, current } => Ok(FramesReply {
            frames: serde_json::to_value(frames).map_err(|e| e.to_string())?,
            current,
        }),
        EngineResponse::Error(e) => Err(e.to_string()),
        _ => Err("the engine cannot list frames yet".to_owned()),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataOrderReply {
    pub order: OrderId,
    pub n_rows: u64,
    pub state: u64,
}

#[tauri::command]
pub async fn data_order_set(
    state: State<'_, HostState>,
    session: Option<SessionId>,
    frame: String,
    spec: OrderSpec,
) -> Result<DataOrderReply, String> {
    let session = state.session_or_current(session)?;
    match state
        .request(EngineRequest::DataOrderSet {
            session,
            frame,
            spec,
        })
        .await?
    {
        EngineResponse::DataOrder {
            order,
            n_rows,
            state: st,
        } => Ok(DataOrderReply {
            order,
            n_rows,
            state: st.0,
        }),
        EngineResponse::Error(e) => Err(e.to_string()),
        _ => Err("the engine cannot order frames yet".to_owned()),
    }
}

#[tauri::command]
pub async fn data_order_drop(
    state: State<'_, HostState>,
    session: Option<SessionId>,
    order: OrderId,
) -> Result<(), String> {
    let session = state.session_or_current(session)?;
    let _ = state
        .request(EngineRequest::DataOrderDrop { session, order })
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Graphs
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn graph_render(
    state: State<'_, HostState>,
    session: Option<SessionId>,
    result: ResultId,
    format: GraphFormat,
    width_pt: Option<f32>,
) -> Result<AssetRef, String> {
    let session = state.session_or_current(session)?;
    // The engine renders into the bulk ring; the webview then fetches through
    // the asset URL this reply names. The command never carries image bytes.
    let _ = state
        .request(EngineRequest::GraphRender {
            session,
            result,
            format,
            width_pt: width_pt.unwrap_or(432.0),
        })
        .await?;
    let ext = match format {
        GraphFormat::Svg => "svg",
        GraphFormat::Png => "png",
        GraphFormat::Pdf => "pdf",
    };
    Ok(AssetRef {
        path: format!("graph/{}/{}.{ext}", session.0, result.0),
        mime: match format {
            GraphFormat::Svg => "image/svg+xml".to_owned(),
            GraphFormat::Png => "image/png".to_owned(),
            GraphFormat::Pdf => "application/pdf".to_owned(),
        },
        bytes: 0,
    })
}

// ---------------------------------------------------------------------------
// The log
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogRangeReply {
    pub runs: Vec<StyledRun>,
    pub line_starts: Vec<u32>,
}

#[tauri::command]
pub async fn log_range(
    state: State<'_, HostState>,
    session: Option<SessionId>,
    from_line: u64,
    to_line: u64,
) -> Result<LogRangeReply, String> {
    let session = state.session_or_current(session)?;
    match state
        .request(EngineRequest::LogRange {
            session,
            from_line,
            to_line,
        })
        .await?
    {
        EngineResponse::LogRange {
            runs, line_starts, ..
        } => Ok(LogRangeReply { runs, line_starts }),
        EngineResponse::Error(e) => Err(e.to_string()),
        _ => Err("the engine cannot serve the log yet".to_owned()),
    }
}

/// Copy ALWAYS goes through Rust so text outside the rendered window is
/// included (CONTRACTS §11). `format` is accepted and currently always plain:
/// the styled formats land with the log pane's export work.
#[tauri::command]
pub async fn log_copy(
    state: State<'_, HostState>,
    session: Option<SessionId>,
    from_line: u64,
    to_line: u64,
    format: Option<String>,
) -> Result<String, String> {
    let _ = format;
    let session = state.session_or_current(session)?;
    match state
        .request(EngineRequest::LogRange {
            session,
            from_line,
            to_line,
        })
        .await?
    {
        EngineResponse::LogRange { runs, .. } => Ok(stratum_proto::styled::to_plain(&runs)),
        EngineResponse::Error(e) => Err(e.to_string()),
        _ => Err("the engine cannot serve the log yet".to_owned()),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogSearchReply {
    pub hits: Value,
    pub total: u64,
}

#[tauri::command]
pub async fn log_search(
    state: State<'_, HostState>,
    session: Option<SessionId>,
    query: String,
    opts: Option<LogSearchOpts>,
) -> Result<LogSearchReply, String> {
    let session = state.session_or_current(session)?;
    match state
        .request(EngineRequest::LogSearch {
            session,
            query,
            opts: opts.unwrap_or(LogSearchOpts {
                regex: false,
                case_sensitive: false,
                max_hits: 200,
            }),
        })
        .await?
    {
        EngineResponse::LogSearch { hits, total } => Ok(LogSearchReply {
            hits: serde_json::to_value(hits).map_err(|e| e.to_string())?,
            total,
        }),
        EngineResponse::Error(e) => Err(e.to_string()),
        _ => Err("the engine cannot search the log yet".to_owned()),
    }
}

// ---------------------------------------------------------------------------
// Repro, def/use, completion
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn repro_report(
    state: State<'_, HostState>,
    session: Option<SessionId>,
    doc: DocumentId,
    verify: Option<bool>,
) -> Result<ReproReport, String> {
    let session = state.session_or_current(session)?;
    match state
        .request(EngineRequest::ReproReport {
            session,
            doc,
            verify: verify.unwrap_or(false),
        })
        .await?
    {
        EngineResponse::ReproReport(report) => Ok(report),
        EngineResponse::Error(e) => Err(e.to_string()),
        _ => Err("the engine cannot build a repro report yet".to_owned()),
    }
}

#[tauri::command]
pub async fn defuse_index(
    state: State<'_, HostState>,
    session: Option<SessionId>,
) -> Result<Value, String> {
    let session = state.session_or_current(session)?;
    match state.request(EngineRequest::DefUse { session }).await? {
        EngineResponse::DefUse(index) => serde_json::to_value(index).map_err(|e| e.to_string()),
        EngineResponse::Error(e) => Err(e.to_string()),
        _ => Err("the engine cannot index def/use yet".to_owned()),
    }
}

#[tauri::command]
pub async fn completion_env(
    state: State<'_, HostState>,
    session: Option<SessionId>,
) -> Result<CompletionEnv, String> {
    let session = state.session_or_current(session)?;
    match state
        .request(EngineRequest::CompletionEnv { session })
        .await?
    {
        EngineResponse::CompletionEnv(env) => Ok(env),
        EngineResponse::Error(e) => Err(e.to_string()),
        _ => Err("the engine cannot build a completion env yet".to_owned()),
    }
}

// ---------------------------------------------------------------------------
// Workspace, layouts, keymaps, sidecars — host-local persistence
// ---------------------------------------------------------------------------

fn read_json(path: &std::path::Path) -> Result<Value, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("parsing {}: {e}", path.display()))
}

fn write_json(path: &std::path::Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("writing {}: {e}", path.display()))
}

#[tauri::command]
pub async fn workspace_load(
    state: State<'_, HostState>,
    project_root: Option<String>,
) -> Result<Value, String> {
    let _ = project_root;
    let path = state.config_dir.join("workspace.json");
    if path.is_file() {
        read_json(&path)
    } else {
        Ok(Value::Object(serde_json::Map::new()))
    }
}

#[tauri::command]
pub async fn workspace_save(
    state: State<'_, HostState>,
    project_root: Option<String>,
    workspace_state: Option<Value>,
    state_value: Option<Value>,
) -> Result<(), String> {
    let _ = project_root;
    // The frontend sends `{ state }`; tauri cannot name a parameter `state`
    // (it is the DI slot), so both spellings are accepted and merged.
    let value = workspace_state
        .or(state_value)
        .unwrap_or(Value::Object(serde_json::Map::new()));
    let path = state.config_dir.join("workspace.json");
    // Merge instead of clobber: settings and window state arrive separately.
    let merged = match (read_json(&path).ok(), value) {
        (Some(Value::Object(mut base)), Value::Object(new)) => {
            for (k, v) in new {
                base.insert(k, v);
            }
            Value::Object(base)
        }
        (_, v) => v,
    };
    write_json(&path, &merged)?;
    state.registry.host_event_all(&HostEvent::SettingsChanged);
    Ok(())
}

#[tauri::command]
pub async fn layout_load(state: State<'_, HostState>, id: String) -> Result<Value, String> {
    let file = state
        .config_dir
        .join("layouts")
        .join(format!("{}.json", sanitize_id(&id)));
    if file.is_file() {
        read_json(&file)
    } else {
        Err(format!("no saved layout `{id}`"))
    }
}

#[tauri::command]
pub async fn layout_save(state: State<'_, HostState>, spec: Value) -> Result<(), String> {
    let id = spec
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| "layout spec has no id".to_owned())?;
    let file = state
        .config_dir
        .join("layouts")
        .join(format!("{}.json", sanitize_id(id)));
    write_json(&file, &spec)?;
    state
        .registry
        .host_event_all(&HostEvent::LayoutChanged { id: id.to_owned() });
    Ok(())
}

#[tauri::command]
pub async fn layout_reset(state: State<'_, HostState>, id: String) -> Result<(), String> {
    let file = state
        .config_dir
        .join("layouts")
        .join(format!("{}.json", sanitize_id(&id)));
    match std::fs::remove_file(&file) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[tauri::command]
pub async fn keymap_load(state: State<'_, HostState>, preset: String) -> Result<Value, String> {
    let file = state
        .config_dir
        .join("keymaps")
        .join(format!("{}.json", sanitize_id(&preset)));
    if file.is_file() {
        read_json(&file)
    } else {
        // No overlay is a complete answer: the preset alone is a full keymap.
        Ok(Value::Array(Vec::new()))
    }
}

#[tauri::command]
pub async fn keymap_save(
    state: State<'_, HostState>,
    preset: Option<String>,
    bindings: Value,
) -> Result<(), String> {
    let file = state.config_dir.join("keymaps").join(format!(
        "{}.json",
        sanitize_id(preset.as_deref().unwrap_or("custom"))
    ));
    write_json(&file, &bindings)
}

#[tauri::command]
pub async fn sidecar_get(state: State<'_, HostState>, doc: Value) -> Result<Value, String> {
    let key = sidecar_key(&doc);
    let file = state
        .config_dir
        .join("sidecars")
        .join(format!("{key}.json"));
    if file.is_file() {
        read_json(&file)
    } else {
        Ok(Value::Null)
    }
}

#[tauri::command]
pub async fn sidecar_patch(
    state: State<'_, HostState>,
    doc: Value,
    patch: Value,
) -> Result<(), String> {
    let key = sidecar_key(&doc);
    let file = state
        .config_dir
        .join("sidecars")
        .join(format!("{key}.json"));
    let merged = match (read_json(&file).ok(), patch) {
        (Some(Value::Object(mut base)), Value::Object(new)) => {
            for (k, v) in new {
                base.insert(k, v);
            }
            Value::Object(base)
        }
        (_, v) => v,
    };
    write_json(&file, &merged)
}

fn sidecar_key(doc: &Value) -> String {
    match doc {
        Value::String(s) => sanitize_id(s),
        other => sanitize_id(&other.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Menus, windows, platform
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn menu_accelerator(action: String, preset: Option<String>) -> Result<Value, String> {
    let preset = match preset.as_deref() {
        Some("stata") | Some("stata_like") | Some("stataLike") => KeymapPreset::StataLike,
        Some("vscode") | Some("vs_code_like") | Some("vsCodeLike") => KeymapPreset::VsCodeLike,
        Some("custom") => KeymapPreset::Custom,
        _ => KeymapPreset::Modern,
    };
    let platform = stratum_platform_host::host();
    let accel = platform
        .menus()
        .accelerator(&ActionId::from(action.as_str()), preset);
    Ok(match accel {
        Some(a) => Value::String(a.display(platform.id())),
        None => Value::Null,
    })
}

#[derive(Deserialize)]
pub struct PaneBounds {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowOpened {
    pub label: String,
}

#[tauri::command]
pub async fn window_open_pane<R: Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, HostState>,
    session: Option<SessionId>,
    role: String,
    pane_id: Option<String>,
    label: Option<String>,
    bounds: Option<PaneBounds>,
) -> Result<WindowOpened, String> {
    let session = state.session_or_current(session).ok();
    let label = label.unwrap_or_else(|| {
        format!(
            "pane-{}-{}",
            sanitize_id(pane_id.as_deref().unwrap_or(&role)),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_millis())
                .unwrap_or(0)
        )
    });
    let mut url = format!("pane.html?role={role}&label={label}");
    if let Some(pane) = &pane_id {
        url.push_str(&format!("&paneId={pane}"));
    }
    let mut builder =
        tauri::WebviewWindowBuilder::new(&app, &label, tauri::WebviewUrl::App(url.into()))
            .title("Stratum")
            .visible(!crate::headless());
    if let Some(b) = bounds {
        builder = builder.position(b.x, b.y).inner_size(b.w, b.h);
    }
    builder.build().map_err(|e| e.to_string())?;
    if let Some(session) = session {
        state.registry.bind_label(session, &label);
    }
    Ok(WindowOpened { label })
}

#[tauri::command]
pub async fn window_close<R: Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, HostState>,
    label: String,
) -> Result<(), String> {
    state.registry.unsubscribe_label(&label);
    if let Some(window) = app.get_webview_window(&label) {
        window.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn platform_open_external(url: String) -> Result<(), String> {
    let external = stratum_platform::dialogs::ExternalUrl::parse(&url)
        .map_err(|e| format!("refusing to open {url}: {e}"))?;
    stratum_platform_host::host()
        .dialogs()
        .open_external(&external)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn platform_reveal(path: String) -> Result<(), String> {
    stratum_platform_host::host()
        .dialogs()
        .reveal(camino::Utf8Path::new(&path))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn credentials_backend() -> Result<Value, String> {
    let backend = stratum_platform_host::host().credentials().backend();
    serde_json::to_value(backend).map_err(|e| e.to_string())
}

/// CONTRACTS §11 `bench_report { marks } -> void`. The webview's performance
/// marks land beside the host's own ADR-017 counters on stderr, so a bench run
/// reads as one report: transport frames/seq-gaps, the two-copy bulk ledger,
/// the C21 ladder budgets, and the supervised pid.
#[tauri::command]
pub async fn bench_report(state: State<'_, HostState>, marks: Option<Value>) -> Result<(), String> {
    let tx = state.engine.transport().await;
    let stats = tx.stats();
    let copies = state.engine.ledger.total_copies();
    let pid = state.engine.child_pid().await;
    eprintln!(
        "bench_report: marks={} frames_in={} frames_out={} events={} last_seq={} seq_gaps={} \
         dropped_no_subscriber={} bulk_copies={} fanout_deliveries={} fanout_encodes={} \
         fanout_snapshots={} fanout_replays={} engine_pid={pid:?} ack_budget={:?} \
         interrupt_to_abort={:?} abort_to_kill={:?}",
        marks.unwrap_or(Value::Null),
        stats.frames_in.load(Ordering::Relaxed),
        stats.frames_out.load(Ordering::Relaxed),
        stats.events.load(Ordering::Relaxed),
        stats.last_seq.load(Ordering::Relaxed),
        stats.seq_gaps.load(Ordering::Relaxed),
        stats.dropped_no_subscriber.load(Ordering::Relaxed),
        copies,
        state.registry.counters.deliveries.load(Ordering::Relaxed),
        state.registry.counters.encodes.load(Ordering::Relaxed),
        state.registry.counters.snapshots.load(Ordering::Relaxed),
        state.registry.counters.replays.load(Ordering::Relaxed),
        crate::engine_host::ACK_BUDGET,
        crate::engine_host::INTERRUPT_TO_ABORT,
        crate::engine_host::ABORT_TO_KILL,
    );
    if cfg!(windows) {
        eprintln!(
            "bench_report: orphan-prevention gap on Windows: {}",
            crate::engine_host::orphan::WINDOWS_JOB_OBJECT_NOTE
        );
    }
    Ok(())
}

/// Where the harness's loopback address waits until the webview is ready.
///
/// The e2e control channel forwards every harness request to the webview and
/// waits 20 s for the reply — so connecting back the moment the process starts
/// (which is when `e2e_cmds::tauri_surface::attach` would do it, reading the
/// env var itself) loses the race on every boot: the harness's `hello` arrives
/// while the webview is still fetching its bundle, the emitted request has no
/// listener, and the handshake times out. `main()` therefore stashes the
/// address here, removes the env var so `attach` stays quiet, and `app_ready`
/// — the webview saying "my listener is installed" — is what dials out.
#[cfg(feature = "e2e")]
pub static E2E_CALLBACK: std::sync::OnceLock<(u16, String)> = std::sync::OnceLock::new();

/// The `--selftest` handshake, and where the webview receives the asset token.
#[tauri::command]
pub async fn app_ready<R: Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, HostState>,
) -> Result<Value, String> {
    let first = !state.ready.swap(true, Ordering::SeqCst);
    if let Ok(tx) = state.ready_tx.lock() {
        if let Some(tx) = tx.as_ref() {
            let _ = tx.send(());
        }
    }

    // The harness is waiting for this process to dial back (ADR-011); now that
    // the webview can answer, do. Fenced with the feature — in a shipped build
    // neither the constant nor the thread exists.
    #[cfg(feature = "e2e")]
    if first {
        if let Some((port, host)) = E2E_CALLBACK.get().cloned() {
            let control =
                std::sync::Arc::clone(&*app.state::<std::sync::Arc<crate::e2e_cmds::Control>>());
            std::thread::spawn(move || {
                if let Err(e) = crate::e2e_cmds::serve(&control, port, &host) {
                    eprintln!("e2e control channel: {e}");
                }
            });
        }
    }
    #[cfg(not(feature = "e2e"))]
    let _ = (&app, first);

    Ok(serde_json::json!({
        "assetToken": state.token_hex,
        "e2e": std::env::var_os("STRATUM_E2E").is_some(),
    }))
}

// ---------------------------------------------------------------------------
// The stratum-asset:// handler (CONTRACTS §10; the glue over `asset.rs`)
// ---------------------------------------------------------------------------

fn http_response(status: u16, mime: &str, body: Vec<u8>) -> tauri::http::Response<Vec<u8>> {
    let mut builder = tauri::http::Response::builder().status(status);
    if !mime.is_empty() {
        builder = builder.header("content-type", mime);
    }
    builder
        .header("access-control-allow-origin", "*")
        .body(body)
        .unwrap_or_else(|_| tauri::http::Response::new(Vec::new()))
}

fn deny(status: u16) -> tauri::http::Response<Vec<u8>> {
    http_response(status, "", Vec::new())
}

/// Resolve one asset request. Sync policy first (parse, token, label binding),
/// then the async engine round trips on the runtime, then respond.
pub fn handle_asset<R: Runtime>(
    ctx: tauri::UriSchemeContext<'_, R>,
    request: tauri::http::Request<Vec<u8>>,
    responder: tauri::UriSchemeResponder,
) {
    let app = ctx.app_handle().clone();
    let label = ctx.webview_label().to_owned();
    let url = request.uri().to_string();
    let token = request
        .headers()
        .get(asset::TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned);

    tauri::async_runtime::spawn(async move {
        let state = app.state::<HostState>();
        let parsed = match asset::parse(&url) {
            Ok(p) => p,
            Err(_) => {
                responder.respond(deny(400));
                return;
            }
        };

        // `app/{…}` is bundled static content: no session, no token — the same
        // bytes the main window was booted from.
        if parsed.kind == AssetKind::App {
            let path = format!("/{}", parsed.rest.join("/"));
            let mime = asset::mime_for(&parsed).to_owned();
            match app.asset_resolver().get(path) {
                Some(found) => responder.respond(http_response(200, &mime, found.bytes)),
                None => responder.respond(deny(404)),
            }
            return;
        }

        // §10.2: every data request carries the token…
        if !asset::token_ok(token.as_deref(), &state.token_hex) {
            responder.respond(deny(401));
            return;
        }
        // …and comes from a webview bound to the session in its URL.
        let Some(session_num) = asset::numeric_id(&parsed.session) else {
            responder.respond(deny(400));
            return;
        };
        let session = SessionId(session_num as u32);
        if !state.registry.label_bound(session, &label) {
            responder.respond(deny(403));
            return;
        }

        let mime = asset::mime_for(&parsed).to_owned();
        match parsed.kind {
            AssetKind::Result => {
                let (Some(id_seg), Some(what)) = (parsed.rest.first(), parsed.rest.get(1)) else {
                    responder.respond(deny(404));
                    return;
                };
                let Some(result) = asset::numeric_id(id_seg) else {
                    responder.respond(deny(404));
                    return;
                };
                match what.as_str() {
                    "raw" => match state.registry.raw_text(session, ResultId(result)) {
                        Some(text) => {
                            responder.respond(http_response(200, &mime, text.into_bytes()))
                        }
                        None => responder.respond(deny(404)),
                    },
                    // SDP1 table bytes travel the bulk ring; W17 serves them the
                    // moment an engine writes them. No engine does yet, so this
                    // is an honest 404 rather than invented bytes.
                    _ => responder.respond(deny(404)),
                }
            }
            AssetKind::Graph => {
                let Some(name) = parsed.rest.first() else {
                    responder.respond(deny(404));
                    return;
                };
                let (id_part, format) = match name.rsplit_once('.') {
                    Some((id, "svg")) => (id, GraphFormat::Svg),
                    Some((id, "png")) => (id, GraphFormat::Png),
                    _ => {
                        responder.respond(deny(404));
                        return;
                    }
                };
                let Some(result) = asset::numeric_id(id_part) else {
                    responder.respond(deny(404));
                    return;
                };
                match engine_bulk(
                    &state,
                    session,
                    EngineRequest::GraphRender {
                        session,
                        result: ResultId(result),
                        format,
                        width_pt: 432.0,
                    },
                )
                .await
                {
                    Ok(bytes) => responder.respond(http_response(200, &mime, bytes)),
                    Err(_) => responder.respond(deny(404)),
                }
            }
            AssetKind::Frame => {
                let (Some(frame), Some(route)) = (parsed.rest.first(), parsed.rest.get(1)) else {
                    responder.respond(deny(404));
                    return;
                };
                if route != "page" {
                    responder.respond(deny(404));
                    return;
                }
                let Ok(q) = asset::parse_page_query(parsed.query.as_deref().unwrap_or("")) else {
                    responder.respond(deny(400));
                    return;
                };
                let request = EngineRequest::DataPage {
                    session,
                    request: stratum_proto::data::PageRequest {
                        frame: frame.clone(),
                        state: stratum_proto::ids::DatasetStateId(q.state),
                        row0: q.row0,
                        nrows: q.nrows,
                        cols: q.cols.into_iter().map(stratum_proto::ids::VarIdx).collect(),
                        order: q.order.map(OrderId),
                        render: if q.render == "edit" {
                            stratum_proto::data::RenderMode::Edit
                        } else {
                            stratum_proto::data::RenderMode::Display
                        },
                        seq: q.seq,
                    },
                };
                match engine_bulk(&state, session, request).await {
                    Ok(bytes) => responder.respond(http_response(200, &mime, bytes)),
                    Err(_) => responder.respond(deny(404)),
                }
            }
            AssetKind::App => unreachable!("handled above"),
        }
    });
}

/// Ask the engine for a bulk payload and resolve it out of the mmap ring —
/// the two-copy path of CONTRACTS §10 (engine → mmap, mmap → response body).
async fn engine_bulk(
    state: &HostState,
    session: SessionId,
    req: EngineRequest,
) -> Result<Vec<u8>, String> {
    match state.request(req).await? {
        EngineResponse::Bulk { bulk } => {
            let segments = &state.engine.bulk;
            let slice = match segments.resolve(&bulk) {
                Ok(s) => Ok(s),
                Err(_) => {
                    // First touch of this segment: attach it by the shared
                    // naming convention, then resolve again.
                    let path = crate::transport::BulkSegments::segment_path(
                        &std::env::temp_dir(),
                        session.0,
                        bulk.segment,
                    );
                    segments
                        .attach(bulk.segment, &path, bulk.epoch)
                        .map_err(|e| e.to_string())?;
                    if let Ok(mut attached) = state.attached_segments.lock() {
                        if !attached.contains(&bulk.segment) {
                            attached.push(bulk.segment);
                        }
                    }
                    segments.resolve(&bulk).map_err(|e| e.to_string())
                }
            }?;
            Ok(slice.into_response_body())
        }
        EngineResponse::Error(e) => Err(e.to_string()),
        _ => Err("the engine did not answer with bulk".to_owned()),
    }
}

// ---------------------------------------------------------------------------
// The engine event pump and crash supervision
// ---------------------------------------------------------------------------

/// Forward every engine event to every subscribed window, in order, forever.
/// On stream loss: report `Crashed` (the banner — result cards are host state
/// and stay), respawn through W07's supervisor, report `Ready` again.
pub fn spawn_event_pump(engine: Arc<EngineHost>, registry: Arc<SessionRegistry>) {
    tauri::async_runtime::spawn(async move {
        loop {
            let mut rx = engine.transport().await.subscribe();
            loop {
                match rx.recv().await {
                    Ok(ev) => {
                        if let Some(session) = event_session(&ev, &registry) {
                            registry.apply_engine_event(session, &ev);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            // The transport died. If the host is shutting down, stop; otherwise
            // this is C21's crash path: banner, respawn, banner again.
            if matches!(
                *engine.health().borrow(),
                stratum_proto::engine::EngineHealth::Stopped
            ) {
                break;
            }
            let crashed = stratum_proto::engine::EngineHealth::Crashed {
                signal: None,
                last_statement: None,
                log_tail: "the engine process ended unexpectedly".to_owned(),
            };
            registry.set_health(&crashed);
            registry.host_event_all(&HostEvent::EngineHealth { health: crashed });
            if engine.restart().await.is_err() {
                // The supervisor could not bring an engine back at all; from
                // here every command answers with this error.
                eprintln!(
                    "stratum-desktop: {}",
                    crate::engine_host::HostError::NotRunning
                );
                break;
            }
            let ready = stratum_proto::engine::EngineHealth::Ready;
            registry.set_health(&ready);
            registry.host_event_all(&HostEvent::EngineHealth { health: ready });
        }
    });
}

/// Which session an event belongs to. The wire does not stamp one on every
/// event; a single-session host (the product today) routes to its only hub.
fn event_session(
    ev: &stratum_proto::engine::EngineEvent,
    registry: &SessionRegistry,
) -> Option<SessionId> {
    if let stratum_proto::engine::EngineEvent::RunStarted { session, .. } = ev {
        return Some(*session);
    }
    registry.only_session()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_apply_back_to_front() {
        let mut text = "abcdef".to_owned();
        apply_edits(
            &mut text,
            &[
                Edit {
                    span: stratum_proto::ids::Span { start: 0, end: 1 },
                    text: "X".to_owned(),
                },
                Edit {
                    span: stratum_proto::ids::Span { start: 5, end: 6 },
                    text: "Y".to_owned(),
                },
            ],
        )
        .expect("both edits apply");
        assert_eq!(text, "XbcdeY");
    }

    #[test]
    fn an_edit_outside_the_document_is_refused() {
        let mut text = "ab".to_owned();
        assert!(apply_edits(
            &mut text,
            &[Edit {
                span: stratum_proto::ids::Span { start: 1, end: 9 },
                text: String::new(),
            }],
        )
        .is_err());
        assert_eq!(text, "ab", "a refused edit changes nothing");
    }

    #[test]
    fn the_token_is_stable_hex_of_32_bytes() {
        let a = token_hex();
        let b = token_hex();
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn eol_and_bom_are_detected_and_recorded() {
        let (eol, bom, text) = detect_eol_bom(b"\xEF\xBB\xBFdi 1\r\ndi 2\r\n");
        assert_eq!(eol, "crlf");
        assert!(bom);
        assert_eq!(text, "di 1\ndi 2\n");

        let (eol, bom, text) = detect_eol_bom(b"di 1\ndi 2\n");
        assert_eq!(eol, "lf");
        assert!(!bom);
        assert_eq!(text, "di 1\ndi 2\n");
    }
}
