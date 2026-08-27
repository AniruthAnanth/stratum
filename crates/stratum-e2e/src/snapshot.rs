//! What a scenario is allowed to look at, and what it is allowed to conclude.
//!
//! [`Snapshot`] is the reply to `e2e_snapshot { what }` — plan W25: "gutter
//! glyphs, card headers and bodies, pane contents, caret offset and
//! `doc.toString()`". Both tiers return the same structure, because the whole
//! point of the two-tier design is that one scenario script drives both.
//!
//! # Why every field is a [`Field`] and not a bare value
//!
//! W25 lands before W13 (editor), W14 (renderers), W15 (results), W16 (classic
//! panes) and W18 (Data Editor). A harness written against panes that do not
//! exist has exactly two dishonest options — assert nothing, or assert against
//! a stand-in the harness itself wrote — and the second is worse, because a
//! green scenario against a stand-in is indistinguishable in a summary from a
//! green scenario against the product.
//!
//! So a snapshot field is either `Present` or `Unavailable { unit }`, the host
//! decides which, and an expectation that reads an `Unavailable` field returns
//! [`Verdict::Blocked`] naming the unit that owes it. Nothing is faked, nothing
//! is skipped silently, and the day W13's editor advertises `caret` the caret
//! assertions in Scenario A start running against the real editor with no edit
//! to any scenario script. The ledger printed by the runner is the honest
//! answer to "is Scenario A real yet?".

use std::fmt;

use serde::{Deserialize, Serialize};

/// A snapshot field, or the unit that has not written it yet.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field<T> {
    /// The host produced a real value.
    Present(T),
    /// The host cannot produce this yet, and says who owes it.
    Unavailable {
        /// Work unit id, e.g. `"W16"`. Printed in the blocked ledger.
        unit: String,
        /// Human reason, e.g. `"no module registered a `viewer` pane"`.
        why: String,
        /// **The repo-relative path whose ABSENCE is the claim.**
        ///
        /// Not optional, and not `#[serde(default)]`: a host that omits it fails
        /// to deserialise, which is the point. Through wave 1 this variant
        /// carried prose only, the prose went stale — the bridge said W13 owed
        /// the document because "no editor is mounted" a whole wave after W13
        /// shipped a mountable editor — and nothing could fail, because a
        /// sentence has no truth value a test can read. A path does.
        ///
        /// `the_blocked_ledger_has_not_expired` (`apps/desktop/src/e2e/
        /// serve.test.ts`) and `no_blocked_field_has_an_expired_witness` (the
        /// live half of `tests/e2e/harness.rs`) both assert the same thing about
        /// it: the file named here must not exist in the tree.
        witness: String,
    },
}

impl<T> Field<T> {
    /// The value, or the blocked verdict naming its owner.
    ///
    /// # Errors
    /// Returns [`Verdict::Blocked`] when the field is [`Field::Unavailable`].
    pub fn get(&self, what: &str) -> Result<&T, Verdict> {
        match self {
            Self::Present(v) => Ok(v),
            Self::Unavailable { unit, why, witness } => Err(Verdict::Blocked {
                unit: unit.clone(),
                // The witness is printed, not hidden: a blocked step that names
                // the missing file is a work item; one that names only a unit is
                // a complaint.
                why: format!("{what}: {why} (awaiting {witness})"),
            }),
        }
    }

    /// Whether the host actually produced this field.
    #[must_use]
    pub const fn is_present(&self) -> bool {
        matches!(self, Self::Present(_))
    }
}

/// The status glyph a gutter draws — CONTRACTS §3's `BlockStatus` discriminant,
/// spelled exactly as `STATUS_RANK` in `apps/desktop/src/ipc/hand.ts` spells it.
///
/// Deliberately NOT `stratum_proto::status::BlockStatus`: an expectation names
/// the *displayed* glyph, which is `worseOf(local, kernel)` and therefore a
/// frontend verdict, not the kernel's opinion. Comparing against the wire type
/// would silently assert the wrong one of the two.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Glyph {
    /// Never executed in this session.
    NeverRun,
    /// The text does not parse; nothing can run.
    Broken,
    /// Ran and returned a non-zero `_rc`.
    Failed,
    /// Cancelled by the user or by the cancel ladder.
    Interrupted,
    /// Output on screen was produced by code that has since changed.
    Stale,
    /// Current, but the engine cannot prove upstream state is unchanged.
    CurrentUnverifiable,
    /// Output on screen matches the code and the state it was produced from.
    Current,
    /// In a run plan, not started.
    Queued,
    /// Executing now.
    Running,
}

impl fmt::Display for Glyph {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::NeverRun => "never_run",
            Self::Broken => "broken",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Stale => "stale",
            Self::CurrentUnverifiable => "current_unverifiable",
            Self::Current => "current",
            Self::Queued => "queued",
            Self::Running => "running",
        };
        f.write_str(s)
    }
}

/// The editor's view of the open document.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct DocView {
    /// Path as the app shows it, or `None` for an unsaved buffer.
    pub path: Option<String>,
    /// `doc.toString()` — the whole buffer, so a scenario can assert the source
    /// was not corrupted (Spec §38-A's last clause) by comparing to the fixture.
    pub text: String,
    /// Caret offset in UTF-16 code units, which is what CodeMirror counts in.
    pub caret: u32,
    /// The document version the frontend believes it holds.
    pub version: u32,
}

/// One gutter row: a block and the glyph drawn beside it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct GutterRow {
    /// Zero-based block index within the document, as the block map orders them.
    pub block: u32,
    /// What the gutter draws.
    pub glyph: Glyph,
}

/// One result card, as the reader sees it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Card {
    /// The block the card hangs under, if it is attached to one.
    pub block: Option<u32>,
    /// `ResultId`, so "a second run added a card" is distinguishable from "the
    /// same card re-rendered".
    pub result: u64,
    /// The header line, e.g. `summarize price mpg`.
    pub header: String,
    /// The rendered body, one entry per visual line.
    pub body: Vec<String>,
    /// Stata's `_rc` for the command that produced it.
    pub rc: u32,
}

/// One result envelope as the frontend's result store holds it.
///
/// Deliberately distinct from [`Card`]. A card is what the reader sees and is
/// W14's; this is `apps/desktop/src/state/results.ts`, which is W12's and exists
/// today. Keeping them apart is what lets Scenario A assert something true right
/// now — StataMP's own numbers arriving over the real event shape and landing
/// against the right block identity — while the assertions about the *rendered*
/// card stay honestly blocked on W14.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ResultView {
    /// `ResultId`.
    pub result: u64,
    /// The block index it was attached to, if any.
    pub block: Option<u32>,
    /// `clientKey(hash, ordinal)` — the identity the store filed it under.
    pub client_key: String,
    /// The command line that produced it.
    pub cmdline: String,
    /// Stata's `_rc`.
    pub rc: u32,
    /// The head of the raw classic-text output, one entry per line.
    pub raw_head: Vec<String>,
    /// Payload discriminants, e.g. `["summarize"]`.
    pub payloads: Vec<String>,
}

/// One dock pane.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PaneView {
    /// `PaneComponentId`, e.g. `"results"`, `"history"`, `"dataeditor"`.
    pub id: String,
    /// Present in the layout AND not collapsed. This much is the dock's, which
    /// is W12's and exists today.
    pub visible: bool,
    /// Text content, one entry per visual line. Separate from `visible` because
    /// the two are owned by different units: a pane can be genuinely visible
    /// long before W16 has written anything to put in it.
    pub content: Field<Vec<String>>,
}

/// Layout and the view modes a scenario switches.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct LayoutView {
    /// The layout id, e.g. `"classic"`.
    pub id: String,
    /// `InlineResultsMode`: `always` | `editor-run` | `compact` | `off`.
    pub inline_results: String,
}

/// One Review-pane row.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct HistoryRow {
    /// The command text as it was submitted.
    pub command: String,
    /// Stata's `_rc`. The pane colours a non-zero row red.
    pub rc: i32,
}

/// A block as the frontend currently understands it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BlockView {
    /// Zero-based index in the block map.
    pub index: u32,
    /// Byte span into the document text.
    pub span: (u32, u32),
    /// Displayed status — `worseOf(local, kernel)`.
    pub status: Glyph,
    /// `CodeHash`, 32 lowercase hex.
    pub hash: String,
}

/// Everything a scenario may look at.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    /// Which host produced it. Printed with every failure, because "the pre-host
    /// bridge said so" and "the packaged app said so" are very different claims.
    pub host: String,
    /// The open document.
    pub doc: Field<DocView>,
    /// Gutter glyphs, in block order.
    pub gutter: Field<Vec<GutterRow>>,
    /// The result store, oldest first (W12).
    pub results: Field<Vec<ResultView>>,
    /// Rendered result cards, oldest first (W14).
    pub cards: Field<Vec<Card>>,
    /// Dock panes.
    pub panes: Field<Vec<PaneView>>,
    /// The focused pane's id.
    pub focus: Field<String>,
    /// Layout and view modes.
    pub layout: Field<LayoutView>,
    /// The Review history, oldest first.
    pub history: Field<Vec<HistoryRow>>,
    /// The block map as the frontend holds it.
    pub blocks: Field<Vec<BlockView>>,
}

impl Snapshot {
    /// A snapshot in which nothing is available — the shape a host that
    /// implements none of this would return. Used by the harness self-test.
    #[must_use]
    pub fn all_unavailable(host: &str, unit: &str, why: &str) -> Self {
        // A generic fn rather than a closure: a closure is monomorphised once,
        // and every field here has a different `T`.
        fn f<T>(unit: &str, why: &str) -> Field<T> {
            Field::Unavailable {
                unit: unit.to_owned(),
                why: why.to_owned(),
                // A path that cannot exist: this constructor builds the
                // self-test's imaginary host, so there is no real module whose
                // absence it is waiting on.
                witness: "(no witness: Snapshot::all_unavailable)".to_owned(),
            }
        }
        Self {
            host: host.to_owned(),
            doc: f(unit, why),
            gutter: f(unit, why),
            results: f(unit, why),
            cards: f(unit, why),
            panes: f(unit, why),
            focus: f(unit, why),
            layout: f(unit, why),
            history: f(unit, why),
            blocks: f(unit, why),
        }
    }
}

/// Which sections of the snapshot to build.
///
/// Not decoration: `doc.toString()` on a 2 MB buffer is the expensive field and
/// most steps do not read it. A step asks for what it asserts on, which is what
/// keeps a snapshot O(what is asserted) rather than O(document).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    /// [`Snapshot::doc`].
    Doc,
    /// [`Snapshot::gutter`].
    Gutter,
    /// [`Snapshot::results`].
    Results,
    /// [`Snapshot::cards`].
    Cards,
    /// [`Snapshot::panes`].
    Panes,
    /// [`Snapshot::focus`].
    Focus,
    /// [`Snapshot::layout`].
    Layout,
    /// [`Snapshot::history`].
    History,
    /// [`Snapshot::blocks`].
    Blocks,
}

/// The set of sections one `e2e_snapshot` call asks for.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct What(pub Vec<Section>);

impl What {
    /// Every section. What the failure path asks for, so a report is complete.
    #[must_use]
    pub fn all() -> Self {
        Self(vec![
            Section::Doc,
            Section::Gutter,
            Section::Results,
            Section::Cards,
            Section::Panes,
            Section::Focus,
            Section::Layout,
            Section::History,
            Section::Blocks,
        ])
    }
}

/// The verdict on one expectation.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// The expectation held.
    Passed,
    /// The expectation did not hold, and here is the observed value.
    Failed {
        /// What was asserted.
        expected: String,
        /// What the host actually reported.
        actual: String,
    },
    /// The field this expectation reads is not implemented yet.
    Blocked {
        /// The work unit that owes the field.
        unit: String,
        /// Which field, and the host's reason.
        why: String,
    },
}

/// One declarative assertion over a [`Snapshot`].
///
/// Declarative rather than a closure so that the SAME value is evaluated
/// identically under both tiers, is printable in a report, and can be checked
/// for "does this scenario assert anything at all" — a scenario whose every
/// step is `Blocked` is a scenario that proves nothing, and the runner says so.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Expect {
    /// `doc.toString()` is exactly this. Spec §38-A: "no source-code corruption".
    DocEquals(String),
    /// The caret sits at this UTF-16 offset.
    CaretAt(u32),
    /// The caret sits inside the span of this block index.
    CaretInBlock(u32),
    /// This block's gutter glyph.
    GutterIs(u32, Glyph),
    /// This block's displayed status.
    BlockStatusIs(u32, Glyph),
    /// The document has exactly this many cards attached to this block.
    CardsForBlock(u32, usize),
    /// The newest card on this block has a header equal to this.
    CardHeaderIs(u32, String),
    /// The newest card on this block has a body line containing this needle.
    CardBodyContains(u32, String),
    /// Cards, in order, hang under these blocks. Spec §38-A's "underneath".
    CardOrderIs(Vec<u32>),
    /// The store holds exactly this many results for this block.
    ResultsForBlock(u32, usize),
    /// The newest result on this block has a raw output line containing this.
    ResultRawContains(u32, String),
    /// The store's results, in arrival order, belong to these blocks.
    ResultOrderIs(Vec<u32>),
    /// The newest result on this block carries this payload discriminant.
    ResultPayloadIs(u32, String),
    /// The layout in force.
    LayoutIs(String),
    /// The inline-results mode in force.
    InlineResultsIs(String),
    /// This pane is present and not collapsed.
    PaneVisible(String),
    /// This pane is absent or collapsed.
    PaneHidden(String),
    /// A visible pane's content contains this line.
    PaneContains(String, String),
    /// The focused pane.
    FocusIs(String),
    /// The Review history ends with these commands, oldest first.
    HistoryTailIs(Vec<String>),
}

impl Expect {
    /// Evaluate against a snapshot.
    #[must_use]
    #[allow(clippy::too_many_lines)] // one arm per assertion; splitting it hides the table
    pub fn check(&self, snap: &Snapshot) -> Verdict {
        match self {
            Self::DocEquals(want) => match snap.doc.get("doc") {
                Err(v) => v,
                Ok(doc) => eq(want, &doc.text, "doc.toString()"),
            },
            Self::CaretAt(want) => match snap.doc.get("doc.caret") {
                Err(v) => v,
                Ok(doc) => eq(&want.to_string(), &doc.caret.to_string(), "caret offset"),
            },
            Self::CaretInBlock(index) => {
                match (snap.doc.get("doc.caret"), snap.blocks.get("blocks")) {
                    (Err(v), _) | (_, Err(v)) => v,
                    (Ok(doc), Ok(blocks)) => match blocks.iter().find(|b| b.index == *index) {
                        None => failed(
                            &format!("caret inside block {index}"),
                            &format!("no block {index} in a map of {}", blocks.len()),
                        ),
                        Some(b) => {
                            if doc.caret >= b.span.0 && doc.caret <= b.span.1 {
                                Verdict::Passed
                            } else {
                                failed(
                                    &format!(
                                        "caret inside block {index} ({}..{})",
                                        b.span.0, b.span.1
                                    ),
                                    &format!("caret at {}", doc.caret),
                                )
                            }
                        }
                    },
                }
            }
            Self::GutterIs(index, glyph) => match snap.gutter.get("gutter") {
                Err(v) => v,
                Ok(rows) => match rows.iter().find(|r| r.block == *index) {
                    None => failed(
                        &format!("gutter glyph {glyph} on block {index}"),
                        &format!("no gutter row for block {index}"),
                    ),
                    Some(row) => eq(
                        &glyph.to_string(),
                        &row.glyph.to_string(),
                        &format!("gutter glyph on block {index}"),
                    ),
                },
            },
            Self::BlockStatusIs(index, glyph) => match snap.blocks.get("blocks") {
                Err(v) => v,
                Ok(blocks) => match blocks.iter().find(|b| b.index == *index) {
                    None => failed(&format!("block {index} status {glyph}"), "no such block"),
                    Some(b) => eq(
                        &glyph.to_string(),
                        &b.status.to_string(),
                        &format!("status of block {index}"),
                    ),
                },
            },
            Self::CardsForBlock(index, n) => match snap.cards.get("cards") {
                Err(v) => v,
                Ok(cards) => {
                    let have = cards.iter().filter(|c| c.block == Some(*index)).count();
                    eq(
                        &n.to_string(),
                        &have.to_string(),
                        &format!("cards attached to block {index}"),
                    )
                }
            },
            Self::CardHeaderIs(index, want) => match newest_card(snap, *index) {
                Err(v) => v,
                Ok(card) => eq(
                    want,
                    &card.header,
                    &format!("header of block {index}'s card"),
                ),
            },
            Self::CardBodyContains(index, needle) => match newest_card(snap, *index) {
                Err(v) => v,
                Ok(card) => {
                    if card.body.iter().any(|line| line.contains(needle)) {
                        Verdict::Passed
                    } else {
                        // Print the lines, not just how many there are. The
                        // acceptance bullet asks a failing scenario to report
                        // what it saw rather than that it did not see something,
                        // and "8 body lines, none matching" cost this unit a
                        // round trip through a debugger to learn that the card
                        // says "R\u{b2}" where the classic log says "R-squared".
                        failed(
                            &format!("a body line containing {needle:?}"),
                            &render_body(&card.body),
                        )
                    }
                }
            },
            Self::CardOrderIs(want) => match snap.cards.get("cards") {
                Err(v) => v,
                Ok(cards) => {
                    let have: Vec<u32> = cards.iter().filter_map(|c| c.block).collect();
                    eq(
                        &format!("{want:?}"),
                        &format!("{have:?}"),
                        "the blocks cards hang under, in order",
                    )
                }
            },
            Self::ResultsForBlock(index, n) => match snap.results.get("results") {
                Err(v) => v,
                Ok(rs) => {
                    let have = rs.iter().filter(|r| r.block == Some(*index)).count();
                    eq(
                        &n.to_string(),
                        &have.to_string(),
                        &format!("results filed against block {index}"),
                    )
                }
            },
            Self::ResultRawContains(index, needle) => match newest_result(snap, *index) {
                Err(v) => v,
                Ok(r) => {
                    if r.raw_head.iter().any(|line| line.contains(needle)) {
                        Verdict::Passed
                    } else {
                        failed(
                            &format!("a raw output line containing {needle:?}"),
                            &format!("{:?}", r.raw_head),
                        )
                    }
                }
            },
            Self::ResultOrderIs(want) => match snap.results.get("results") {
                Err(v) => v,
                Ok(rs) => {
                    let have: Vec<u32> = rs.iter().filter_map(|r| r.block).collect();
                    eq(
                        &format!("{want:?}"),
                        &format!("{have:?}"),
                        "the blocks results were filed against, in order",
                    )
                }
            },
            Self::ResultPayloadIs(index, want) => match newest_result(snap, *index) {
                Err(v) => v,
                Ok(r) => {
                    if r.payloads.iter().any(|p| p == want) {
                        Verdict::Passed
                    } else {
                        failed(
                            &format!("a {want:?} payload on block {index}"),
                            &format!("{:?}", r.payloads),
                        )
                    }
                }
            },
            Self::LayoutIs(want) => match snap.layout.get("layout") {
                Err(v) => v,
                Ok(l) => eq(want, &l.id, "layout id"),
            },
            Self::InlineResultsIs(want) => match snap.layout.get("layout") {
                Err(v) => v,
                Ok(l) => eq(want, &l.inline_results, "inline results mode"),
            },
            Self::PaneVisible(id) => pane_visibility(snap, id, true),
            Self::PaneHidden(id) => pane_visibility(snap, id, false),
            Self::PaneContains(id, needle) => match snap.panes.get("panes") {
                Err(v) => v,
                Ok(panes) => match panes.iter().find(|p| &p.id == id) {
                    None => failed(&format!("pane {id} present"), "no such pane"),
                    Some(p) => match p.content.get("pane content") {
                        Err(v) => v,
                        Ok(lines) => {
                            if lines.iter().any(|line| line.contains(needle)) {
                                Verdict::Passed
                            } else {
                                failed(
                                    &format!("pane {id} containing {needle:?}"),
                                    &format!("{} lines, none matching", lines.len()),
                                )
                            }
                        }
                    },
                },
            },
            Self::FocusIs(want) => match snap.focus.get("focus") {
                Err(v) => v,
                Ok(f) => eq(want, f, "focused pane"),
            },
            Self::HistoryTailIs(want) => match snap.history.get("history") {
                Err(v) => v,
                Ok(rows) => {
                    let tail: Vec<String> = rows
                        .iter()
                        .rev()
                        .take(want.len())
                        .rev()
                        .map(|r| r.command.clone())
                        .collect();
                    eq(
                        &format!("{want:?}"),
                        &format!("{tail:?}"),
                        "the tail of the Review history",
                    )
                }
            },
        }
    }
}

fn newest_card(snap: &Snapshot, index: u32) -> Result<&Card, Verdict> {
    let cards = snap.cards.get("cards")?;
    cards
        .iter()
        .rfind(|c| c.block == Some(index))
        .ok_or_else(|| {
            failed(
                &format!("a card on block {index}"),
                &format!("{} cards, none on that block", cards.len()),
            )
        })
}

fn newest_result(snap: &Snapshot, index: u32) -> Result<&ResultView, Verdict> {
    let results = snap.results.get("results")?;
    results
        .iter()
        .rfind(|r| r.block == Some(index))
        .ok_or_else(|| {
            failed(
                &format!("a result on block {index}"),
                &format!("{} results, none on that block", results.len()),
            )
        })
}

fn pane_visibility(snap: &Snapshot, id: &str, want_visible: bool) -> Verdict {
    match snap.panes.get("panes") {
        Err(v) => v,
        Ok(panes) => {
            let visible = panes.iter().any(|p| p.id == id && p.visible);
            if visible == want_visible {
                Verdict::Passed
            } else {
                failed(
                    &format!(
                        "pane {id} {}",
                        if want_visible { "visible" } else { "hidden" }
                    ),
                    &format!(
                        "pane {id} is {}",
                        if visible { "visible" } else { "hidden" }
                    ),
                )
            }
        }
    }
}

fn eq(want: &str, have: &str, what: &str) -> Verdict {
    if want == have {
        Verdict::Passed
    } else {
        failed(&format!("{what} == {want:?}"), &format!("{have:?}"))
    }
}

/// The lines a card actually drew, bounded.
///
/// Bounded because a `log` card's body is the whole classic-text scrollback and
/// a CI log that has to be scrolled past is a CI log people stop reading. The
/// cap is on the *report*, never on the assertion: the needle is looked for in
/// every line.
fn render_body(body: &[String]) -> String {
    const SHOWN: usize = 12;
    let mut out = format!("{} body lines", body.len());
    for line in body.iter().take(SHOWN) {
        out.push_str("\n           | ");
        out.push_str(line);
    }
    if body.len() > SHOWN {
        out.push_str(&format!("\n           | … {} more", body.len() - SHOWN));
    }
    out
}

fn failed(expected: &str, actual: &str) -> Verdict {
    Verdict::Failed {
        expected: expected.to_owned(),
        actual: actual.to_owned(),
    }
}
