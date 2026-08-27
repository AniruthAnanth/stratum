//! `set` and the `c()` class — IMPLEMENTATION_PLAN §1, A16.
//!
//! # `set linesize` is rejected, and that is the feature
//!
//! v1 renders classic output at 80 columns and accepts `set linesize 80` only.
//! Any other value is `rc = 10` with diagnostic `STRATUM0010`, and
//! [`Settings::linesize`] returns [`LINESIZE`] — a constant, not a field —
//! so there is no code path that can report anything else. That is the
//! acceptance bullet the plan assigns to this file, expressed structurally
//! rather than as a test that a future edit could route around.
//!
//! Accepting the setting and then emitting 78-column `regress` tables anyway is
//! a silent semantic divergence in a differentially-tested product, and it is
//! the first thing a 20-year Stata user notices on a window resize (spec
//! Scenario C). W16 owns not offering the control in the Classic preset; this
//! file owns saying no.
//!
//! # What `set` covers
//!
//! The plan's v1 list is `seed, type, varabbrev, more, linesize, level, obs`.
//! `obs` is not a setting — it resizes the dataset — but it is spelled `set`,
//! so it is parsed here and applied to the frame.

use stratum_core::fmt::fmt_fc;
use stratum_data::StorageType;
use stratum_parse::ast::CommandAst;
use stratum_parse::StataError;
use stratum_proto::{ScalarValue, StyleId};

use super::{err, rest, rest_span, CmdHost, CmdOutcome, CmdResult, Out};

/// The one line width v1 renders at (A16).
pub const LINESIZE: u16 = 80;

/// The confidence level a fresh session starts at.
pub const DEFAULT_LEVEL: f64 = 95.0;

/// The RNG seed a clean session starts from (ARCHITECTURE §7.7).
pub const DEFAULT_SEED: u64 = 123_456_789;

/// Session settings reachable from `set` and `c()`.
///
/// `linesize` is deliberately absent as a field — see [`Settings::linesize`].
#[derive(Clone, PartialEq, Debug)]
pub struct Settings {
    /// `set level` — the default confidence level for estimation, in percent.
    pub level: f64,
    /// `set more`. Always `false` in v1: there is no `--more--` prompt in a
    /// GUI that streams results into cards, and a clean session forces it off
    /// (ARCHITECTURE §7.7).
    pub more: bool,
    /// `set varabbrev`. When off, a bare name must match a variable exactly.
    pub varabbrev: bool,
    /// `set type` — the storage type `generate` uses when none is given.
    pub gen_type: StorageType,
    /// `set seed` — the value the session's RNG was last seeded from.
    pub seed: u64,
    /// Bumped by every `set seed`.
    ///
    /// The RNG itself lives in the session (W08b), not here. The session
    /// reseeds when this counter moves, which makes `set seed 12345` twice with
    /// the same value still restart the stream — Stata's behaviour, and what
    /// `core_surface.log`'s `assert u == u2` checks.
    pub seed_epoch: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            level: DEFAULT_LEVEL,
            more: false,
            varabbrev: true,
            gen_type: StorageType::Float,
            seed: DEFAULT_SEED,
            seed_epoch: 0,
        }
    }
}

impl Settings {
    /// The line width classic output is rendered at.
    ///
    /// A method over a constant, not a field. `c(linesize)` returns 80 in every
    /// code path because there is no other value to return (A16).
    #[must_use]
    pub fn linesize(&self) -> u16 {
        LINESIZE
    }
}

/// `set <what> [<value>] [, permanently]`.
///
/// # Errors
///
/// r(198) for an unknown setting or an unparseable value; **rc 10** for
/// `set linesize` with anything but 80 (A16).
pub fn set(host: &mut dyn CmdHost, ast: &CommandAst) -> CmdResult {
    let args = rest(ast).trim();
    let span = rest_span(ast);
    let mut words = args.split_whitespace();
    let Some(what) = words.next() else {
        // Bare `set` lists every setting in Stata; v1 does not, and saying so
        // is better than printing a subset that looks complete.
        return Err(err::unsupported("set with no setting name").at(span));
    };
    // `set x 1, permanently` — the option is accepted and ignored: there is no
    // profile.do to write to in v1, and silently *not* persisting is the
    // honest failure, since a session that claims persistence and loses it is
    // worse than one that never claimed it.
    let value: Option<&str> = words
        .next()
        .map(|v| v.trim_end_matches(',').trim())
        .filter(|v| !v.is_empty());

    match what {
        "linesize" => set_linesize(value, span),
        "level" => {
            let v = num_value(value, "level", span)?;
            if !(10.0..=99.99).contains(&v) {
                return Err(err::invalid(value.unwrap_or("level")).at(span));
            }
            host.settings_mut().level = v;
            Ok(CmdOutcome::text_only())
        }
        "more" => {
            let on = on_off(value, "more", span)?;
            host.settings_mut().more = on;
            Ok(CmdOutcome::text_only())
        }
        "varabbrev" => {
            let on = on_off(value, "varabbrev", span)?;
            host.settings_mut().varabbrev = on;
            Ok(CmdOutcome::text_only())
        }
        "type" => {
            let ty = match value {
                Some("float") => StorageType::Float,
                Some("double") => StorageType::Double,
                // Stata 18 accepts only float and double for `set type`.
                other => return Err(err::invalid(other.unwrap_or("type")).at(span)),
            };
            host.settings_mut().gen_type = ty;
            Ok(CmdOutcome::text_only())
        }
        "seed" => {
            let raw = value.ok_or_else(|| err::invalid("seed").at(span))?;
            let seed: u64 = raw.parse().map_err(|_| err::invalid(raw).at(span))?;
            let s = host.settings_mut();
            s.seed = seed;
            s.seed_epoch = s.seed_epoch.wrapping_add(1);
            Ok(CmdOutcome::text_only())
        }
        "obs" => set_obs(host, value, span),
        other => Err(err::unsupported(&format!("set {other}"))
            .token(other)
            .at(span)),
    }
}

/// A16, in one function.
fn set_linesize(value: Option<&str>, span: stratum_proto::Span) -> CmdResult {
    let raw = value.ok_or_else(|| err::invalid("linesize").at(span))?;
    let n: u32 = raw.parse().map_err(|_| err::invalid(raw).at(span))?;
    if n == u32::from(LINESIZE) {
        return Ok(CmdOutcome::text_only());
    }
    // The message is transcribed from IMPLEMENTATION_PLAN §1 verbatim. The
    // offending token is the value the user typed, not the phrase — that field
    // is what spec §21 turns into "Did you mean 80?".
    Err(err::unsupported("set linesize other than 80")
        .token(raw)
        .at(span))
}

/// `set obs N` — grow the dataset, never shrink it.
fn set_obs(host: &mut dyn CmdHost, value: Option<&str>, span: stratum_proto::Span) -> CmdResult {
    let raw = value.ok_or_else(|| err::invalid("obs").at(span))?;
    let n: u64 = raw.parse().map_err(|_| err::invalid(raw).at(span))?;
    let before = host.frames().current().n_obs();
    if n < before {
        // Stata: "may not decrease the number of observations", r(198).
        return Err(
            StataError::new(198, "may not decrease the number of observations")
                .token(raw)
                .at(span),
        );
    }
    if n > before {
        let frame = host.frames_mut().current_mut();
        frame.begin_command();
        frame.set_n_obs(n);
        frame.commit();
    }
    let mut out = Out::new();
    out.txt("Number of observations (_N) was ");
    out.res(&commas(before));
    out.txt(", now ");
    out.res(&commas(n));
    out.txt(".");
    out.nl();
    host.emit(out.runs());
    Ok(CmdOutcome::text_only())
}

/// `creturn list` — v1 prints the `c()` values it actually has.
pub fn creturn_list(host: &mut dyn CmdHost, _ast: &CommandAst) -> CmdResult {
    let mut out = Out::new();
    for name in C_NAMES {
        let Some(v) = creturn(host, name) else {
            continue;
        };
        out.txt(&format!("{:>25} = ", format!("c({name})")));
        match v {
            ScalarValue::Num { display, .. } => out.res(display.trim()),
            ScalarValue::Str { value } => out.res(&value),
        }
        out.nl();
    }
    host.emit(out.runs());
    Ok(CmdOutcome::text_only())
}

/// Every `c()` name v1 answers to, in `creturn list` order.
pub const C_NAMES: &[&str] = &[
    "N",
    "k",
    "changed",
    "level",
    "linesize",
    "matsize",
    "maxvar",
    "more",
    "obs",
    "pi",
    "rc",
    "seed",
    "type",
    "varabbrev",
    "version",
];

/// `c(name)`.
///
/// **THE single implementation.** `eval.rs` (W06a) routes `Expr::Stored { class:
/// C, .. }` here rather than answering `c()` itself; a second table is how
/// `c(linesize)` ends up disagreeing with the renderer, which is precisely the
/// divergence A16 exists to prevent.
///
/// Returns `None` for a name v1 does not implement, which the caller turns into
/// r(198). Clock- and environment-derived names (`current_date`,
/// `current_time`, `username`, `pwd`) are deliberately absent: reading them is
/// a `Taint::EXTERNAL` event that has to be recorded by `ExecCtx` (design 03
/// §4.6), so they are answered one layer up where the recording happens.
#[must_use]
pub fn creturn(host: &dyn CmdHost, name: &str) -> Option<ScalarValue> {
    let s = host.settings();
    let frame = host.frames().current();
    Some(match name {
        "N" => num(frame.n_obs() as f64),
        "k" => num(f64::from(frame.n_vars())),
        "changed" => num(if frame.changed() { 1.0 } else { 0.0 }),
        "level" => num(s.level),
        // A16: 80, always, whatever anyone did to the settings.
        "linesize" => num(f64::from(s.linesize())),
        "matsize" => num(800.0),
        "maxvar" => num(5000.0),
        "more" => str_(if s.more { "on" } else { "off" }),
        "obs" => num(frame.n_obs() as f64),
        "pi" => num(core::f64::consts::PI),
        "rc" => num(f64::from(host.last_rc())),
        "seed" => str_(&s.seed.to_string()),
        "type" => str_(match s.gen_type {
            StorageType::Double => "double",
            _ => "float",
        }),
        "varabbrev" => str_(if s.varabbrev { "on" } else { "off" }),
        "version" => num(18.5),
        _ => return None,
    })
}

fn num(v: f64) -> ScalarValue {
    ScalarValue::Num {
        value: v,
        display: stratum_core::fmt::fmt_g(v, 10).trim_start().to_owned(),
    }
}

fn str_(v: &str) -> ScalarValue {
    ScalarValue::Str {
        value: v.to_owned(),
    }
}

fn on_off(value: Option<&str>, what: &str, span: stratum_proto::Span) -> Result<bool, StataError> {
    match value {
        Some("on") => Ok(true),
        Some("off") => Ok(false),
        other => Err(err::invalid(other.unwrap_or(what)).at(span)),
    }
}

fn num_value(
    value: Option<&str>,
    what: &str,
    span: stratum_proto::Span,
) -> Result<f64, StataError> {
    let raw = value.ok_or_else(|| err::invalid(what).at(span))?;
    raw.parse::<f64>().map_err(|_| err::invalid(raw).at(span))
}

/// An integer with Stata's thousands separators: `1000` → `1,000`.
///
/// `%15.0fc` then trim, so the grouping is `stratum_core::fmt`'s and not a
/// second implementation in this file.
fn commas(n: u64) -> String {
    fmt_fc(n as f64, 21, 0).trim_start().to_owned()
}

/// The style a `c()` value is printed in, exposed so `display` renders a
/// `c()` reference the same way `creturn list` does.
pub const VALUE_STYLE: StyleId = StyleId::Result;
