//! ARCHITECTURE §8.12 / audit finding A21 — the packaged app's CSP lists every
//! URL scheme the frontend can actually fetch.
//!
//! This failure mode is invisible in development: Vite serves the frontend from
//! `http://localhost:1420`, where the CSP is not applied. It appears only in a
//! packaged build, as every inline graph rendering as a broken image and every
//! `Raw ▸` over 8 KB failing — which is spec §17's "compatibility is never
//! hidden" and §18 both going quietly wrong. So it is a build check.
//!
//! The required policy is CONTRACTS §10.2, normative. Note that Tauri v2 maps a
//! custom scheme to `http://stratum-asset.localhost/…` on Windows because
//! WebView2 cannot register a real custom scheme, which is why both spellings
//! must be present on every platform.

use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use clap::Args;
use serde_json::Value;

use crate::Ctx;

/// Per-directive minimum source lists, CONTRACTS §10.2 transcribed.
///
/// §8.12 states the rule as "enumerates every URL scheme the frontend can fetch
/// and fails if any is absent from `img-src`/`connect-src`". The union it names
/// is `stratum-asset:`, `http://stratum-asset.localhost`, `ipc:`,
/// `http://ipc.localhost`, `asset:`, `data:`, `blob:` — and §10.2 says which
/// directive each belongs to. Checking per-directive is the stricter reading
/// and the only one that matches the policy §10.2 actually writes down: `ipc:`
/// in `img-src` would be meaningless, and `data:` missing from `img-src` would
/// pass a naive union check while still breaking every inline graph.
const REQUIRED: &[(&str, &[&str])] = &[
    ("default-src", &["'self'"]),
    (
        "img-src",
        &[
            "'self'",
            "asset:",
            "data:",
            "blob:",
            "stratum-asset:",
            "http://stratum-asset.localhost",
        ],
    ),
    (
        "connect-src",
        &[
            "'self'",
            "ipc:",
            "http://ipc.localhost",
            "stratum-asset:",
            "http://stratum-asset.localhost",
        ],
    ),
    ("style-src", &["'self'", "'unsafe-inline'"]),
    ("script-src", &["'self'"]),
    ("object-src", &["'none'"]),
    ("frame-src", &["'none'"]),
];

/// Sources that must NOT appear, per directive. `script-src 'unsafe-eval'` in a
/// desktop app with filesystem access is a remote-code-execution primitive.
const BANNED: &[(&str, &[&str])] = &[
    ("script-src", &["'unsafe-inline'", "'unsafe-eval'", "*"]),
    ("default-src", &["*"]),
    ("object-src", &["*", "'self'"]),
];

#[derive(Args)]
pub struct Cmd {
    /// Tauri config to read. Defaults to `apps/desktop/src-tauri/tauri.conf.json`.
    #[arg(long, value_name = "FILE")]
    pub config: Option<Utf8PathBuf>,

    /// Fail if the config does not exist yet. CI turns this on once W17 lands.
    #[arg(long)]
    pub require: bool,
}

pub fn run(ctx: &Ctx, cmd: &Cmd) -> Result<()> {
    let path = cmd
        .config
        .clone()
        .unwrap_or_else(|| ctx.path("apps/desktop/src-tauri/tauri.conf.json"));

    if !path.is_file() {
        anyhow::ensure!(!cmd.require, "--require: {path} does not exist");
        println!("csp-check: skipped, {path} does not exist yet (W17 creates it)");
        return Ok(());
    }

    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
    let conf: Value = serde_json::from_str(&text).with_context(|| format!("parsing {path}"))?;
    let csp = conf.pointer("/app/security/csp").with_context(|| {
        format!("{path} has no `app.security.csp`; CONTRACTS §10.2 requires one")
    })?;

    let policy = Policy::from_json(csp)
        .with_context(|| format!("{path}: `app.security.csp` is neither a string nor an object"))?;

    let problems = policy.problems();
    if problems.is_empty() {
        println!(
            "csp-check: OK — {} directive(s) satisfy CONTRACTS §10.2",
            REQUIRED.len()
        );
        return Ok(());
    }
    eprintln!("csp-check: {path} violates CONTRACTS §10.2:");
    for p in &problems {
        eprintln!("    {p}");
    }
    anyhow::bail!("the packaged app's CSP is incomplete");
}

/// A parsed `Content-Security-Policy`. Tauri accepts either a single string or
/// a `{directive: sources}` object, and both spellings reach the same place.
#[derive(Debug, Default)]
pub struct Policy {
    directives: Vec<(String, Vec<String>)>,
}

impl Policy {
    pub fn from_json(value: &Value) -> Option<Self> {
        match value {
            Value::String(s) => Some(Self::parse(s)),
            Value::Object(map) => {
                let mut directives = Vec::new();
                for (name, sources) in map {
                    let list = match sources {
                        Value::String(s) => s.split_whitespace().map(str::to_owned).collect(),
                        Value::Array(items) => items
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect(),
                        _ => return None,
                    };
                    directives.push((name.to_lowercase(), list));
                }
                Some(Self { directives })
            }
            _ => None,
        }
    }

    pub fn parse(policy: &str) -> Self {
        let directives = policy
            .split(';')
            .filter_map(|clause| {
                let mut parts = clause.split_whitespace();
                let name = parts.next()?.to_lowercase();
                Some((name, parts.map(str::to_owned).collect()))
            })
            .collect();
        Self { directives }
    }

    fn sources(&self, directive: &str) -> Option<&[String]> {
        self.directives
            .iter()
            .find(|(n, _)| n == directive)
            .map(|(_, s)| s.as_slice())
    }

    /// Every way this policy falls short of CONTRACTS §10.2, in one pass, so a
    /// contributor fixes all of them rather than one per CI round-trip.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (directive, required) in REQUIRED {
            let Some(sources) = self.sources(directive) else {
                out.push(format!(
                    "`{directive}` is missing entirely; §10.2 requires `{directive} {}`",
                    required.join(" ")
                ));
                continue;
            };
            for want in *required {
                if !sources.iter().any(|s| s == want) {
                    out.push(format!("`{directive}` is missing `{want}`"));
                }
            }
        }
        for (directive, banned) in BANNED {
            let Some(sources) = self.sources(directive) else {
                continue;
            };
            for bad in *banned {
                if sources.iter().any(|s| s == bad) {
                    out.push(format!("`{directive}` must not allow `{bad}`"));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CONTRACTS §10.2, transcribed. If this stops passing, either the contract
    /// moved or the checker did, and both are worth a stop.
    const NORMATIVE: &str = "default-src 'self'; \
         img-src     'self' asset: data: blob: stratum-asset: http://stratum-asset.localhost; \
         connect-src 'self' ipc: http://ipc.localhost stratum-asset: http://stratum-asset.localhost; \
         style-src   'self' 'unsafe-inline'; \
         script-src  'self'; \
         object-src  'none'; \
         frame-src   'none';";

    #[test]
    fn the_normative_policy_passes() {
        assert!(Policy::parse(NORMATIVE).problems().is_empty());
    }

    /// The exact pre-audit defect A21 found: the asset scheme absent from both
    /// directives, which breaks only the packaged build.
    #[test]
    fn the_pre_audit_policy_fails() {
        let pre_audit = "default-src 'self'; img-src 'self' asset: data: blob:; \
             connect-src 'self' ipc: http://ipc.localhost; style-src 'self' 'unsafe-inline'; \
             script-src 'self'; object-src 'none'; frame-src 'none';";
        let problems = Policy::parse(pre_audit).problems();
        assert_eq!(problems.len(), 4, "{problems:#?}");
        assert!(problems
            .iter()
            .any(|p| p.contains("img-src") && p.contains("stratum-asset:")));
        assert!(problems
            .iter()
            .any(|p| p.contains("connect-src") && p.contains("http://stratum-asset.localhost")));
    }

    /// Dropping one spelling is the Windows-only failure; both must be present.
    #[test]
    fn one_spelling_is_not_enough() {
        let only_custom = NORMATIVE.replace(" http://stratum-asset.localhost", "");
        let problems = Policy::parse(&only_custom).problems();
        assert_eq!(problems.len(), 2, "{problems:#?}");

        let only_http = NORMATIVE.replace("stratum-asset: ", "");
        assert_eq!(Policy::parse(&only_http).problems().len(), 2);
    }

    #[test]
    fn a_missing_directive_is_reported_once_with_the_fix() {
        let no_connect = NORMATIVE
            .lines()
            .collect::<String>()
            .replace(
                "connect-src 'self' ipc: http://ipc.localhost stratum-asset: http://stratum-asset.localhost;",
                "",
            );
        let problems = Policy::parse(&no_connect).problems();
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("missing entirely"), "{problems:?}");
        assert!(problems[0].contains("ipc:"), "the message carries the fix");
    }

    #[test]
    fn unsafe_script_sources_are_rejected() {
        let unsafe_eval =
            NORMATIVE.replace("script-src  'self'", "script-src 'self' 'unsafe-eval'");
        let problems = Policy::parse(&unsafe_eval).problems();
        assert_eq!(problems, ["`script-src` must not allow `'unsafe-eval'`"]);
    }

    #[test]
    fn the_object_form_is_accepted_too() {
        let json: Value = serde_json::json!({
            "default-src": ["'self'"],
            "img-src": "'self' asset: data: blob: stratum-asset: http://stratum-asset.localhost",
            "connect-src": ["'self'", "ipc:", "http://ipc.localhost", "stratum-asset:", "http://stratum-asset.localhost"],
            "style-src": ["'self'", "'unsafe-inline'"],
            "script-src": ["'self'"],
            "object-src": ["'none'"],
            "frame-src": ["'none'"],
        });
        let policy = Policy::from_json(&json).expect("object form parses");
        assert!(policy.problems().is_empty(), "{:?}", policy.problems());
    }

    #[test]
    fn directive_names_are_case_insensitive_and_whitespace_tolerant() {
        let messy = "  Default-Src   'self' ;\nIMG-SRC 'self' asset: data: blob: stratum-asset: http://stratum-asset.localhost;\
             connect-src 'self' ipc: http://ipc.localhost stratum-asset: http://stratum-asset.localhost;\
             style-src 'self' 'unsafe-inline'; script-src 'self'; object-src 'none'; frame-src 'none'";
        assert!(Policy::parse(messy).problems().is_empty());
    }
}
