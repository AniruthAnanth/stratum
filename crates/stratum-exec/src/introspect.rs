//! The engine's `SessionIntrospect` implementation — A5, CONTRACTS §9.1/§13.
//!
//! # Why this is a snapshot and not a view of the live session
//!
//! [`stratum_proto::SessionIntrospect`] is `Send + Sync`. The session is `!Sync`
//! and is owned by the session worker, so a trait object over the live session
//! could not exist, and one that took a lock the worker holds across a command
//! would break C50 the moment anybody asked for a variable list during a
//! 30-second `regress`.
//!
//! So the worker builds an [`IntrospectSnapshot`] BETWEEN commands
//! ([`crate::SessionHost::introspect`]) and publishes it through
//! [`crate::Snapshot`]. Readers clone an `Arc` and answer from an immutable
//! value with no lock held. The cost is that answers are as of the last
//! completed command, which is exactly the semantics the UI wants: a variable
//! list that mutated halfway through a `merge` would be a worse answer, not a
//! fresher one.
//!
//! # Why there is no observation value anywhere in this file
//!
//! A5 declares the trait over proto types precisely so that the tier-1 privacy
//! guarantee of `07` §4 is structural. `DatasetMeta` is metadata,
//! [`QuickSummary`] is an aggregate, and neither this type nor the trait has a
//! shape that can carry a cell. A context packer therefore cannot leak
//! observation data through this seam, because the seam has no room for it.
//!
//! # `var_stats` is lazy, and that is a property of the producer
//!
//! Spec §20 explicitly refuses to compute a summary for every variable. The
//! snapshot is immutable, so there is nothing to cache INTO here: it carries the
//! summaries the worker was actually asked to compute, and answers `None` for
//! the rest. A caller that gets `None` asks the engine, which computes it on the
//! worker and publishes it with the next snapshot.

use stratum_proto::{
    AiContextSnapshot, AiContextWant, CompletionEnv, DatasetMeta, Diagnostic, EstimateHandle,
    FrameInfo, MacroInfo, QuickSummary, SessionId, SessionIntrospect, StoredResultsView,
    VariableInfo,
};

use crate::ledger::LedgerView;

/// How many command lines `AiContextWant::RECENT_COMMANDS` is worth.
///
/// Enough for the packer to see the shape of what the user is doing, small
/// enough that it never becomes the bulk of a prompt. The ledger is the source
/// (`03` §3), not a second buffer that could disagree with History.
pub const AI_CONTEXT_COMMANDS: usize = 32;

/// How many diagnostics `AiContextWant::RECENT_ERRORS` is worth.
pub const AI_CONTEXT_ERRORS: usize = 16;

/// One frame's metadata, in the order the session reports its frames.
///
/// `Vec` rather than a hash map on purpose: this order reaches the Variables
/// pane and an AI prompt, and `03` §9.3 keeps hash iteration order out of
/// anything a user or a golden can see.
#[derive(Clone, Debug)]
pub struct FrameView {
    /// Shape and identity of the frame.
    pub info: FrameInfo,
    /// Storage order, exactly as `describe` prints it.
    pub variables: Vec<VariableInfo>,
    /// Only the summaries somebody actually asked for; see the module header.
    pub summaries: Vec<QuickSummary>,
}

impl FrameView {
    /// An empty frame view named `name`.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            info: FrameInfo {
                name: name.into(),
                n_obs: 0,
                n_vars: 0,
                sorted_by: Vec::new(),
                changed: false,
                state: stratum_proto::DatasetStateId(0),
            },
            variables: Vec::new(),
            summaries: Vec::new(),
        }
    }
}

/// A metadata-only view of the session, as of the last completed command.
///
/// Published by the session worker and read by the control thread, the request
/// handler and `stratum-intel`. Immutable once published — every field is
/// rebuilt by the worker rather than mutated in place, which is what lets
/// readers work without a lock at all.
#[derive(Clone, Debug)]
pub struct IntrospectSnapshot {
    /// Whose session this is. `SessionId(0)` on the default snapshot, which is
    /// what an engine that has not run anything yet publishes.
    pub session: SessionId,
    /// Bumped by the worker on every publication. A consumer that caches this
    /// snapshot compares generations rather than the whole value.
    pub generation: u64,
    /// Every live frame, in the session's own order. The current frame is
    /// `dataset.frame`.
    pub frames: Vec<FrameView>,
    /// Shape of the CURRENT frame.
    pub dataset: DatasetMeta,
    /// Locals and globals in scope.
    pub macros: Vec<MacroInfo>,
    /// `r()`, `e()` and `s()`, insertion-ordered as `return list` prints them.
    pub stored: StoredResultsView,
    /// `estimates store` handles.
    pub estimates: Vec<EstimateHandle>,
    /// Newest last, so `recent_errors(n)` is a tail.
    pub errors: Vec<Diagnostic>,
    /// What the completion popup needs, already bounded by
    /// `CompletionEnv::enforce_bounds`.
    pub completion: CompletionEnv,
}

/// Hand-written for the reason `stratum_proto::DatasetMeta`'s is: the ids in
/// CONTRACTS §1 deliberately do not derive `Default`, because a silently
/// defaulted id is a real bug rather than a typo. `SessionId(0)` here is the
/// engine's own "no session opened yet", set once at spawn and replaced by the
/// first snapshot the worker publishes.
impl Default for IntrospectSnapshot {
    fn default() -> Self {
        Self {
            session: SessionId(0),
            generation: 0,
            frames: Vec::new(),
            dataset: DatasetMeta::default(),
            macros: Vec::new(),
            stored: StoredResultsView::default(),
            estimates: Vec::new(),
            errors: Vec::new(),
            completion: CompletionEnv::default(),
        }
    }
}

impl IntrospectSnapshot {
    /// An empty snapshot for `session`.
    #[must_use]
    pub fn for_session(session: SessionId) -> Self {
        Self {
            session,
            ..Self::default()
        }
    }

    /// The frame `name`, if it is live.
    #[must_use]
    pub fn frame(&self, name: &str) -> Option<&FrameView> {
        self.frames.iter().find(|f| f.info.name == name)
    }

    /// Serve `EngineRequest::AiContext` (A5).
    ///
    /// `want` is honoured strictly: a flag that is off yields the empty value,
    /// so the caller never receives — and therefore never has to remember to
    /// drop — data the privacy tier gate would have stripped on the way out
    /// (`07` §4). The gate filters again downstream; narrowing here is what
    /// keeps tier-3 data from being read into desktop memory at all.
    ///
    /// `recent_commands` comes from the ledger rather than from this snapshot,
    /// because the ledger is the one history the History pane also reads and a
    /// second copy could disagree with it.
    #[must_use]
    pub fn ai_context(&self, want: AiContextWant, ledger: &LedgerView<'_>) -> AiContextSnapshot {
        AiContextSnapshot {
            session: self.session,
            generation: self.generation,
            dataset: want
                .contains(AiContextWant::DATASET_META)
                .then(|| self.dataset.clone()),
            macros: if want.contains(AiContextWant::MACROS) {
                self.macros.clone()
            } else {
                Vec::new()
            },
            stored: want
                .contains(AiContextWant::STORED_RESULTS)
                .then(|| self.stored.clone()),
            estimates: if want.contains(AiContextWant::ESTIMATES) {
                self.estimates.clone()
            } else {
                Vec::new()
            },
            recent_errors: if want.contains(AiContextWant::RECENT_ERRORS) {
                self.recent_errors(AI_CONTEXT_ERRORS)
            } else {
                Vec::new()
            },
            recent_commands: if want.contains(AiContextWant::RECENT_COMMANDS) {
                ledger.recent_commands(AI_CONTEXT_COMMANDS)
            } else {
                Vec::new()
            },
            var_summaries: if want.contains(AiContextWant::VAR_SUMMARIES) {
                self.frame(&self.dataset.frame)
                    .map(|f| f.summaries.clone())
                    .unwrap_or_default()
            } else {
                Vec::new()
            },
        }
    }
}

impl SessionIntrospect for IntrospectSnapshot {
    fn frames(&self) -> Vec<FrameInfo> {
        self.frames.iter().map(|f| f.info.clone()).collect()
    }

    fn variables(&self, frame: &str) -> Vec<VariableInfo> {
        self.frame(frame)
            .map(|f| f.variables.clone())
            .unwrap_or_default()
    }

    fn var_stats(&self, frame: &str, v: &str) -> Option<QuickSummary> {
        // `None` means "not computed yet", never "no such variable" — the two
        // are distinguished by `variables()`, and conflating them would make a
        // caller render an empty summary card for a variable that exists.
        self.frame(frame)?
            .summaries
            .iter()
            .find(|s| s.var == v)
            .cloned()
    }

    fn macros(&self) -> Vec<MacroInfo> {
        self.macros.clone()
    }

    fn stored_results(&self) -> StoredResultsView {
        self.stored.clone()
    }

    fn estimates_store(&self) -> Vec<EstimateHandle> {
        self.estimates.clone()
    }

    fn recent_errors(&self, n: usize) -> Vec<Diagnostic> {
        let start = self.errors.len().saturating_sub(n);
        self.errors[start..].to_vec()
    }

    fn dataset_meta(&self) -> DatasetMeta {
        self.dataset.clone()
    }

    fn completion_env(&self) -> CompletionEnv {
        self.completion.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stratum_proto::{
        DatasetStateId, ExecStatus, ExecutionId, ExecutionRecord, MacroScope, Severity, StateId,
        Taint,
    };

    use crate::ledger::{Committed, ExecutionLedger};
    use crate::staleness::{RecordedReads, RecordedWrites};
    use std::sync::Arc;

    fn summary(var: &str) -> QuickSummary {
        QuickSummary {
            var: var.to_owned(),
            state: DatasetStateId(1),
            n: 74,
            n_missing: 0,
            mean: Some(6165.257),
            median: None,
            sd: None,
            min: None,
            max: None,
            display: Vec::new(),
            sparkline: None,
            deferred: false,
        }
    }

    fn snapshot() -> IntrospectSnapshot {
        let mut frame = FrameView::new("default");
        frame.summaries.push(summary("price"));
        IntrospectSnapshot {
            session: SessionId(3),
            generation: 7,
            dataset: DatasetMeta {
                frame: "default".to_owned(),
                n_obs: 74,
                ..DatasetMeta::default()
            },
            frames: vec![frame],
            macros: vec![MacroInfo {
                name: "path".to_owned(),
                scope: MacroScope::Global,
                value: "/data".to_owned(),
                truncated: false,
                defined_at: None,
            }],
            errors: vec![Diagnostic {
                severity: Severity::Error,
                code: "STATA0111".to_owned(),
                stata_rc: Some(111),
                message: "variable mpg not found".to_owned(),
                file: None,
                span: None,
                offending_token: Some("mpg".to_owned()),
                block: None,
                related: Vec::new(),
                suggestions: Vec::new(),
                notes: Vec::new(),
                confidence: stratum_proto::Confidence::Exact,
            }],
            ..IntrospectSnapshot::default()
        }
    }

    fn ledger_with(commands: &[&str]) -> ExecutionLedger {
        let mut ledger = ExecutionLedger::new();
        for (i, src) in commands.iter().enumerate() {
            let exec = ExecutionId(i as u64 + 1);
            ledger.append(Committed {
                record: ExecutionRecord {
                    exec,
                    seq: 0,
                    session: SessionId(3),
                    epoch: stratum_proto::SessionEpoch(0),
                    run: stratum_proto::RunId(1),
                    block: stratum_proto::BlockId(i as u64 + 1),
                    doc: None,
                    origin: stratum_proto::ExecOrigin::Editor,
                    code_hash: stratum_proto::CodeHash([0; 16]),
                    source: (*src).to_owned(),
                    input_state: StateId(0),
                    output_state: StateId(0),
                    input_dataset: DatasetStateId(0),
                    output_dataset: DatasetStateId(0),
                    result: None,
                    status: ExecStatus::Succeeded,
                    started_at_ms: 0,
                    duration_us: 0,
                    stale_on_arrival: false,
                    taint: Taint::empty(),
                },
                reads: Arc::new(RecordedReads::default()),
                writes: Arc::new(RecordedWrites::default()),
            });
        }
        ledger
    }

    #[test]
    fn a_summary_that_was_never_computed_is_none_not_empty() {
        let s = snapshot();
        assert!(s.var_stats("default", "price").is_some());
        assert!(s.var_stats("default", "mpg").is_none());
        assert!(s.var_stats("other", "price").is_none());
    }

    #[test]
    fn want_flags_are_honoured_strictly() {
        // The privacy gate filters again downstream, but a field the caller did
        // not ask for must never be populated here — that is what makes the
        // narrowing structural rather than a downstream promise.
        let s = snapshot();
        let ledger = ledger_with(&["summarize price", "regress price mpg"]);
        let view = ledger.view();

        let none = s.ai_context(AiContextWant::empty(), &view);
        assert!(none.dataset.is_none());
        assert!(none.stored.is_none());
        assert!(none.macros.is_empty());
        assert!(none.recent_commands.is_empty());
        assert!(none.recent_errors.is_empty());
        assert!(none.var_summaries.is_empty());
        assert_eq!(none.session, SessionId(3));
        assert_eq!(none.generation, 7);

        let some = s.ai_context(
            AiContextWant::DATASET_META | AiContextWant::RECENT_COMMANDS,
            &view,
        );
        assert_eq!(some.dataset.map(|d| d.n_obs), Some(74));
        assert_eq!(some.recent_commands.len(), 2);
        assert_eq!(some.recent_commands.last().unwrap(), "regress price mpg");
        // Still off, even though the snapshot holds them.
        assert!(some.macros.is_empty());
        assert!(some.var_summaries.is_empty());
    }

    #[test]
    fn recent_errors_is_a_tail_and_never_panics_when_short() {
        let s = snapshot();
        assert_eq!(s.recent_errors(0).len(), 0);
        assert_eq!(s.recent_errors(1).len(), 1);
        assert_eq!(s.recent_errors(99).len(), 1);
    }

    #[test]
    fn the_default_snapshot_answers_every_trait_method() {
        // The engine publishes this one before anything has run, and every pane
        // that asks must get an empty answer rather than a panic.
        let s = IntrospectSnapshot::default();
        assert!(s.frames().is_empty());
        assert!(s.variables("default").is_empty());
        assert!(s.var_stats("default", "x").is_none());
        assert!(s.macros().is_empty());
        assert!(s.estimates_store().is_empty());
        assert!(s.recent_errors(5).is_empty());
        assert_eq!(s.dataset_meta().n_obs, 0);
        assert_eq!(s.completion_env().var_total, 0);
        assert!(s.stored_results().e_b_colnames.is_empty());
    }
}
