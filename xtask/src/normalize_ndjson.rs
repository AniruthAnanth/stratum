//! CONTRACTS §7.2 — the `--deterministic` normalizer (audit finding A8), and
//! the artifact ARCHITECTURE §8.9 compares across macOS, Windows and Linux.
//!
//! `stratum run <f> --json | cargo xtask normalize-ndjson` is specified to be
//! equivalent to `stratum run <f> --json --deterministic`, so the substitution
//! table below is the *only* place the rule is written down and both
//! implementations are expected to agree with it byte for byte.
//!
//! The substitutions, verbatim from §7.2:
//!
//! | field | becomes |
//! |---|---|
//! | every `*_at_ms` | `0` |
//! | every `duration_us` | `0` |
//! | `RunStarted.stratum_version` | `"<version>"` |
//! | `RunStarted.cwd` | `"<cwd>"` |
//! | `RunStarted.source`, `Diagnostic.file`, `Finding`/`SiteRef` paths, `DepKey::File`, `StaleReason::FileChanged` | relative to the entry file's parent, `/` separators, or `"<abs>"` if it escapes |
//! | `AssetRef.path`, `RawRef.asset` | session id replaced by `S0` |
//! | anything else | verbatim |
//!
//! What §7.2 deliberately does NOT normalize, and neither does this: `seq` and
//! every id (`ExecutionId`, `ResultId`, `BlockId`, `DatasetStateId`, `StateId`).
//! They are already deterministic, and normalizing them would hide id-allocation
//! drift — the exact class of bug this comparison exists to catch.
//!
//! `SessionId` is in neither list, so it is left verbatim: only the session
//! segment *inside an asset path* is rewritten. That is only consistent if
//! `stratum run` allocates a deterministic session id, which is what invariant 9
//! requires of it. If a run ever emits a random `RunStarted.session`, §7.2 is the
//! thing to change, not this file.

use std::io::{BufRead, BufWriter, Read, Write};

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use clap::Args;
use serde_json::{Map, Value};

use crate::Ctx;

#[derive(Args)]
pub struct Cmd {
    /// NDJSON to read. `-` or omitted reads stdin.
    #[arg(value_name = "FILE")]
    pub input: Option<Utf8PathBuf>,

    /// Write here instead of stdout.
    #[arg(short, long, value_name = "FILE")]
    pub out: Option<Utf8PathBuf>,

    /// Directory paths are made relative to. Defaults to the parent of
    /// `RunStarted.source`, which is what §7.2 specifies ("the entry file's
    /// parent"); pass it explicitly for a stream that has no `RunStarted`.
    #[arg(long, value_name = "DIR")]
    pub base: Option<Utf8PathBuf>,
}

pub fn run(_ctx: &Ctx, cmd: &Cmd) -> Result<()> {
    let mut input: Box<dyn BufRead> = match cmd.input.as_deref().map(Utf8Path::as_str) {
        None | Some("-") => Box::new(std::io::BufReader::new(std::io::stdin())),
        Some(p) => Box::new(std::io::BufReader::new(
            std::fs::File::open(p).with_context(|| format!("opening {p}"))?,
        )),
    };
    let mut text = String::new();
    input.read_to_string(&mut text).context("reading NDJSON")?;

    let out = normalize_stream(&text, cmd.base.as_deref().map(Utf8Path::as_str))?;

    match &cmd.out {
        Some(p) => std::fs::write(p, out).with_context(|| format!("writing {p}")),
        None => {
            let stdout = std::io::stdout();
            let mut w = BufWriter::new(stdout.lock());
            w.write_all(out.as_bytes())?;
            w.flush().map_err(Into::into)
        }
    }
}

/// Normalize a whole stream. Lines are LF-terminated, UTF-8, no BOM (§7.1);
/// a line that is not JSON is passed through unchanged, because §7.1 requires a
/// reader that does not understand a line to skip it and continue, and a
/// normalizer that panics on a future line shape would break that promise.
pub fn normalize_stream(text: &str, base: Option<&str>) -> Result<String> {
    let mut base = base.map(str::to_owned);
    let mut out = String::with_capacity(text.len());

    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            out.push_str(line);
            continue;
        }
        let Ok(mut value) = serde_json::from_str::<Value>(trimmed) else {
            out.push_str(line);
            continue;
        };
        // The first RunStarted fixes the base directory for every path in the
        // rest of the stream, and is itself normalized against that base — so
        // `source` becomes the bare file name.
        if base.is_none() {
            if let Some(src) = find_run_started_source(&value) {
                base = parent_of(&src);
            }
        }
        normalize_value(&mut value, base.as_deref());
        out.push_str(&serde_json::to_string(&value).context("re-encoding a normalized line")?);
        out.push('\n');
    }
    Ok(out)
}

fn find_run_started_source(value: &Value) -> Option<String> {
    let body = value.get("body").unwrap_or(value);
    if body.get("event")? != "run_started" {
        return None;
    }
    body.get("source")?.as_str().map(str::to_owned)
}

fn normalize_value(value: &mut Value, base: Option<&str>) {
    match value {
        Value::Array(items) => {
            for item in items {
                normalize_value(item, base);
            }
        }
        Value::Object(map) => normalize_object(map, base),
        _ => {}
    }
}

fn normalize_object(map: &mut Map<String, Value>, base: Option<&str>) {
    // `AssetRef { path, mime, bytes }` is the one object whose `path` is not a
    // filesystem path (CONTRACTS §9: "Never a filesystem path"), so it is
    // recognised structurally and handled before the by-name rules below.
    if is_asset_ref(map) {
        if let Some(Value::String(p)) = map.get_mut("path") {
            *p = asset_session_to_s0(p);
        }
        return;
    }

    for (key, val) in map.iter_mut() {
        match (key.as_str(), &mut *val) {
            (k, Value::Number(n)) if k.ends_with("_at_ms") && !n.is_f64() => {
                *val = Value::from(0u64);
            }
            (k, Value::Number(_)) if k.ends_with("duration_us") => {
                *val = Value::from(0u64);
            }
            ("stratum_version", Value::String(_)) => {
                *val = Value::from("<version>");
            }
            ("cwd", Value::String(_)) => {
                *val = Value::from("<cwd>");
            }
            // Every path-valued field named by §7.2 uses one of these four
            // names in CONTRACTS: `source` (RunStarted), `file` (Diagnostic,
            // Related), `path` (DepKey::File, StaleReason::FileChanged,
            // BrokenReason::MissingFile, Provenance) and `files`
            // (DefUseIndex, which is what `Finding`/`SiteRef` index into —
            // SiteRef::file is a u32 index, and the `Value::String` guard is
            // what keeps this rule off it).
            ("source" | "file" | "path", Value::String(s)) => {
                *val = Value::from(relativize(s, base));
            }
            ("files", Value::Array(items)) => {
                for item in items.iter_mut() {
                    if let Value::String(s) = item {
                        *item = Value::from(relativize(s, base));
                    }
                }
            }
            _ => normalize_value(val, base),
        }
    }
}

/// `AssetRef { path, mime, bytes }` — recognised by shape rather than by a
/// field count, so adding a field to it in proto does not silently turn its
/// `path` into a filesystem path here.
fn is_asset_ref(map: &Map<String, Value>) -> bool {
    map.get("path").is_some_and(Value::is_string)
        && map.get("mime").is_some_and(Value::is_string)
        && map.get("bytes").is_some_and(Value::is_number)
}

/// `{kind}/{session}/…` — CONTRACTS §10.1 pins the shape, so replacing segment
/// 1 is exact rather than a guess. A path that does not have that shape is left
/// alone: inventing a substitution would corrupt an asset URL we do not own.
fn asset_session_to_s0(path: &str) -> String {
    let mut parts: Vec<&str> = path.split('/').collect();
    let kind_at = usize::from(parts.first() == Some(&""));
    if parts.len() > kind_at + 1 && matches!(parts[kind_at], "result" | "graph" | "frame") {
        parts[kind_at + 1] = "S0";
    }
    parts.join("/")
}

/// Make `p` relative to `base`, `/`-separated, or `"<abs>"` if it escapes.
///
/// Purely lexical: it never touches the filesystem. The whole point is that
/// macOS, Windows and Linux produce the same answer for a path that may not
/// exist on the machine doing the comparison.
pub fn relativize(p: &str, base: Option<&str>) -> String {
    let path = to_slashes(p);
    if !is_absolute(&path) {
        return path;
    }
    let Some(base) = base else {
        return "<abs>".to_owned();
    };
    let base = trim_trailing_slash(&to_slashes(base));

    let (hay, needle) = if cfg!(windows) {
        (path.to_lowercase(), base.to_lowercase())
    } else {
        (path.clone(), base.clone())
    };
    if hay == needle {
        return ".".to_owned();
    }
    match hay.strip_prefix(&needle) {
        Some(rest) if rest.starts_with('/') => path[base.len() + 1..].to_owned(),
        _ => "<abs>".to_owned(),
    }
}

fn parent_of(p: &str) -> Option<String> {
    let p = to_slashes(p);
    let idx = p.rfind('/')?;
    Some(if idx == 0 {
        "/".to_owned()
    } else {
        p[..idx].to_owned()
    })
}

fn to_slashes(p: &str) -> String {
    p.replace('\\', "/")
}

fn trim_trailing_slash(p: &str) -> String {
    if p.len() > 1 && p.ends_with('/') {
        p[..p.len() - 1].to_owned()
    } else {
        p.to_owned()
    }
}

/// True for `/x` and for a Windows path such as `C:/x` or `//server/share`.
/// Recognising Windows shapes on every host is deliberate: CI normalizes the
/// Windows runner's output on Linux before diffing it.
fn is_absolute(p: &str) -> bool {
    let b = p.as_bytes();
    if b.first() == Some(&b'/') {
        return true;
    }
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/'
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN_STARTED: &str = r#"{"v":1,"t":"event","body":{"event":"run_started","seq":0,"schema":1,"run":"R1","session":"S7f3","stratum_version":"0.1.0+ci","source":"/home/u/proj/analysis.do","clean_state":true,"cwd":"/home/u/proj","started_at_ms":1755820000123,"seed":null,"plan_len":4}}"#;

    fn one(line: &str, base: Option<&str>) -> Value {
        let out = normalize_stream(line, base).unwrap();
        serde_json::from_str(out.trim_end()).unwrap()
    }

    #[test]
    fn run_started_is_normalized_and_sets_the_base() {
        let v = one(&format!("{RUN_STARTED}\n"), None);
        let b = &v["body"];
        assert_eq!(b["started_at_ms"], 0);
        assert_eq!(b["stratum_version"], "<version>");
        assert_eq!(b["cwd"], "<cwd>");
        assert_eq!(
            b["source"], "analysis.do",
            "source is relative to its own parent"
        );
        // Not normalized, on purpose.
        assert_eq!(b["seq"], 0);
        assert_eq!(b["run"], "R1");
        assert_eq!(b["session"], "S7f3");
        assert_eq!(b["plan_len"], 4);
    }

    #[test]
    fn durations_and_timestamps_go_to_zero_everywhere() {
        let src = format!(
            "{RUN_STARTED}\n{}\n{}\n",
            r#"{"v":1,"t":"event","body":{"event":"block_finished","seq":9,"run":"R1","exec":"E3","block":"B2","result":"Q1","status":{"state":"ok"},"rc":0,"duration_us":41233,"dataset_state_out":"D4"}}"#,
            r#"{"v":1,"t":"resp","corr":7,"body":{"generated_at_ms":1755820001000,"verified_duration_us":12,"nested":[{"finished_at_ms":9}]}}"#
        );
        let out = normalize_stream(&src, None).unwrap();
        let lines: Vec<Value> = out
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines[1]["body"]["duration_us"], 0);
        assert_eq!(lines[1]["body"]["rc"], 0);
        assert_eq!(lines[1]["body"]["seq"], 9);
        assert_eq!(lines[2]["body"]["generated_at_ms"], 0);
        assert_eq!(lines[2]["body"]["verified_duration_us"], 0);
        assert_eq!(lines[2]["body"]["nested"][0]["finished_at_ms"], 0);
        assert_eq!(lines[2]["corr"], 7);
    }

    #[test]
    fn diagnostic_and_dep_paths_are_relativized() {
        let src = format!(
            "{RUN_STARTED}\n{}\n",
            r#"{"v":1,"t":"event","body":{"event":"diagnostic","seq":3,"exec":"E1","diagnostic":{"severity":"error","code":"STATA0111","message":"variable not found","file":"/home/u/proj/sub/clean.do","related":[{"span":[0,4],"file":"/elsewhere/other.do","message":"first used here"}],"notes":[]}}}"#
        );
        let v: Value = serde_json::from_str(
            normalize_stream(&src, None)
                .unwrap()
                .lines()
                .nth(1)
                .unwrap(),
        )
        .unwrap();
        let d = &v["body"]["diagnostic"];
        assert_eq!(d["file"], "sub/clean.do");
        assert_eq!(
            d["related"][0]["file"], "<abs>",
            "escaping the entry dir is <abs>"
        );
        assert_eq!(d["code"], "STATA0111");
    }

    #[test]
    fn stale_reason_and_depkey_paths_are_relativized() {
        let src = format!(
            "{RUN_STARTED}\n{}\n",
            r#"{"v":1,"t":"event","body":{"event":"status_changed","seq":5,"doc":"D1","changed":[["B1",{"state":"stale","reason":{"why":"file_changed","path":"/home/u/proj/data/raw.csv"},"since":"E2"}],["B2",{"state":"stale","reason":{"why":"input_changed","key":{"ns":"file","path":"/home/u/proj/w.dta"},"at":null}}]]}}"#
        );
        let v: Value = serde_json::from_str(
            normalize_stream(&src, None)
                .unwrap()
                .lines()
                .nth(1)
                .unwrap(),
        )
        .unwrap();
        let ch = &v["body"]["changed"];
        assert_eq!(ch[0][1]["reason"]["path"], "data/raw.csv");
        assert_eq!(ch[1][1]["reason"]["key"]["path"], "w.dta");
        assert_eq!(ch[0][1]["since"], "E2", "ids stay verbatim");
    }

    #[test]
    fn asset_refs_lose_the_session_but_keep_the_shape() {
        let src = format!(
            "{RUN_STARTED}\n{}\n",
            r#"{"v":1,"t":"event","body":{"event":"result","seq":6,"exec":"E1","envelope":{"raw":{"bytes":9001,"lines":42,"head":"…","truncated":true,"asset":{"path":"result/7f3a9c/Q12/raw","mime":"text/plain","bytes":9001}},"graph":{"path":"graph/7f3a9c/Q13.svg","mime":"image/svg+xml","bytes":1024}}}}"#
        );
        let v: Value = serde_json::from_str(
            normalize_stream(&src, None)
                .unwrap()
                .lines()
                .nth(1)
                .unwrap(),
        )
        .unwrap();
        let e = &v["body"]["envelope"];
        assert_eq!(e["raw"]["asset"]["path"], "result/S0/Q12/raw");
        assert_eq!(e["raw"]["asset"]["mime"], "text/plain");
        assert_eq!(
            e["raw"]["bytes"], 9001,
            "RawRef.bytes is not an AssetRef field"
        );
        assert_eq!(e["graph"]["path"], "graph/S0/Q13.svg");
    }

    #[test]
    fn defuse_file_table_is_relativized() {
        let src = format!(
            "{RUN_STARTED}\n{}\n",
            r#"{"v":1,"t":"resp","corr":2,"body":{"generation":3,"files":["/home/u/proj/analysis.do","/home/u/proj/lib/util.do","/opt/ado/x.ado"],"defs":[["income",[{"file":0,"line":12,"col":1,"span":[100,120],"block":"B1","statement":"gen income = .","kind":"def","confidence":"exact"}]]],"uses":[],"unresolved":[]}}"#
        );
        let v: Value = serde_json::from_str(
            normalize_stream(&src, None)
                .unwrap()
                .lines()
                .nth(1)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(v["body"]["files"][0], "analysis.do");
        assert_eq!(v["body"]["files"][1], "lib/util.do");
        assert_eq!(v["body"]["files"][2], "<abs>");
        assert_eq!(
            v["body"]["defs"][0][1][0]["file"], 0,
            "SiteRef::file is an index into `files`, not a path"
        );
    }

    /// The invariant the whole subcommand exists for: two runs of the same
    /// analysis, on different machines, differ only in the fields §7.2 names.
    #[test]
    fn two_platforms_agree_after_normalization() {
        let unix = format!(
            "{RUN_STARTED}\n{}\n",
            r#"{"v":1,"t":"event","body":{"event":"block_finished","seq":9,"run":"R1","exec":"E3","block":"B2","result":null,"status":{"state":"ok"},"rc":0,"duration_us":41233,"dataset_state_out":"D4"}}"#
        );
        let windows = format!(
            "{}\n{}\n",
            r#"{"v":1,"t":"event","body":{"event":"run_started","seq":0,"schema":1,"run":"R1","session":"S7f3","stratum_version":"0.1.0+win","source":"C:\\Users\\u\\proj\\analysis.do","clean_state":true,"cwd":"C:\\Users\\u\\proj","started_at_ms":1755999999999,"seed":null,"plan_len":4}}"#,
            r#"{"v":1,"t":"event","body":{"event":"block_finished","seq":9,"run":"R1","exec":"E3","block":"B2","result":null,"status":{"state":"ok"},"rc":0,"duration_us":98,"dataset_state_out":"D4"}}"#
        );
        assert_eq!(
            normalize_stream(&unix, None).unwrap(),
            normalize_stream(&windows, None).unwrap()
        );
    }

    #[test]
    fn unrecognised_lines_pass_through_and_the_stream_stays_ndjson() {
        let src = "not json at all\n\n{\"v\":1,\"t\":\"event\",\"body\":{\"event\":\"future\",\"seq\":1}}\n";
        let out = normalize_stream(src, None).unwrap();
        assert_eq!(
            out,
            "not json at all\n\n{\"v\":1,\"t\":\"event\",\"body\":{\"event\":\"future\",\"seq\":1}}\n"
        );
        assert!(out.ends_with('\n') && !out.contains("\r\n"));
    }

    #[test]
    fn normalization_is_idempotent() {
        let src = format!("{RUN_STARTED}\n");
        let once = normalize_stream(&src, None).unwrap();
        assert_eq!(normalize_stream(&once, None).unwrap(), once);
    }

    #[test]
    fn relativize_edge_cases() {
        assert_eq!(relativize("/a/b/c.do", Some("/a/b")), "c.do");
        assert_eq!(relativize("/a/b/d/c.do", Some("/a/b")), "d/c.do");
        assert_eq!(
            relativize("/a/bb/c.do", Some("/a/b")),
            "<abs>",
            "prefix must end at a separator"
        );
        assert_eq!(relativize("/a/b", Some("/a/b")), ".");
        assert_eq!(relativize("/a/b/c.do", None), "<abs>");
        assert_eq!(relativize("rel/c.do", Some("/a/b")), "rel/c.do");
        assert_eq!(relativize("rel\\c.do", None), "rel/c.do");
        assert_eq!(relativize("C:\\p\\c.do", Some("C:\\p")), "c.do");
    }

    #[test]
    fn asset_paths_that_are_not_ours_are_left_alone() {
        assert_eq!(asset_session_to_s0("app/index.html"), "app/index.html");
        assert_eq!(asset_session_to_s0("result"), "result");
        assert_eq!(
            asset_session_to_s0("/frame/abc/f1/page"),
            "/frame/S0/f1/page"
        );
    }
}
