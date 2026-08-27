//! **The two-tier end-to-end harness** — plan W25, ADR-011, Q16.
//!
//! Spec §38's Scenarios A–E are the product's definition of done. Before this
//! crate they were five paragraphs of English with no driver, no harness and no
//! owner (audit finding A20), so M4 and M5 could not be evaluated at all. This
//! crate is what makes "Scenario A passes" a statement with a truth value.
//!
//! # Two tiers, one script
//!
//! * **Tier 1 — host harness. macOS, Windows, Linux.** Drives the app through
//!   `e2e_dispatch { action, args }` / `e2e_snapshot { what }`, which route into
//!   the *same* command registry the keymap and the command palette use. Fast,
//!   deterministic, and it **cannot catch a broken key binding or a CSS
//!   regression — we do not claim it does.**
//! * **Tier 2 — real WebDriver input. Windows and Linux only.** `tauri-driver`
//!   in front of Edge WebDriver / `WebKitWebDriver`, `fantoccini` as the client.
//!   Real keystrokes, real clicks, real focus.
//! * **macOS is Tier-1 only.** WKWebView exposes no WebDriver endpoint, so
//!   `tauri-driver` cannot attach. That is Q16 and ADR-011, it is a fact about
//!   the platform rather than a gap in this crate, and [`tier2::connect`] says
//!   so at run time instead of pretending.
//!
//! # How this crate stays honest while the panes are still being written
//!
//! W25 lands before W13/W14/W15/W16/W17/W18. Three mechanisms, all mechanical:
//!
//! 1. A [`Snapshot`] field is `Present` or `Unavailable { unit }` — the host
//!    says which, and an expectation over an unavailable field is
//!    [`StepOutcome::Blocked`], never a pass and never a silent skip.
//! 2. A step declares the [`Capability`] set it needs. If the host does not
//!    advertise one, the step is blocked *without being dispatched*, so a
//!    report never contains "ran `run.blockAndAdvance`, nothing happened".
//! 3. **A capability that is advertised is then held to it.** If the host claims
//!    `Editor` and `run.blockAndAdvance` comes back `unknown`, that is a
//!    [`StepOutcome::Failed`], not a blocked step. This is the mechanism that
//!    makes the gap close itself out loud when W13 lands rather than rotting.
//!
//! `cargo xtask e2e --tier 1 --require-complete` is the M4/M5 gate: it fails on
//! any blocked step. Without the flag, blocked steps are reported and exit 0,
//! because a job that is red for work nobody has started yet trains people to
//! ignore red (ci.yml's `preflight` makes the same argument).
//!
//! # Counters, not stopwatches (ADR-017)
//!
//! The plan says a Tier-1 scenario is "fast (< 3 s per scenario)". ADR-017 is
//! binding and forbids a wall-clock acceptance gate, so the property is asserted
//! as the counters that *cause* it — one round trip per dispatch, one per
//! snapshot, **zero sleeps and zero polls** — and the duration is recorded
//! beside them. A harness that waits by sleeping passes a 3-second budget on a
//! fast machine and fails it on a loaded one; a harness with `sleeps == 0` is
//! fast for a reason that does not depend on the machine.

#![forbid(unsafe_code)]

// `tests/e2e/*.rs` are compiled INTO this crate (see the `#[path]` at the foot
// of this file) but are owned by five different units and are written against
// the public API, not against `crate::`. This alias makes `stratum_e2e::…`
// resolve inside the crate too, so a scenario file reads the same whether it is
// compiled here or, later, as an ordinary integration test.
extern crate self as stratum_e2e;

pub mod actions;
pub mod compare;
pub mod fence;
pub mod fixtures;
pub mod host;
pub mod snapshot;
pub mod tier1;
pub mod tier2;

use std::collections::BTreeMap;
use std::fmt;
use std::time::Instant;

use serde::{Deserialize, Serialize};

pub use actions::{Action, Chord, Dispatched, Target};
pub use snapshot::{Expect, Field, Glyph, Section, Snapshot, Verdict, What};

/// Which tier a driver implements.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Host harness, in-process command dispatch. All three OSes.
    One,
    /// Real WebDriver input through `tauri-driver`. Windows and Linux.
    Two,
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::One => "tier 1",
            Self::Two => "tier 2",
        })
    }
}

/// A part of the product a scenario step needs in order to mean anything.
///
/// The owning unit is attached to the capability rather than to the step, so a
/// blocked ledger reads "3 steps blocked on W13" without any scenario having to
/// name a work unit.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// A command registry that `e2e_dispatch` can route into. W12.
    Commands,
    /// Keymap tries loaded from `resources/keymaps/*.json`. W12.
    Keymap,
    /// Layout presets and the dock. W12.
    Layout,
    /// User settings, including the inline-results mode. W12.
    Settings,
    /// The result store. W12 owns the store; W14/W15 own the cards over it.
    Results,
    /// The host will accept `EngineEvent`s from the harness in place of an
    /// engine. True of the pre-host bridge, false of the packaged app — which
    /// is why [`Action::Run`] names both mechanisms and lets the driver choose.
    EventInjection,
    /// The Review history store. W12.
    History,
    /// A CodeMirror document with a caret, and edits that go through it. W13.
    Editor,
    /// Gutter glyphs beside blocks. W13.
    Gutter,
    /// Rendered result cards with headers and bodies. W14.
    Cards,
    /// Pane contents as text — Results, History, Variables. W16.
    Panes,
    /// The Data Editor. W18.
    DataEditor,
    /// A real engine on the other end of the transport. W08/W09; until then the
    /// harness plays the engine with W07's committed stream.
    Engine,
}

impl Capability {
    /// The unit that owes this capability, for the blocked ledger.
    #[must_use]
    pub const fn owner(self) -> &'static str {
        match self {
            Self::Commands
            | Self::Keymap
            | Self::Layout
            | Self::Settings
            | Self::Results
            | Self::History
            | Self::EventInjection => "W12",
            Self::Editor | Self::Gutter => "W13",
            Self::Cards => "W14",
            Self::Panes => "W16",
            Self::DataEditor => "W18",
            Self::Engine => "W09",
        }
    }
}

/// The set of capabilities a host advertises in its `hello`.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Capabilities(pub Vec<Capability>);

impl Capabilities {
    /// Whether the host claims this capability.
    #[must_use]
    pub fn has(&self, c: Capability) -> bool {
        self.0.contains(&c)
    }

    /// The first capability in `needs` the host does not claim.
    #[must_use]
    pub fn missing(&self, needs: &[Capability]) -> Option<Capability> {
        needs.iter().copied().find(|c| !self.has(*c))
    }
}

/// The five acceptance scenarios of spec §38.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioId {
    /// Notebook-like analysis.
    A,
    /// Stale state.
    B,
    /// Classic workflow.
    C,
    /// Interoperability.
    D,
    /// Cross-platform.
    E,
}

impl fmt::Display for ScenarioId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
            Self::D => "D",
            Self::E => "E",
        })
    }
}

/// One step of a scenario: do a thing, then assert about what the app looks like.
#[derive(Clone, PartialEq, Debug)]
pub struct Step {
    /// What this step is *for*, in the language of spec §38. Printed verbatim in
    /// the report, so the report reads as the scenario rather than as a log.
    pub what: &'static str,
    /// The thing to do.
    pub action: Action,
    /// Capabilities without which this step means nothing.
    pub requires: Vec<Capability>,
    /// What must be true afterwards.
    pub expect: Vec<Expect>,
}

impl Step {
    /// A step with no capability requirements beyond a command registry.
    #[must_use]
    pub fn new(what: &'static str, action: Action) -> Self {
        Self {
            what,
            action,
            requires: vec![Capability::Commands],
            expect: Vec::new(),
        }
    }

    /// Declare what this step needs.
    #[must_use]
    pub fn needs(mut self, caps: &[Capability]) -> Self {
        for c in caps {
            if !self.requires.contains(c) {
                self.requires.push(*c);
            }
        }
        self
    }

    /// Add an expectation.
    #[must_use]
    pub fn expect(mut self, e: Expect) -> Self {
        self.expect.push(e);
        self
    }

    /// The snapshot sections this step's expectations actually read.
    #[must_use]
    pub fn sections(&self) -> What {
        let mut want: Vec<Section> = Vec::new();
        let mut add = |s: Section| {
            if !want.contains(&s) {
                want.push(s);
            }
        };
        for e in &self.expect {
            match e {
                Expect::DocEquals(_) | Expect::CaretAt(_) => add(Section::Doc),
                Expect::CaretInBlock(_) => {
                    add(Section::Doc);
                    add(Section::Blocks);
                }
                Expect::GutterIs(..) => add(Section::Gutter),
                Expect::BlockStatusIs(..) => add(Section::Blocks),
                Expect::CardsForBlock(..)
                | Expect::CardHeaderIs(..)
                | Expect::CardBodyContains(..)
                | Expect::CardOrderIs(_) => add(Section::Cards),
                Expect::ResultsForBlock(..)
                | Expect::ResultRawContains(..)
                | Expect::ResultOrderIs(_)
                | Expect::ResultPayloadIs(..) => add(Section::Results),
                Expect::LayoutIs(_) | Expect::InlineResultsIs(_) => add(Section::Layout),
                Expect::PaneVisible(_) | Expect::PaneHidden(_) | Expect::PaneContains(..) => {
                    add(Section::Panes);
                }
                Expect::FocusIs(_) => add(Section::Focus),
                Expect::HistoryTailIs(_) => add(Section::History),
            }
        }
        What(want)
    }
}

/// One acceptance scenario.
#[derive(Clone, PartialEq, Debug)]
pub struct Scenario {
    /// A–E.
    pub id: ScenarioId,
    /// The §38 sentence this scenario is.
    pub title: &'static str,
    /// The steps, in order.
    pub steps: Vec<Step>,
}

// ---------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------

/// What went wrong talking to a host.
#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    /// The host did not answer inside the deadline. Carries the operation so the
    /// report can say *what* we were waiting for.
    #[error("no answer to {op} within {after_ms} ms")]
    Timeout {
        /// The operation that hung.
        op: String,
        /// The deadline, in whole milliseconds.
        after_ms: u64,
    },
    /// The control channel broke.
    #[error("transport: {0}")]
    Transport(String),
    /// The host answered, and the answer was an error.
    #[error("host: {0}")]
    Host(String),
    /// This tier cannot run here, and here is why.
    #[error("{0}")]
    Unsupported(String),
}

/// Counted work, per ADR-017. Every acceptance counter this crate asserts is a
/// field of this struct.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Counters {
    /// `e2e_dispatch` calls.
    pub dispatches: u32,
    /// `e2e_snapshot` calls.
    pub snapshots: u32,
    /// Request/response pairs on the control channel, `hello` included.
    pub round_trips: u32,
    /// **Must stay 0.** A harness that waits by sleeping has a wall-clock
    /// dependency baked into its result.
    pub sleeps: u32,
    /// **Must stay 0.** Re-asking for a snapshot until it changes is the same
    /// bug wearing a loop.
    pub polls: u32,
    /// Engine events handed to the frontend.
    pub events_fed: u32,
    /// Bytes read from the control channel.
    pub bytes_rx: u64,
    /// Bytes written to the control channel.
    pub bytes_tx: u64,
}

/// Something that can drive the app.
///
/// Object-safe on purpose: `xtask e2e` picks a tier at run time from a flag, and
/// a generic would push that choice into the type system where a command-line
/// flag cannot reach it.
pub trait Driver {
    /// Which tier this is.
    fn tier(&self) -> Tier;
    /// A human name for the thing on the other end, printed with every failure.
    fn host(&self) -> String;
    /// What the host claims it can do.
    fn capabilities(&self) -> Capabilities;
    /// Do a thing.
    ///
    /// # Errors
    /// Transport failures, host-side errors, and deadline expiry.
    fn dispatch(&mut self, action: &Action) -> Result<Dispatched, DriverError>;
    /// Look at the app.
    ///
    /// # Errors
    /// Transport failures, host-side errors, and deadline expiry.
    fn snapshot(&mut self, what: &What) -> Result<Snapshot, DriverError>;
    /// Counted work so far.
    fn counters(&self) -> Counters;
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

/// What happened to one step.
#[derive(Clone, PartialEq, Debug)]
pub enum StepOutcome {
    /// Every expectation held.
    Passed,
    /// At least one expectation did not hold.
    Failed(Vec<Verdict>),
    /// Every expectation this step could evaluate held, and at least one could
    /// not be evaluated because the field it reads is not written yet.
    ///
    /// The distinction from [`Self::Blocked`] is the difference between "this
    /// proved nothing" and "this proved what there is to prove". Scenario A's
    /// `summarize` step asserts both that StataMP's own row lands in the result
    /// store *and* that a card renders it; the first is true today and the
    /// second is W14's. Collapsing the two into one verdict would throw away a
    /// real assertion, and a harness that reports less than it knows is a
    /// harness people stop reading.
    Partial {
        /// Expectations that held.
        passed: usize,
        /// Unit → how many expectations it owes, for the ledger.
        blocked: BTreeMap<String, usize>,
        /// The first blocked expectation, for the report line.
        why: String,
    },
    /// The step needs something nobody has written yet.
    Blocked {
        /// The unit that owes it.
        unit: String,
        /// What is missing.
        why: String,
    },
    /// The host stopped answering. **Carries the last snapshot we hold**, which
    /// is the acceptance bullet: a timeout must report what the app looked like,
    /// not the word "timeout".
    TimedOut {
        /// The operation that hung.
        op: String,
        /// The most recent successful snapshot, if there was one.
        last: Option<Box<Snapshot>>,
    },
}

/// One step's line in the report.
#[derive(Clone, PartialEq, Debug)]
pub struct StepReport {
    /// Zero-based index in the scenario.
    pub index: usize,
    /// [`Step::what`].
    pub what: &'static str,
    /// [`Action::label`].
    pub action: String,
    /// How the host carried it out — `verb`, `chord`, `injection`, `observe`.
    /// Empty for a step that was never dispatched.
    pub via: String,
    /// The verdict.
    pub outcome: StepOutcome,
}

/// What happened to one scenario.
#[derive(Clone, PartialEq, Debug)]
pub struct ScenarioReport {
    /// A–E.
    pub id: ScenarioId,
    /// The §38 sentence.
    pub title: &'static str,
    /// Which tier ran it.
    pub tier: Tier,
    /// What drove it.
    pub host: String,
    /// One entry per step.
    pub steps: Vec<StepReport>,
    /// Counted work (ADR-017).
    pub counters: Counters,
    /// **Recorded, never asserted** (ADR-017). Whole milliseconds, because a
    /// float precision spec in a `crates/` file is what `check-topology.sh`'s
    /// `number-format` scan exists to catch.
    pub elapsed_ms: u64,
}

impl ScenarioReport {
    /// Steps that passed.
    #[must_use]
    pub fn passed(&self) -> usize {
        self.count(|o| matches!(o, StepOutcome::Passed))
    }

    /// Steps that failed.
    #[must_use]
    pub fn failed(&self) -> usize {
        self.count(|o| matches!(o, StepOutcome::Failed(_) | StepOutcome::TimedOut { .. }))
    }

    /// Steps blocked on a unit that has not landed.
    #[must_use]
    pub fn blocked(&self) -> usize {
        self.count(|o| matches!(o, StepOutcome::Blocked { .. }))
    }

    /// Steps that proved part of what they assert.
    #[must_use]
    pub fn partial(&self) -> usize {
        self.count(|o| matches!(o, StepOutcome::Partial { .. }))
    }

    /// Expectations — not steps — that were evaluated and held. The honest
    /// answer to "how much of this scenario is real today".
    #[must_use]
    pub fn assertions_passed(&self) -> usize {
        self.steps
            .iter()
            .map(|s| match &s.outcome {
                StepOutcome::Partial { passed, .. } => *passed,
                _ => 0,
            })
            .sum::<usize>()
            + self
                .steps
                .iter()
                .filter(|s| matches!(s.outcome, StepOutcome::Passed))
                .count()
    }

    fn count(&self, f: impl Fn(&StepOutcome) -> bool) -> usize {
        self.steps.iter().filter(|s| f(&s.outcome)).count()
    }

    /// Blocked steps grouped by the unit that owes them.
    #[must_use]
    pub fn blocked_ledger(&self) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        for s in &self.steps {
            match &s.outcome {
                StepOutcome::Blocked { unit, .. } => *out.entry(unit.clone()).or_insert(0) += 1,
                StepOutcome::Partial { blocked, .. } => {
                    for (unit, n) in blocked {
                        *out.entry(unit.clone()).or_insert(0) += n;
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// Nothing failed. **Not** the same as "the scenario is proven": see
    /// [`Self::is_complete`].
    #[must_use]
    pub fn is_green(&self) -> bool {
        self.failed() == 0
    }

    /// Nothing failed *and* nothing was blocked, partially or wholly — the only
    /// state in which "Scenario X passes" is a true sentence.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.is_green() && self.blocked() == 0 && self.partial() == 0
    }

    /// A **platform- and timing-free** rendering, for spec §38-E.
    ///
    /// Scenario E is "the same analysis file produces equivalent runtime results"
    /// on macOS, Windows and Linux. `e2e.yml` runs the same scenario on all
    /// three and compares this string byte for byte, so it must contain what the
    /// app *did* and nothing about where it ran: no host name, no durations, no
    /// paths. `render()` is for a human reading a failure; this is for `diff`.
    #[must_use]
    pub fn transcript(&self) -> String {
        let mut out = format!("scenario {}\n", self.id);
        for s in &self.steps {
            let outcome = match &s.outcome {
                StepOutcome::Passed => "passed".to_owned(),
                StepOutcome::Partial {
                    passed, blocked, ..
                } => {
                    let owed: Vec<String> =
                        blocked.iter().map(|(u, n)| format!("{u}x{n}")).collect();
                    format!("partial {passed} blocked {}", owed.join(","))
                }
                StepOutcome::Blocked { unit, .. } => format!("blocked {unit}"),
                StepOutcome::Failed(_) => "failed".to_owned(),
                StepOutcome::TimedOut { op, .. } => format!("timeout {op}"),
            };
            out.push_str(&format!(
                "{} {} via {} {}\n",
                s.index, s.what, s.via, outcome
            ));
        }
        out.push_str(&format!(
            "counters dispatches={} snapshots={} round_trips={} sleeps={} polls={} events_fed={}\n",
            self.counters.dispatches,
            self.counters.snapshots,
            self.counters.round_trips,
            self.counters.sleeps,
            self.counters.polls,
            self.counters.events_fed,
        ));
        out
    }

    /// The human report. One line per step, then the ledger.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "scenario {} — {} [{} · {}]\n",
            self.id, self.title, self.tier, self.host
        ));
        for s in &self.steps {
            let (mark, detail) = match &s.outcome {
                StepOutcome::Passed => ("ok  ", String::new()),
                StepOutcome::Blocked { unit, why } => ("BLOCK", format!("  ({unit}: {why})")),
                StepOutcome::Partial {
                    passed,
                    blocked,
                    why,
                } => (
                    "part",
                    format!(
                        "  ({passed} of {} held; {why})",
                        passed + blocked.values().sum::<usize>()
                    ),
                ),
                StepOutcome::Failed(vs) => {
                    let mut d = String::new();
                    for v in vs {
                        if let Verdict::Failed { expected, actual } = v {
                            d.push_str(&format!(
                                "\n        expected {expected}\n        actual   {actual}"
                            ));
                        }
                    }
                    ("FAIL", d)
                }
                StepOutcome::TimedOut { op, last } => (
                    "TIMEOUT",
                    match last {
                        None => format!(
                            "  waiting for {op}; no snapshot was ever taken, so there is \
                             nothing to show — the host never answered at all"
                        ),
                        Some(snap) => format!(
                            "  waiting for {op}; last snapshot follows\n{}",
                            indent(&render_snapshot(snap))
                        ),
                    },
                ),
            };
            let via = if s.via.is_empty() {
                String::new()
            } else {
                format!(" [via {}]", s.via)
            };
            out.push_str(&format!(
                "  {mark} [{}] {} — {}{via}{detail}\n",
                s.index, s.what, s.action
            ));
        }
        out.push_str(&format!(
            "  {} passed, {} partial, {} blocked, {} failed · {} assertions held · \
             {} dispatches, {} snapshots, \
             {} round trips, {} sleeps, {} polls · recorded {} ms\n",
            self.passed(),
            self.partial(),
            self.blocked(),
            self.failed(),
            self.assertions_passed(),
            self.counters.dispatches,
            self.counters.snapshots,
            self.counters.round_trips,
            self.counters.sleeps,
            self.counters.polls,
            self.elapsed_ms,
        ));
        for (unit, n) in self.blocked_ledger() {
            // Assertions, not steps: a step can be partly blocked, and counting
            // it as a whole blocked step would overstate what is missing.
            out.push_str(&format!("  blocked on {unit}: {n} assertion(s)\n"));
        }
        out
    }
}

fn indent(s: &str) -> String {
    s.lines()
        .map(|l| format!("        {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A snapshot, rendered for a human reading a failure.
#[must_use]
pub fn render_snapshot(s: &Snapshot) -> String {
    serde_json::to_string_pretty(s).unwrap_or_else(|e| format!("<unrenderable snapshot: {e}>"))
}

// ---------------------------------------------------------------------------
// The runner
// ---------------------------------------------------------------------------

/// Knobs the runner takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct RunOptions {
    /// Stop at the first failure. Off by default: a report that shows every
    /// blocked step is worth more during the build-out than one that stops at
    /// the first thing W13 has not written.
    pub fail_fast: bool,
}

/// Run one scenario against one driver.
///
/// Never returns `Err`: a driver failure *is* the result, and the caller needs
/// the partial report — especially the last snapshot — more than it needs an
/// error type.
#[must_use]
pub fn run_scenario(
    driver: &mut dyn Driver,
    scenario: &Scenario,
    opts: RunOptions,
) -> ScenarioReport {
    let started = Instant::now();
    let caps = driver.capabilities();
    let mut steps = Vec::with_capacity(scenario.steps.len());
    let mut last: Option<Box<Snapshot>> = None;

    for (index, step) in scenario.steps.iter().enumerate() {
        let mut via = String::new();
        let outcome = run_step(driver, step, &caps, &mut last, &mut via);
        let stop = opts.fail_fast
            && matches!(
                outcome,
                StepOutcome::Failed(_) | StepOutcome::TimedOut { .. }
            );
        steps.push(StepReport {
            index,
            what: step.what,
            action: step.action.label(),
            via,
            outcome,
        });
        if stop {
            break;
        }
    }

    ScenarioReport {
        id: scenario.id,
        title: scenario.title,
        tier: driver.tier(),
        host: driver.host(),
        steps,
        counters: driver.counters(),
        // Recorded, not asserted (ADR-017). `as` rather than a float division:
        // the number is a record, and a lossless one at this magnitude.
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    }
}

fn run_step(
    driver: &mut dyn Driver,
    step: &Step,
    caps: &Capabilities,
    last: &mut Option<Box<Snapshot>>,
    via: &mut String,
) -> StepOutcome {
    if let Some(missing) = caps.missing(&step.requires) {
        return StepOutcome::Blocked {
            unit: missing.owner().to_owned(),
            why: format!("the host does not implement {missing:?}"),
        };
    }

    let dispatched = match driver.dispatch(&step.action) {
        Ok(d) => d,
        Err(DriverError::Timeout { op, .. }) => {
            return StepOutcome::TimedOut {
                op,
                last: last.clone(),
            }
        }
        Err(e) => {
            return StepOutcome::Failed(vec![Verdict::Failed {
                expected: format!("the host to run {}", step.action.label()),
                actual: e.to_string(),
            }])
        }
    };

    via.clone_from(&dispatched.via);

    // A host that advertised the capability and then answered `unknown` has lied
    // about its surface. That is a defect in the host, not a gap in the plan, so
    // it fails rather than blocking — which is what makes the blocked ledger
    // shrink honestly as units land instead of quietly turning into passes.
    if dispatched.result == "unknown" {
        return StepOutcome::Failed(vec![Verdict::Failed {
            expected: format!(
                "{} to be registered — the host advertised {:?}",
                step.action.label(),
                step.requires
            ),
            actual: "the command registry does not know that id".to_owned(),
        }]);
    }
    if dispatched.result == "disabled" {
        return StepOutcome::Failed(vec![Verdict::Failed {
            expected: format!("{} to be enabled in this context", step.action.label()),
            actual: "the command's `enabled` predicate said no".to_owned(),
        }]);
    }

    // Tier 1's own contribution to key-binding coverage: the live trie must
    // resolve the chord the script names to the command the script dispatches.
    if let Action::Verb {
        command,
        chord: Some(Chord(chord)),
        ..
    } = &step.action
    {
        if caps.has(Capability::Keymap) {
            match &dispatched.chord_resolves_to {
                Some(resolved) if resolved == command => {}
                other => {
                    return StepOutcome::Failed(vec![Verdict::Failed {
                        expected: format!("{chord} to be bound to {command}"),
                        actual: match other {
                            Some(r) => format!("{chord} resolves to {r}"),
                            None => format!("{chord} is bound to nothing in the active keymap"),
                        },
                    }])
                }
            }
        }
    }

    if step.expect.is_empty() {
        return StepOutcome::Passed;
    }

    let want = step.sections();
    let snap = match driver.snapshot(&want) {
        Ok(s) => s,
        Err(DriverError::Timeout { op, .. }) => {
            return StepOutcome::TimedOut {
                op,
                last: last.clone(),
            }
        }
        Err(e) => {
            return StepOutcome::Failed(vec![Verdict::Failed {
                expected: "a snapshot".to_owned(),
                actual: e.to_string(),
            }])
        }
    };
    *last = Some(Box::new(snap.clone()));

    // EVERY expectation is evaluated, not just up to the first one that cannot
    // be. A step that asserts both "the golden row is in the result store"
    // (true today) and "a card renders it" (W14's) has proved the first, and
    // the report says so.
    let verdicts: Vec<Verdict> = step.expect.iter().map(|e| e.check(&snap)).collect();
    if verdicts.iter().any(|v| matches!(v, Verdict::Failed { .. })) {
        return StepOutcome::Failed(verdicts);
    }

    let mut blocked: BTreeMap<String, usize> = BTreeMap::new();
    let mut first_why = String::new();
    let mut passed = 0;
    for v in &verdicts {
        match v {
            Verdict::Passed => passed += 1,
            Verdict::Blocked { unit, why } => {
                *blocked.entry(unit.clone()).or_insert(0) += 1;
                if first_why.is_empty() {
                    // Without the unit prefix: `Blocked`'s own line already
                    // prints the unit, and "(W13: W13: …)" reads like a bug.
                    first_why.clone_from(why);
                }
            }
            Verdict::Failed { .. } => unreachable!("handled above"),
        }
    }

    if blocked.is_empty() {
        return StepOutcome::Passed;
    }
    if passed == 0 {
        let (unit, _) = blocked.iter().next().expect("non-empty");
        return StepOutcome::Blocked {
            unit: unit.clone(),
            why: first_why,
        };
    }
    StepOutcome::Partial {
        passed,
        blocked,
        why: first_why,
    }
}

// ---------------------------------------------------------------------------
// The harness's own self-test (plan W25, acceptance bullet 4)
// ---------------------------------------------------------------------------
//
// "The harness itself has a self-test: a deliberately broken assertion fails,
// and a scenario that times out reports the last snapshot rather than 'timed
// out'." A harness nobody has ever seen fail is a harness nobody knows works.
#[cfg(test)]
mod selftest {
    use super::*;
    use crate::snapshot::{DocView, LayoutView};

    /// A driver whose answers are written down in the test. This is a double for
    /// the HARNESS, not for the product: it exists so the runner's failure paths
    /// can be observed. It is `cfg(test)` and never leaves this file.
    struct ScriptedDriver {
        caps: Capabilities,
        answers: Vec<Result<Snapshot, DriverError>>,
        dispatch: Vec<Result<Dispatched, DriverError>>,
        counters: Counters,
    }

    fn ran() -> Dispatched {
        Dispatched {
            via: "verb".to_owned(),
            result: "ran".to_owned(),
            chord_resolves_to: None,
            events_applied: 0,
        }
    }

    fn snap_with_layout(id: &str) -> Snapshot {
        let mut s = Snapshot::all_unavailable("scripted", "W99", "the double provides nothing");
        s.layout = Field::Present(LayoutView {
            id: id.to_owned(),
            inline_results: "off".to_owned(),
        });
        s.doc = Field::Present(DocView {
            path: Some("auto.do".to_owned()),
            text: "sysuse auto, clear\n".to_owned(),
            caret: 3,
            version: 1,
        });
        s
    }

    impl Driver for ScriptedDriver {
        fn tier(&self) -> Tier {
            Tier::One
        }
        fn host(&self) -> String {
            "scripted".to_owned()
        }
        fn capabilities(&self) -> Capabilities {
            self.caps.clone()
        }
        fn dispatch(&mut self, _action: &Action) -> Result<Dispatched, DriverError> {
            self.counters.dispatches += 1;
            self.counters.round_trips += 1;
            if self.dispatch.is_empty() {
                return Ok(ran());
            }
            self.dispatch.remove(0)
        }
        fn snapshot(&mut self, _what: &What) -> Result<Snapshot, DriverError> {
            self.counters.snapshots += 1;
            self.counters.round_trips += 1;
            if self.answers.is_empty() {
                return Err(DriverError::Host(
                    "the script ran out of answers".to_owned(),
                ));
            }
            self.answers.remove(0)
        }
        fn counters(&self) -> Counters {
            self.counters
        }
    }

    fn scenario(steps: Vec<Step>) -> Scenario {
        Scenario {
            id: ScenarioId::A,
            title: "the harness's own self-test",
            steps,
        }
    }

    #[test]
    fn a_deliberately_broken_assertion_fails() {
        let mut d = ScriptedDriver {
            caps: Capabilities(vec![Capability::Commands, Capability::Layout]),
            answers: vec![Ok(snap_with_layout("modern"))],
            dispatch: Vec::new(),
            counters: Counters::default(),
        };
        let s = scenario(vec![
            Step::new("switch layout", Action::command("layout.apply"))
            .needs(&[Capability::Layout])
            // The app is in `modern`; the scenario insists on `classic`.
            .expect(Expect::LayoutIs("classic".to_owned())),
        ]);

        let report = run_scenario(&mut d, &s, RunOptions::default());
        assert_eq!(report.failed(), 1, "{}", report.render());
        assert!(!report.is_green());
        let rendered = report.render();
        assert!(rendered.contains("\"classic\""), "{rendered}");
        assert!(rendered.contains("\"modern\""), "{rendered}");
    }

    #[test]
    fn a_timeout_reports_the_last_snapshot_rather_than_the_word_timeout() {
        let mut d = ScriptedDriver {
            caps: Capabilities(vec![Capability::Commands, Capability::Layout]),
            answers: vec![
                Ok(snap_with_layout("modern")),
                Err(DriverError::Timeout {
                    op: "e2e_snapshot".to_owned(),
                    after_ms: 5_000,
                }),
            ],
            dispatch: Vec::new(),
            counters: Counters::default(),
        };
        let s = scenario(vec![
            Step::new("first, which answers", Action::command("layout.apply"))
                .needs(&[Capability::Layout])
                .expect(Expect::LayoutIs("modern".to_owned())),
            Step::new("second, which hangs", Action::command("layout.apply"))
                .needs(&[Capability::Layout])
                .expect(Expect::LayoutIs("classic".to_owned())),
        ]);

        let report = run_scenario(&mut d, &s, RunOptions::default());
        let last = report.steps.last().expect("two steps");
        match &last.outcome {
            StepOutcome::TimedOut { op, last } => {
                assert_eq!(op, "e2e_snapshot");
                let snap = last.as_ref().expect("the previous snapshot is retained");
                assert!(snap.doc.is_present());
            }
            other => panic!("expected a timeout, got {other:?}"),
        }
        // The acceptance bullet is about what the REPORT says, so assert on it.
        let rendered = report.render();
        assert!(rendered.contains("last snapshot follows"), "{rendered}");
        assert!(rendered.contains("sysuse auto, clear"), "{rendered}");
    }

    #[test]
    fn a_timeout_with_no_snapshot_yet_says_so_instead_of_showing_nothing() {
        let mut d = ScriptedDriver {
            caps: Capabilities(vec![Capability::Commands, Capability::Layout]),
            answers: vec![Err(DriverError::Timeout {
                op: "e2e_snapshot".to_owned(),
                after_ms: 5_000,
            })],
            dispatch: Vec::new(),
            counters: Counters::default(),
        };
        let s = scenario(vec![Step::new(
            "hangs immediately",
            Action::command("layout.apply"),
        )
        .needs(&[Capability::Layout])
        .expect(Expect::LayoutIs("classic".to_owned()))]);

        let report = run_scenario(&mut d, &s, RunOptions::default());
        let rendered = report.render();
        assert!(rendered.contains("never answered at all"), "{rendered}");
    }

    #[test]
    fn a_missing_capability_blocks_the_step_and_never_dispatches_it() {
        let mut d = ScriptedDriver {
            caps: Capabilities(vec![Capability::Commands]),
            answers: Vec::new(),
            dispatch: Vec::new(),
            counters: Counters::default(),
        };
        let s = scenario(vec![Step::new(
            "move the caret",
            Action::PlaceCaret { offset: 20 },
        )
        .needs(&[Capability::Editor])
        .expect(Expect::CaretAt(20))]);

        let report = run_scenario(&mut d, &s, RunOptions::default());
        assert_eq!(report.blocked(), 1);
        assert!(report.is_green(), "blocked is not failed");
        assert!(!report.is_complete(), "blocked is not proven either");
        assert_eq!(report.blocked_ledger().get("W13"), Some(&1));
        assert_eq!(
            report.counters.dispatches, 0,
            "a blocked step must not be dispatched: 'ran it, nothing happened' is not a test result"
        );
    }

    #[test]
    fn an_advertised_capability_that_answers_unknown_is_a_failure_not_a_block() {
        let mut d = ScriptedDriver {
            caps: Capabilities(vec![Capability::Commands, Capability::Editor]),
            answers: Vec::new(),
            dispatch: vec![Ok(Dispatched {
                via: "verb".to_owned(),
                result: "unknown".to_owned(),
                chord_resolves_to: None,
                events_applied: 0,
            })],
            counters: Counters::default(),
        };
        let s = scenario(vec![Step::new(
            "run the block",
            Action::command("run.blockAndAdvance"),
        )
        .needs(&[Capability::Editor])]);

        let report = run_scenario(&mut d, &s, RunOptions::default());
        assert_eq!(report.failed(), 1, "{}", report.render());
        assert!(report.render().contains("advertised"));
    }

    #[test]
    fn a_chord_bound_to_the_wrong_command_fails_the_step() {
        let mut d = ScriptedDriver {
            caps: Capabilities(vec![Capability::Commands, Capability::Keymap]),
            answers: Vec::new(),
            dispatch: vec![Ok(Dispatched {
                via: "chord".to_owned(),
                result: "ran".to_owned(),
                chord_resolves_to: Some("run.block".to_owned()),
                events_applied: 0,
            })],
            counters: Counters::default(),
        };
        let s = scenario(vec![Step::new(
            "Shift+Enter runs and advances",
            Action::verb("run.blockAndAdvance", Chord::new("Shift+Enter")),
        )
        .needs(&[Capability::Keymap])]);

        let report = run_scenario(&mut d, &s, RunOptions::default());
        assert_eq!(report.failed(), 1);
        assert!(report
            .render()
            .contains("Shift+Enter resolves to run.block"));
    }

    #[test]
    fn the_runner_takes_one_round_trip_per_dispatch_and_one_per_snapshot() {
        // ADR-017: this is the counter that expresses the plan's "< 3 s per
        // scenario". Two steps, one of which asserts nothing and therefore
        // takes no snapshot: 2 dispatches + 1 snapshot = 3 round trips, and
        // zero sleeps and zero polls no matter how slow the machine is.
        let mut d = ScriptedDriver {
            caps: Capabilities(vec![Capability::Commands, Capability::Layout]),
            answers: vec![Ok(snap_with_layout("classic"))],
            dispatch: Vec::new(),
            counters: Counters::default(),
        };
        let s = scenario(vec![
            Step::new("no assertion", Action::command("view.cycleInlineResults")),
            Step::new("one assertion", Action::command("layout.apply"))
                .needs(&[Capability::Layout])
                .expect(Expect::LayoutIs("classic".to_owned())),
        ]);

        let report = run_scenario(&mut d, &s, RunOptions::default());
        assert!(report.is_complete(), "{}", report.render());
        assert_eq!(report.counters.dispatches, 2);
        assert_eq!(report.counters.snapshots, 1);
        assert_eq!(report.counters.round_trips, 3);
        assert_eq!(report.counters.sleeps, 0);
        assert_eq!(report.counters.polls, 0);
    }

    #[test]
    fn a_step_asks_only_for_the_snapshot_sections_it_asserts_on() {
        // `doc.toString()` on a 2 MB buffer is the expensive field. A step that
        // asserts on the layout must not drag the document across the channel.
        let step = Step::new("switch layout", Action::command("layout.apply"))
            .expect(Expect::LayoutIs("classic".to_owned()));
        assert_eq!(step.sections(), What(vec![Section::Layout]));

        let step =
            Step::new("caret", Action::PlaceCaret { offset: 1 }).expect(Expect::CaretInBlock(1));
        assert_eq!(step.sections(), What(vec![Section::Doc, Section::Blocks]));
    }
}

// The scenario scripts and the fixture corpus they read live in `fixtures.rs`;
// `tests/e2e/mod.rs` is the aggregator that turns them into `cargo nextest`
// cases and pulls in W26's `tests/e2e/scenario_d.rs`.
#[cfg(test)]
#[path = "../../../tests/e2e/mod.rs"]
mod e2e;
