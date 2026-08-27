//! ADR-012 (A5) — the desktop's half of `SessionIntrospect`.
//!
//! `stratum-ai` is linked into `stratum-desktop`, and C24 forbids the desktop
//! from reaching `stratum-exec`. So the packer cannot call the engine's
//! `SessionIntrospect` implementation directly; it codes against the trait,
//! which `stratum-proto` declares over proto types, and the desktop implements
//! that trait **against its cache of the reply to
//! `EngineRequest::AiContext`** — which is this type.
//!
//! The engine's own implementation and this one are two implementations of one
//! trait, and `tests/packer_parity.rs` asserts that packing the same session
//! through both produces byte-identical output. That is the test the amendment
//! exists to make possible.

use stratum_proto::complete::CompletionEnv;
use stratum_proto::data::{FrameInfo, QuickSummary, VariableInfo};
use stratum_proto::diagnostic::Diagnostic;
use stratum_proto::introspect::{
    AiContextSnapshot, DatasetMeta, EstimateHandle, MacroInfo, SessionIntrospect, StoredResultsView,
};

/// `SessionIntrospect` over a cached [`AiContextSnapshot`].
///
/// Cheap to build and cheap to replace: the desktop swaps the whole snapshot
/// when a newer `generation` arrives rather than mutating fields, so a packer
/// mid-render can never observe a half-updated session.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct SnapshotIntrospect {
    snapshot: AiContextSnapshot,
}

impl SnapshotIntrospect {
    /// Wrap a snapshot.
    #[must_use]
    pub const fn new(snapshot: AiContextSnapshot) -> Self {
        Self { snapshot }
    }

    /// The snapshot's generation, so a caller can tell whether a captured
    /// precondition still holds (07 §2.7, context invalidation).
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.snapshot.generation
    }

    /// The commands the engine reported, which `SessionIntrospect` has no
    /// method for — they travel to the packer through
    /// [`crate::context::packer::PackRequest`], the same route 07 §5.1 gives
    /// every other caller-supplied input.
    #[must_use]
    pub fn recent_commands(&self) -> &[String] {
        &self.snapshot.recent_commands
    }

    /// The underlying snapshot.
    #[must_use]
    pub const fn snapshot(&self) -> &AiContextSnapshot {
        &self.snapshot
    }
}

impl SessionIntrospect for SnapshotIntrospect {
    fn frames(&self) -> Vec<FrameInfo> {
        // A5's snapshot carries the *current* frame's metadata, which is what
        // the packer renders. Reporting an empty frame list when there is no
        // dataset is correct: `sysuse` has not run yet.
        self.snapshot
            .dataset
            .as_ref()
            .map(|d| {
                vec![FrameInfo {
                    name: d.frame.clone(),
                    n_obs: d.n_obs,
                    n_vars: d.n_vars,
                    sorted_by: d.sorted_by.clone(),
                    changed: false,
                    state: d.state,
                }]
            })
            .unwrap_or_default()
    }

    fn variables(&self, frame: &str) -> Vec<VariableInfo> {
        self.snapshot
            .dataset
            .as_ref()
            .filter(|d| d.frame == frame)
            .map(|d| d.vars.clone())
            .unwrap_or_default()
    }

    fn var_stats(&self, frame: &str, v: &str) -> Option<QuickSummary> {
        // Only ever populated when `AiContextWant::VAR_SUMMARIES` was set, which
        // `want::tier_mask` allows from tier 2. Below that this returns `None`
        // because the desktop never received the data — not because a filter
        // removed it later.
        let known = self
            .snapshot
            .dataset
            .as_ref()
            .is_none_or(|d| d.frame == frame);
        if !known {
            return None;
        }
        self.snapshot
            .var_summaries
            .iter()
            .find(|s| s.var == v)
            .cloned()
    }

    fn macros(&self) -> Vec<MacroInfo> {
        self.snapshot.macros.clone()
    }

    fn stored_results(&self) -> StoredResultsView {
        self.snapshot.stored.clone().unwrap_or_default()
    }

    fn estimates_store(&self) -> Vec<EstimateHandle> {
        self.snapshot.estimates.clone()
    }

    fn recent_errors(&self, n: usize) -> Vec<Diagnostic> {
        let errors = &self.snapshot.recent_errors;
        let from = errors.len().saturating_sub(n);
        errors[from..].to_vec()
    }

    fn dataset_meta(&self) -> DatasetMeta {
        self.snapshot.dataset.clone().unwrap_or_default()
    }

    fn completion_env(&self) -> CompletionEnv {
        // The packer does not read this; the trait requires it. Derived from the
        // snapshot rather than defaulted so that a caller who does read it gets
        // the same session, not an empty one.
        let dataset = self.snapshot.dataset.as_ref();
        let stored = self.snapshot.stored.as_ref();
        let mut env = CompletionEnv {
            generation: self.snapshot.generation,
            frame: dataset.map(|d| d.frame.clone()).unwrap_or_default(),
            frames: dataset.map(|d| vec![d.frame.clone()]).unwrap_or_default(),
            varnames: dataset
                .map(|d| d.vars.iter().map(|v| v.name.clone()).collect())
                .unwrap_or_default(),
            var_total: dataset.map_or(0, |d| d.n_vars),
            truncated: dataset.is_some_and(|d| d.truncated),
            locals: Vec::new(),
            globals: Vec::new(),
            scalars: Vec::new(),
            matrices: Vec::new(),
            programs: Vec::new(),
            e_names: Vec::new(),
            r_names: Vec::new(),
            value_labels: Vec::new(),
            stored_estimates: self
                .snapshot
                .estimates
                .iter()
                .map(|e| e.name.clone())
                .collect(),
            cwd: camino::Utf8PathBuf::new(),
        };
        for m in &self.snapshot.macros {
            match m.scope {
                stratum_proto::introspect::MacroScope::Local => env.locals.push(m.name.clone()),
                stratum_proto::introspect::MacroScope::Global => env.globals.push(m.name.clone()),
            }
        }
        if let Some(s) = stored {
            env.e_names
                .extend(s.e_scalars.iter().map(|(k, _)| k.clone()));
            env.e_names
                .extend(s.e_macros.iter().map(|(k, _)| k.clone()));
            env.r_names
                .extend(s.r_scalars.iter().map(|(k, _)| k.clone()));
            env.r_names
                .extend(s.r_macros.iter().map(|(k, _)| k.clone()));
        }
        env
    }
}

#[cfg(test)]
mod tests {
    use stratum_proto::ids::SessionId;

    use super::*;

    fn snapshot() -> AiContextSnapshot {
        AiContextSnapshot {
            session: SessionId(1),
            generation: 7,
            dataset: Some(DatasetMeta {
                frame: "default".into(),
                n_obs: 74,
                ..DatasetMeta::default()
            }),
            ..AiContextSnapshot::default()
        }
    }

    #[test]
    fn an_empty_snapshot_answers_every_method_without_panicking() {
        // The state a fresh session is in for its first seconds. A packer that
        // panicked here would take the whole AI panel down on launch.
        let s = SnapshotIntrospect::default();
        assert!(s.frames().is_empty());
        assert!(s.variables("default").is_empty());
        assert!(s.var_stats("default", "price").is_none());
        assert!(s.macros().is_empty());
        assert!(s.estimates_store().is_empty());
        assert!(s.recent_errors(5).is_empty());
        assert_eq!(s.dataset_meta().n_obs, 0);
        assert_eq!(s.stored_results(), StoredResultsView::default());
    }

    #[test]
    fn variables_are_scoped_to_their_frame() {
        let s = SnapshotIntrospect::new(snapshot());
        assert_eq!(s.frames().len(), 1);
        assert!(
            s.variables("other").is_empty(),
            "a frame we have no data for is empty, not wrong"
        );
    }

    #[test]
    fn recent_errors_returns_the_newest_n() {
        let mut snap = snapshot();
        for i in 0..5u32 {
            snap.recent_errors.push(Diagnostic {
                severity: stratum_proto::diagnostic::Severity::Error,
                code: format!("STATA{i:04}"),
                stata_rc: Some(111),
                message: format!("e{i}"),
                file: None,
                span: None,
                offending_token: None,
                block: None,
                related: Vec::new(),
                suggestions: Vec::new(),
                notes: Vec::new(),
                confidence: stratum_proto::diagnostic::Confidence::Exact,
            });
        }
        let s = SnapshotIntrospect::new(snap);
        let got = s.recent_errors(2);
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].message, "e4", "newest last");
    }
}
