//! `RunIntent` → `RunPlan` — design 03 §7, CONTRACTS §6.
//!
//! Twelve verbs, each with a defensible answer to one question: *what else runs
//! when I press this?* The two easy answers are both wrong.
//!
//! * **Run only this block** leaves visibly stale output below it, which is
//!   precisely the Jupyter failure spec §35 exists to solve.
//! * **Run everything below** re-runs the expensive unrelated models that
//!   Example 1 shows are unaffected; in a file with a 90-second `bootstrap` at
//!   the bottom that turns a 200 ms edit-run loop into a coffee break, and users
//!   respond by not using the feature.
//!
//! So *Run from here* runs the forward **may-intersect closure** and reports
//! what it skipped, because silence there would feel like a bug. And nothing
//! ever auto-runs an upstream block: spec §13 says do NOT rerun them, so the
//! plan carries `stale_upstream` for a non-blocking banner instead. Respecting
//! the user's literal request is the Stata contract; guessing is the Jupyter
//! failure mode inverted.

use std::sync::Arc;

use stratum_parse::{segment, Segmentation};
use stratum_proto::{
    BlockId, BlockStatus, CodeHash, DocumentId, ForwardScope, PlanItem, PlanReason, RegionKind,
    RunId, RunIntent, RunPlan, SectionId, SkipReason, Span, Unterminated,
};

use crate::staleness::{
    may_intersect, pending_writes_opt, reads_effective, AnalysedDoc, StatusMap, SweepInput,
};

/// Why an intent could not become a plan.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum PlanError {
    /// The selection is not one or more whole statements. Executing a fragment
    /// is how "why did my loop run once" happens in line-based tools, so we
    /// refuse with a diagnostic instead.
    #[error("selection does not parse as whole statements")]
    PartialStatement {
        /// Where the incomplete construct starts.
        span: Span,
        /// What the scanner was still waiting for.
        expected: Unterminated,
    },
    /// The cursor or block resolves to nothing executable.
    #[error("nothing to run")]
    NothingToRun,
    /// The named block is not in this document any more.
    #[error("block {0} is not in this document")]
    UnknownBlock(BlockId),
    /// `ProjectEntryPoint` was submitted but the workspace has no entry point,
    /// or it is not the open document.
    #[error("the project has no resolvable entry point")]
    NoEntryPoint,
    /// The intent names a different document than the one in the context.
    #[error("intent targets document {0}, context holds another")]
    WrongDocument(DocumentId),
}

/// Everything the planner reads.
pub struct PlanCtx<'a> {
    /// The reconciled, analysed document.
    pub doc: &'a AnalysedDoc,
    /// Statuses as of now — the planner never recomputes them, so the plan and
    /// the gutter cannot disagree.
    pub status: &'a StatusMap,
    /// The document's current text. Item sources are snapshotted from it.
    pub text: &'a str,
    /// The run id to stamp.
    pub run: RunId,
    /// The workspace's configured entry point, for `ProjectEntryPoint` (A23).
    pub project_entry: Option<DocumentId>,
    /// Everything the C7 test needs to answer "would this reach that". Optional
    /// because `EverythingAbove`, `ToCursor`, `WholeFile` and `CleanRun` do not
    /// consult it.
    pub sweep: Option<&'a SweepInput<'a>>,
}

impl PlanCtx<'_> {
    fn check_doc(&self, doc: DocumentId) -> Result<(), PlanError> {
        (doc == self.doc.map.doc)
            .then_some(())
            .ok_or(PlanError::WrongDocument(doc))
    }
}

/// Resolve an intent against a document.
///
/// # Errors
/// See [`PlanError`]. Every error here is a refusal the UI can explain, never a
/// silent no-op.
pub fn resolve(intent: &RunIntent, ctx: &PlanCtx<'_>) -> Result<RunPlan, PlanError> {
    match intent {
        RunIntent::CurrentBlock { doc, cursor } | RunIntent::RunAndAdvance { doc, cursor } => {
            ctx.check_doc(*doc)?;
            let idx = block_at_cursor(ctx.doc, *cursor).ok_or(PlanError::NothingToRun)?;
            Ok(single(ctx, idx))
        }
        RunIntent::Selection { doc, span } => {
            ctx.check_doc(*doc)?;
            selection(ctx, *span)
        }
        RunIntent::FromHere { doc, block, scope } => {
            ctx.check_doc(*doc)?;
            let start = ctx
                .doc
                .index_of(*block)
                .ok_or(PlanError::UnknownBlock(*block))?;
            Ok(from_here(ctx, start, *scope))
        }
        RunIntent::EverythingAbove { doc, block } => {
            ctx.check_doc(*doc)?;
            let target = ctx
                .doc
                .index_of(*block)
                .ok_or(PlanError::UnknownBlock(*block))?;
            // ALL of them regardless of status, and deliberately: this is the
            // "prime the session so I can work here" command, and skipping
            // Current blocks would not do that job when the session is empty.
            Ok(prefix(ctx, target, PlanReason::Prefix))
        }
        RunIntent::ToCursor { doc, cursor } => {
            ctx.check_doc(*doc)?;
            // Inclusive of the block containing the cursor; if the cursor sits
            // in trivia, the last block strictly above it.
            let target = block_at_or_above(ctx.doc, *cursor).ok_or(PlanError::NothingToRun)?;
            Ok(prefix(ctx, target + 1, PlanReason::Prefix))
        }
        RunIntent::CurrentSection { doc, cursor } => {
            ctx.check_doc(*doc)?;
            section(ctx, *cursor)
        }
        RunIntent::AllStale { doc } => {
            ctx.check_doc(*doc)?;
            Ok(all_stale(ctx))
        }
        RunIntent::WholeFile { doc } => {
            ctx.check_doc(*doc)?;
            Ok(whole(ctx, false))
        }
        RunIntent::CleanRun { entry, .. } => {
            ctx.check_doc(*entry)?;
            Ok(whole(ctx, true))
        }
        RunIntent::ProjectEntryPoint { .. } => {
            let entry = ctx.project_entry.ok_or(PlanError::NoEntryPoint)?;
            ctx.check_doc(entry).map_err(|_| PlanError::NoEntryPoint)?;
            Ok(whole(ctx, true))
        }
        RunIntent::CommandBar { text } => Ok(ephemeral(ctx, text, Span { start: 0, end: 0 })),
    }
}

/// Where `Run and advance` puts the cursor: the start of the next non-trivia
/// region, resolved **at enqueue time, not at completion**.
///
/// Otherwise the north-star flow in spec §36 — mashing Shift+Enter through a
/// file — stalls behind a slow `regress`, which is the difference between a
/// tool that feels like Stata's do-file editor and one that does not.
/// `None` means the cursor is in the last block: the caller appends a newline
/// at EOF and puts the cursor there. It never inserts a `// %%` marker.
#[must_use]
pub fn resolve_advance(doc: &AnalysedDoc, cursor: u32) -> Option<u32> {
    let idx = block_at_cursor(doc, cursor)?;
    (idx + 1..doc.len())
        .find(|i| doc.is_executable(*i))
        .map(|i| doc.map.regions[i].span.start)
}

// ---------------------------------------------------------------------------
// Intent bodies
// ---------------------------------------------------------------------------

fn single(ctx: &PlanCtx<'_>, idx: usize) -> RunPlan {
    let mut plan = base(ctx);
    plan.items.push(item(ctx, idx, PlanReason::Requested));
    plan.stale_upstream = stale_upstream(ctx, idx);
    plan
}

fn selection(ctx: &PlanCtx<'_>, span: Span) -> Result<RunPlan, PlanError> {
    let text = slice(ctx.text, span);
    let seg = segment(text);
    if let Some(bad) = seg.regions.iter().find_map(|r| match r.kind {
        stratum_parse::RegionShape::Unterminated { expected } => Some((r.span, expected)),
        _ => None,
    }) {
        return Err(PlanError::PartialStatement {
            // Report in DOCUMENT coordinates: a span relative to the selection
            // would land the caret in the wrong place in the editor.
            span: Span {
                start: span.start + bad.0.start,
                end: span.start + bad.0.end,
            },
            expected: bad.1,
        });
    }
    if seg
        .regions
        .iter()
        .all(|r| matches!(r.kind, stratum_parse::RegionShape::Trivia { .. }))
    {
        return Err(PlanError::NothingToRun);
    }
    Ok(ephemeral_with(ctx, text, span, &seg))
}

fn from_here(ctx: &PlanCtx<'_>, start: usize, scope: ForwardScope) -> RunPlan {
    let mut plan = base(ctx);
    let selected = match scope {
        // The blunt instrument, available in the command palette and bindable.
        ForwardScope::AllBelow => (start..ctx.doc.len())
            .filter(|i| ctx.doc.is_executable(*i))
            .collect::<Vec<_>>(),
        ForwardScope::Dependents => forward_closure(ctx, start),
    };
    for (n, idx) in selected.iter().enumerate() {
        plan.items.push(item(
            ctx,
            *idx,
            if n == 0 {
                PlanReason::Requested
            } else {
                PlanReason::DependencyOf
            },
        ));
    }
    // Blocks NOT selected are reported, with the reason. Their records are
    // untouched and their status stays Current, which is provable: they were
    // out of the closure precisely because nothing they read could have changed.
    for idx in start + 1..ctx.doc.len() {
        if ctx.doc.is_executable(idx) && !selected.contains(&idx) {
            plan.skipped
                .push((ctx.doc.block(idx), SkipReason::Unaffected));
        }
    }
    plan.stale_upstream = stale_upstream(ctx, start);
    plan
}

/// The forward may-intersect closure of `03` §7.
///
/// Every later block that could observe a difference caused by re-running this
/// one, computed transitively over all dependency namespaces, with any
/// statically-unknown read set forcing inclusion. Never guess "no".
fn forward_closure(ctx: &PlanCtx<'_>, start: usize) -> Vec<usize> {
    let mut sel = vec![start];
    let Some(sweep) = ctx.sweep else {
        // No state to reason over: include everything below rather than
        // silently narrowing on missing information.
        return (start..ctx.doc.len())
            .filter(|i| ctx.doc.is_executable(*i))
            .collect();
    };
    let mut acc: Vec<usize> = vec![start];
    for idx in start + 1..ctx.doc.len() {
        if !ctx.doc.is_executable(idx) {
            continue;
        }
        let reads = reads_effective(sweep, idx);
        let include = acc.iter().any(|up| {
            let w = pending_writes_opt(sweep, *up, ctx.status.at(*up));
            may_intersect(&w, &reads).is_some()
        });
        if include {
            sel.push(idx);
            acc.push(idx);
        }
    }
    sel
}

fn prefix(ctx: &PlanCtx<'_>, end: usize, reason: PlanReason) -> RunPlan {
    let mut plan = base(ctx);
    for idx in 0..end.min(ctx.doc.len()) {
        if ctx.doc.is_executable(idx) {
            plan.items.push(item(ctx, idx, reason));
        }
    }
    plan
}

fn whole(ctx: &PlanCtx<'_>, clean: bool) -> RunPlan {
    let mut plan = prefix(ctx, ctx.doc.len(), PlanReason::Requested);
    plan.epoch_reset = clean;
    plan.clean_state = clean;
    plan
}

fn section(ctx: &PlanCtx<'_>, cursor: u32) -> Result<RunPlan, PlanError> {
    let idx = block_at_cursor(ctx.doc, cursor).ok_or(PlanError::NothingToRun)?;
    let Some(sec) = ctx.doc.map.regions[idx].section else {
        // No boundaries at all: degrade to Run current block and let the status
        // bar say so. We do not invent section boundaries from blank lines — a
        // heuristic that is right 70 % of the time is worse than an honest
        // fallback.
        return Ok(single(ctx, idx));
    };
    let mut plan = base(ctx);
    let mut first = true;
    for i in 0..ctx.doc.len() {
        if ctx.doc.is_executable(i) && ctx.doc.map.regions[i].section == Some::<SectionId>(sec) {
            plan.items.push(item(
                ctx,
                i,
                if first {
                    PlanReason::Requested
                } else {
                    PlanReason::DependencyOf
                },
            ));
            first = false;
        }
    }
    if plan.items.is_empty() {
        return Err(PlanError::NothingToRun);
    }
    plan.stale_upstream = stale_upstream(ctx, idx);
    Ok(plan)
}

/// *Run all stale blocks* — the "catch up" command.
///
/// Selection is every `Stale` or `Failed` block, then **backward-closed**: for
/// each selected B, add any A above it that is itself not runnable-clean and
/// writes something B reads, to fixpoint. No forward closure is needed —
/// anything downstream that is affected is already `Stale` by the rule, and is
/// therefore already selected.
///
/// `NeverRun` blocks that nothing stale depends on are deliberately NOT run:
/// the half-written block at the bottom of the file is not something the user
/// asked for.
// The block ordinal indexes `selected`, `ctx.doc` and `ctx.status` in step —
// three parallel structures, not one — and the closure below is a triangular
// fixpoint over that domain. `enumerate()` over any single one of them would
// name the index anyway and hide which structure the ordinal really belongs to.
#[allow(clippy::needless_range_loop)]
fn all_stale(ctx: &PlanCtx<'_>) -> RunPlan {
    let mut plan = base(ctx);
    let n = ctx.doc.len();
    let mut selected = vec![false; n];
    for idx in 0..n {
        if !ctx.doc.is_executable(idx) {
            continue;
        }
        if matches!(
            ctx.status.at(idx),
            Some(BlockStatus::Stale { .. } | BlockStatus::Failed { .. })
        ) {
            selected[idx] = true;
        }
    }
    if let Some(sweep) = ctx.sweep {
        // Terminates: every pass adds only blocks of strictly smaller ordinal.
        let mut changed = true;
        while changed {
            changed = false;
            for b in 0..n {
                if !selected[b] {
                    continue;
                }
                let reads = reads_effective(sweep, b);
                for a in 0..b {
                    if selected[a] || !ctx.doc.is_executable(a) {
                        continue;
                    }
                    let eligible = matches!(
                        ctx.status.at(a),
                        Some(
                            BlockStatus::NeverRun
                                | BlockStatus::Stale { .. }
                                | BlockStatus::Failed { .. }
                        )
                    );
                    if eligible
                        && may_intersect(&pending_writes_opt(sweep, a, ctx.status.at(a)), &reads)
                            .is_some()
                    {
                        selected[a] = true;
                        changed = true;
                    }
                }
            }
        }
    }
    for idx in 0..n {
        if !ctx.doc.is_executable(idx) {
            continue;
        }
        if selected[idx] {
            plan.items.push(item(ctx, idx, PlanReason::Stale));
        } else {
            plan.skipped.push((
                ctx.doc.block(idx),
                match ctx.status.at(idx) {
                    Some(BlockStatus::Current { .. } | BlockStatus::CurrentUnverifiable { .. }) => {
                        SkipReason::AlreadyCurrent
                    }
                    _ => SkipReason::Unaffected,
                },
            ));
        }
    }
    plan
}

fn ephemeral(ctx: &PlanCtx<'_>, text: &str, span: Span) -> RunPlan {
    let seg = segment(text);
    ephemeral_with(ctx, text, span, &seg)
}

fn ephemeral_with(ctx: &PlanCtx<'_>, text: &str, span: Span, seg: &Segmentation<'_>) -> RunPlan {
    let mut plan = base(ctx);
    plan.items.push(PlanItem {
        // Never a node in the staleness graph — but its writes DO bump versions
        // and therefore DO make real blocks stale (§6.4 Example 3).
        block: BlockId::EPHEMERAL,
        span,
        code_hash: stratum_parse::code_hash(text, &seg.lines, &seg.derived),
        reason: PlanReason::Requested,
    });
    plan
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn base(ctx: &PlanCtx<'_>) -> RunPlan {
    RunPlan {
        run: ctx.run,
        items: Vec::new(),
        epoch_reset: false,
        clean_state: false,
        skipped: Vec::new(),
        stale_upstream: Vec::new(),
    }
}

fn item(ctx: &PlanCtx<'_>, idx: usize, reason: PlanReason) -> PlanItem {
    let region = &ctx.doc.map.regions[idx];
    PlanItem {
        block: ctx.doc.block(idx),
        span: region.span,
        // The snapshot's hash, which is what `stale_on_arrival` compares
        // against when the item finally runs.
        code_hash: region.code_hash,
        reason,
    }
}

/// The text each plan item runs, snapshotted now.
///
/// Kept beside the plan rather than inside it because [`PlanItem`] is a wire
/// type and shipping every block's source to every window on every submit is
/// bytes nobody reads. The queue holds these; `BlockStarted.text` carries the
/// one that is actually running.
#[must_use]
pub fn snapshots(intent: &RunIntent, ctx: &PlanCtx<'_>, plan: &RunPlan) -> Vec<Arc<str>> {
    plan.items
        .iter()
        .map(|it| {
            if it.block.is_real() {
                Arc::from(slice(ctx.text, it.span))
            } else {
                match intent {
                    RunIntent::CommandBar { text } => Arc::from(text.as_str()),
                    _ => Arc::from(slice(ctx.text, it.span)),
                }
            }
        })
        .collect()
}

/// Upstream blocks that are not settled and could reach `idx` — the input to
/// the non-blocking "3 upstream blocks are stale — [Run them first]" banner.
fn stale_upstream(ctx: &PlanCtx<'_>, idx: usize) -> Vec<BlockId> {
    let Some(sweep) = ctx.sweep else {
        return Vec::new();
    };
    let reads = reads_effective(sweep, idx);
    (0..idx)
        .filter(|a| {
            ctx.doc.is_executable(*a)
                && !matches!(
                    ctx.status.at(*a),
                    Some(BlockStatus::Current { .. } | BlockStatus::CurrentUnverifiable { .. })
                )
                && may_intersect(&pending_writes_opt(sweep, *a, ctx.status.at(*a)), &reads)
                    .is_some()
        })
        .map(|a| ctx.doc.block(a))
        .collect()
}

fn slice(text: &str, span: Span) -> &str {
    let start = (span.start as usize).min(text.len());
    let end = (span.end as usize).clamp(start, text.len());
    &text[start..end]
}

/// The innermost enclosing executable region at `cursor`.
///
/// `BlockKind::Loop` and `ProgramDefine` are atomic — you never run half a
/// `foreach`, because half of a `foreach` is not a Stata program and the
/// alternative is the source of most "why did my loop run once" confusion in
/// existing tools. Regions already have that granularity, so this is a lookup.
fn block_at_cursor(doc: &AnalysedDoc, cursor: u32) -> Option<usize> {
    let at = region_at(doc, cursor)?;
    if doc.is_executable(at) {
        return Some(at);
    }
    // The cursor is in a comment run or on a blank line. Prefer the next
    // executable region — a cursor parked above a command means that command —
    // and fall back to the previous one at end of file.
    (at + 1..doc.len())
        .find(|i| doc.is_executable(*i))
        .or_else(|| (0..at).rev().find(|i| doc.is_executable(*i)))
}

fn block_at_or_above(doc: &AnalysedDoc, cursor: u32) -> Option<usize> {
    let at = region_at(doc, cursor)?;
    if doc.is_executable(at) {
        return Some(at);
    }
    (0..at).rev().find(|i| doc.is_executable(*i))
}

fn region_at(doc: &AnalysedDoc, cursor: u32) -> Option<usize> {
    if doc.is_empty() {
        return None;
    }
    // `outer_span`s tile the file exactly (CONTRACTS §2), so a partition point
    // is the whole search.
    let i = doc
        .map
        .regions
        .partition_point(|r| r.outer_span.start <= cursor)
        .checked_sub(1)?;
    Some(i)
}

/// True when a region is a `Trivia` run — no run affordance, never a plan item.
#[must_use]
pub fn is_trivia(kind: &RegionKind) -> bool {
    matches!(kind, RegionKind::Trivia { .. })
}

/// Recompute a snapshot's hash to detect an edit that landed while it queued.
#[must_use]
pub fn hash_of(text: &str) -> CodeHash {
    let seg = segment(text);
    stratum_parse::code_hash(text, &seg.lines, &seg.derived)
}
