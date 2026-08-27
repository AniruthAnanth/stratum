//! The FIFO run queue — design 03 §9.1.
//!
//! Mashing Shift+Enter enqueues in order; queued blocks show `Queued` with a
//! position badge and can be cancelled individually. Queueing rather than
//! rejecting while busy is required by the north-star flow in spec §36 — a
//! second press that beeps at you is the same product failure as a modal
//! dialog.
//!
//! # The enqueue-time snapshot, and why it is not the current text
//!
//! [`QueuedItem::source`] and [`QueuedItem::code_hash`] are captured when the
//! plan is resolved. If the user edits the block while it waits, we still run
//! the snapshot and set `stale_on_arrival`, so C3 shows the block
//! `Stale(CodeChanged)` the instant it finishes. The alternative — running the
//! edited text — means the button you pressed does something you never saw.

use std::collections::VecDeque;
use std::sync::Arc;

use stratum_proto::{
    BlockId, CancelLevel, CodeHash, DocumentId, ExecOrigin, ExecutionId, InlineResultsMode,
    PlanReason, RunId, RunPlan, Span, UnixMs,
};

use crate::cancel::CancelToken;
use crate::staleness::RunState;

/// One block, snapshotted at enqueue time.
#[derive(Clone, Debug)]
pub struct QueuedItem {
    /// `BlockId::EPHEMERAL` for command-bar and selection runs.
    pub block: BlockId,
    /// Byte range in the document this was snapshotted from.
    pub span: Span,
    /// Hash of the snapshot, not of the block's current text.
    pub code_hash: CodeHash,
    /// The exact text to execute. `Arc` because it also lands in the
    /// `ExecutionRecord` and in the `BlockStarted` event.
    pub source: Arc<str>,
    /// Why the planner included it.
    pub reason: PlanReason,
}

/// One submitted [`RunPlan`], in flight.
#[derive(Debug)]
pub struct QueuedRun {
    /// Wire id of this submission.
    pub run: RunId,
    /// The document it came from, if any.
    pub doc: Option<DocumentId>,
    /// What the user pressed.
    pub origin: ExecOrigin,
    /// Inline-result policy for this run (spec §4).
    pub inline: InlineResultsMode,
    /// Build a fresh session before the first item (spec §15).
    pub epoch_reset: bool,
    /// This is a clean run; the result surface is visually distinct.
    pub clean_state: bool,
    /// Cancellation for every item in this run.
    pub token: CancelToken,
    /// Remaining items, in execution order.
    pub items: VecDeque<QueuedItem>,
    /// Set once `RunStarted` has been emitted, so it is emitted exactly once
    /// per run — including on error, interrupt and timeout (CONTRACTS §7).
    pub started: bool,
    /// When `RunStarted` was emitted.
    pub started_at_ms: UnixMs,
    /// Completed items, for `RunFinished.blocks_run`.
    pub blocks_run: u32,
    /// Failed items, for `RunFinished.blocks_failed`.
    pub blocks_failed: u32,
    /// Worst return code seen so far.
    pub rc: u32,
    /// Items the plan had when it was submitted.
    pub plan_len: u32,
}

impl QueuedRun {
    /// Build a queue entry from a resolved plan.
    #[must_use]
    pub fn new(
        plan: &RunPlan,
        doc: Option<DocumentId>,
        origin: ExecOrigin,
        inline: InlineResultsMode,
        sources: Vec<Arc<str>>,
    ) -> Self {
        assert_eq!(
            plan.items.len(),
            sources.len(),
            "one snapshot per plan item; the snapshot IS the thing that runs"
        );
        Self {
            run: plan.run,
            doc,
            origin,
            inline,
            epoch_reset: plan.epoch_reset,
            clean_state: plan.clean_state,
            token: CancelToken::new(),
            items: plan
                .items
                .iter()
                .zip(sources)
                .map(|(item, source)| QueuedItem {
                    block: item.block,
                    span: item.span,
                    code_hash: item.code_hash,
                    source,
                    reason: item.reason,
                })
                .collect(),
            started: false,
            started_at_ms: 0,
            blocks_run: 0,
            blocks_failed: 0,
            rc: 0,
            plan_len: u32::try_from(plan.items.len()).unwrap_or(u32::MAX),
        }
    }
}

/// What the worker just took off the queue.
#[derive(Debug)]
pub struct Dequeued {
    /// The run it belongs to.
    pub run: RunId,
    /// The document the run came from.
    pub doc: Option<DocumentId>,
    /// What the user pressed.
    pub origin: ExecOrigin,
    /// Inline-result policy for this run.
    pub inline: InlineResultsMode,
    /// Build a fresh session before this item (only ever set on the first).
    pub epoch_reset: bool,
    /// This is a clean run.
    pub clean_state: bool,
    /// How many items the plan had, for `RunStarted.plan_len`.
    pub plan_len: u32,
    /// The item to execute.
    pub item: QueuedItem,
    /// Cancellation for it.
    pub token: CancelToken,
    /// True for the first item of a run, which is when `RunStarted` is emitted
    /// and when a clean run builds its fresh session.
    pub first: bool,
}

/// The engine's run queue and the counters the acceptance gates assert.
#[derive(Debug, Default)]
pub struct RunQueue {
    runs: VecDeque<QueuedRun>,
    running: Option<(RunId, BlockId, ExecutionId, UnixMs)>,
    enqueued: u64,
    dequeued: u64,
    cancelled: u64,
}

impl RunQueue {
    /// An empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Enqueue a resolved run. Returns its cancel token so the caller can hand
    /// it to the supervisor before the worker has even looked at the queue.
    pub fn push(&mut self, run: QueuedRun) -> CancelToken {
        let token = run.token.clone();
        self.enqueued += run.items.len() as u64;
        self.runs.push_back(run);
        token
    }

    /// Take the next item, skipping runs whose items were all cancelled.
    pub fn pop(&mut self) -> Option<Dequeued> {
        while let Some(front) = self.runs.front_mut() {
            if front.token.is_cancelled() && !front.started {
                // Cancelled before it ever started: it never becomes a run at
                // all, so no RunStarted/RunFinished pair is owed.
                self.cancelled += front.items.len() as u64;
                self.runs.pop_front();
                continue;
            }
            let first = !front.started;
            let item = front.items.pop_front()?;
            self.dequeued += 1;
            front.started = true;
            return Some(Dequeued {
                run: front.run,
                doc: front.doc,
                origin: front.origin,
                inline: front.inline,
                epoch_reset: first && front.epoch_reset,
                clean_state: front.clean_state,
                plan_len: front.plan_len,
                item,
                token: front.token.clone(),
                first,
            });
        }
        None
    }

    /// The run at the head of the queue, which is the one being executed.
    pub fn front_mut(&mut self) -> Option<&mut QueuedRun> {
        self.runs.front_mut()
    }

    /// Is the head run out of items?
    #[must_use]
    pub fn front_is_drained(&self) -> bool {
        self.runs.front().is_some_and(|r| r.items.is_empty())
    }

    /// Remove the head run once its `RunFinished` has been emitted.
    pub fn pop_run(&mut self) -> Option<QueuedRun> {
        self.runs.pop_front()
    }

    /// Note that an item is now executing, for C0.
    pub fn mark_running(&mut self, run: RunId, block: BlockId, exec: ExecutionId, at_ms: UnixMs) {
        self.running = Some((run, block, exec, at_ms));
    }

    /// Note that the executing item finished.
    pub fn mark_idle(&mut self) {
        self.running = None;
    }

    /// Request cancellation of one run.
    pub fn cancel(&mut self, run: RunId, level: CancelLevel, now_ms: u64) -> bool {
        let Some(target) = self.runs.iter_mut().find(|r| r.run == run) else {
            return false;
        };
        target.token.request(level, now_ms);
        // Items that have not started are dropped immediately — a queued block
        // the user cancelled must not run two seconds later because the current
        // command happened to finish.
        if self.running.map(|(r, ..)| r) != Some(run) {
            self.cancelled += target.items.len() as u64;
            target.items.clear();
        }
        true
    }

    /// Request cancellation of everything.
    pub fn cancel_all(&mut self, level: CancelLevel, now_ms: u64) {
        let running = self.running.map(|(r, ..)| r);
        for r in &mut self.runs {
            r.token.request(level, now_ms);
            if Some(r.run) != running {
                self.cancelled += r.items.len() as u64;
                r.items.clear();
            }
        }
    }

    /// Queue state for clause C0 of the sweep.
    #[must_use]
    pub fn run_state(&self) -> RunState {
        RunState {
            running: self.running.map(|(_, block, exec, at)| (block, exec, at)),
            queued: self
                .runs
                .iter()
                .flat_map(|r| r.items.iter().map(|i| i.block))
                .filter(|b| b.is_real())
                .collect(),
        }
    }

    /// Items waiting.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.runs.iter().map(|r| r.items.len()).sum()
    }

    /// True when nothing is queued and nothing is running.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.runs.is_empty() && self.running.is_none()
    }

    /// `(enqueued, dequeued, cancelled)` — the counters the queue acceptance
    /// gates assert, per ADR-017.
    #[must_use]
    pub fn counters(&self) -> (u64, u64, u64) {
        (self.enqueued, self.dequeued, self.cancelled)
    }
}
