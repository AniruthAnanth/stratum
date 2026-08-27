//! The reproducibility audit — `R001`–`R026` and the spec §16 roll-up.
//!
//! ARCHITECTURE C15 puts [`ReproReport`] here, and design 03 §10 describes the
//! audit as "a static abstract interpretation over blocks in document order".
//! [`checks`] holds the twenty-six analyses; this module is the *driver* and the
//! roll-up, and both of its jobs are dominated by one rule.
//!
//! # The honesty rule decides the shape of this module
//!
//! Design 03 §10.2 and `stratum_proto::repro`'s own header: *"a green mark that
//! was inferred from static analysis is the single worst thing this feature
//! could ship."* Three consequences are structural rather than editorial.
//!
//! * **[`Tri::Yes`] needs evidence, and [`Tri::No`] needs evidence too.** The
//!   roll-up below never reads "no findings" as "verified"; a field whose check
//!   could not run — `R004` with no complete project listing, `R024` with no
//!   execution — reports [`Tri::Unknown`], and the UI renders "not verified".
//! * **`runs_clean` is not computed here at all.** It is `Unknown` unless the
//!   caller hands over an [`Observed`] footprint saying an actual
//!   `Isolation::Subprocess` clean run happened. This crate cannot run anything.
//! * **Suppressions are data, not silence.** A `*! nolint(R001)` drops the
//!   finding *and* is listed in [`ReproReport::suppressed`], so a reviewer sees
//!   what was silenced.
//!
//! # What the caller must supply, and why it is not optional
//!
//! [`Audit::new`] takes a [`DocumentId`] and [`Audit::run`] takes a `UnixMs`
//! rather than reaching for either. Audit item A2 keeps `time` out of the wire
//! layer, and ARCHITECTURE's crate table keeps `time` out of *this* crate's
//! dependency tree entirely — it builds for `wasm32-unknown-unknown` and runs in
//! the editor, where there is no clock to reach for and no document identity to
//! invent. Both are facts the host knows and this crate does not.
//!
//! # One parse, thirty-eight checks
//!
//! [`Audit::run`] builds one [`Doc`] and hands every check a borrow of it.
//! A caller that also lints uses [`Audit::run_with`] against the [`Doc`] it
//! already built, so a file that is both linted and audited is parsed **once**
//! — the counter design 07 §6.3 and ADR-017 care about, and the reason
//! [`crate::lints::lint_with`] exists in the same shape.

pub mod checks;

pub use checks::{Check, CHECKS};

use stratum_effects::EffectTable;
use stratum_proto::diagnostic::Severity;
use stratum_proto::repro::{Finding, ReproReport, Tri};
use stratum_proto::{DocumentId, ExecutionId, Span, UnixMs};

use crate::lints::dataflow::Doc;
use crate::{Env, ParseIndex};

/// One check's identity, as the registry declares it.
///
/// The same four fields `lints::LintMeta` carries, minus `owner` and `has_fix`:
/// every `R###` is implemented here, and whether a given finding offers a fix
/// depends on the *finding* (an absolute path outside the project root has no
/// honest rewrite) rather than on the rule.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CheckMeta {
    /// The wire code, `"R001"` — `Diagnostic.code` / `Finding.lint`.
    pub id: &'static str,
    /// Default severity.
    pub severity: Severity,
    /// One line, sentence case, no trailing period. What the finding card titles.
    pub title: &'static str,
    /// The rule, in the words design 03 §10.1 / design 07 §10 wrote it.
    pub rule: &'static str,
}

/// The twenty-six checks, in id order.
#[must_use]
pub fn registry() -> &'static [Check] {
    CHECKS
}

/// Look one check up by code.
#[must_use]
pub fn meta(id: &str) -> Option<&'static CheckMeta> {
    CHECKS.iter().find(|c| c.meta.id == id).map(|c| &c.meta)
}

/// Everything a check is allowed to read.
///
/// There is deliberately no field a check can use to reach the filesystem, the
/// session or the network. What the buffer does not say, [`Env`] says or nobody
/// does — which is what makes `R004`'s "I cannot decide" a *type-level* fact
/// rather than a discipline.
pub struct Cx<'a> {
    /// The segmented buffer, for mapping a line-local span back to the source.
    pub idx: &'a ParseIndex<'a>,
    /// Every statement, parsed once.
    pub doc: &'a Doc<'a>,
    /// What the host knows about the world outside the buffer.
    pub env: &'a Env,
    /// The authoritative effect table, when the caller links a crate that has
    /// rows. `None` in the wasm build, where `lints::facts`' conservative
    /// fallback is the whole answer.
    pub effects: Option<&'a dyn EffectTable>,
    /// What an execution actually did. `None` for a purely static audit, which
    /// is why `R024` emits nothing and `runs_clean` stays [`Tri::Unknown`].
    pub observed: Option<&'a Observed>,
}

/// One `*! stratum:` effect declaration that the observed footprint refuted.
///
/// Design 03 §5.3 rule 9 is "trust, then verify, then never trust again": the
/// annotation is believed by the static extractor, checked against the run, and
/// on a disagreement the block is permanently downgraded to `UNKNOWN_ALL`.
/// `R024` is the user-visible half of that, and the comparison itself belongs to
/// the runtime — this crate never saw the run, so the disagreement arrives as
/// data.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Contradiction {
    /// The command whose declaration was refuted, as it should be printed.
    pub command: String,
    /// What it touched and said it would not — `"the variable `wage`"`,
    /// `` "the file `out.dta`" ``. Rendered into the message verbatim.
    pub footprint: String,
    /// Where the declaration is, in source coordinates.
    pub span: Span,
}

/// Whether the execution behind an [`Observed`] was an actual clean run.
///
/// This is the only thing in the workspace allowed to move `runs_clean` off
/// [`Tri::Unknown`], so it distinguishes "we did not check" from "we checked and
/// it failed". Collapsing the two is exactly the inference design 03 §10.2
/// forbids.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CleanRun {
    /// The execution was a normal in-process run, not an `Isolation::Subprocess`
    /// clean run. It says nothing about whether the file runs from clean state.
    NotVerified,
    /// A clean-state subprocess run of this file completed without error.
    Succeeded,
    /// A clean-state subprocess run of this file did not complete.
    Failed,
}

/// What an execution actually did, as the host observed it.
///
/// Everything here is a fact from outside this crate. `stratum-intel` links no
/// runtime and no engine (ARCHITECTURE C24, C26), so an audit that wants to say
/// anything post-execution is *given* the observation.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Observed {
    execution: ExecutionId,
    clean_run: CleanRun,
    duration_us: Option<u64>,
    contradictions: Vec<Contradiction>,
}

impl Observed {
    /// An observation from `execution`, which was or was not a clean run.
    #[must_use]
    pub fn new(execution: ExecutionId, clean_run: CleanRun) -> Self {
        Observed {
            execution,
            clean_run,
            duration_us: None,
            contradictions: Vec::new(),
        }
    }

    /// How long the run took. Recorded, never asserted on (ADR-017).
    #[must_use]
    pub fn with_duration_us(mut self, us: u64) -> Self {
        self.duration_us = Some(us);
        self
    }

    /// Add one refuted effect declaration for `R024`.
    #[must_use]
    pub fn with_contradiction(mut self, c: Contradiction) -> Self {
        self.contradictions.push(c);
        self
    }

    /// The execution this came from.
    #[must_use]
    pub fn execution(&self) -> ExecutionId {
        self.execution
    }

    /// Whether it was a verified clean run.
    #[must_use]
    pub fn clean_run(&self) -> CleanRun {
        self.clean_run
    }

    /// Wall time, when the host recorded it.
    #[must_use]
    pub fn duration_us(&self) -> Option<u64> {
        self.duration_us
    }

    /// Every refuted effect declaration, in the order the run found them.
    #[must_use]
    pub fn contradictions(&self) -> &[Contradiction] {
        &self.contradictions
    }
}

/// The audit, as a builder.
///
/// The two `with_*` methods are the two things a caller may or may not have.
/// Neither changes what the checks *are*; both let a caller that knows more get
/// a sharper answer, and a caller that knows less still gets a sound one.
pub struct Audit<'a> {
    idx: &'a ParseIndex<'a>,
    env: &'a Env,
    doc_id: DocumentId,
    effects: Option<&'a dyn EffectTable>,
    observed: Option<&'a Observed>,
}

impl<'a> Audit<'a> {
    /// An audit of `idx` against what `env` knows, for document `doc_id`.
    #[must_use]
    pub fn new(idx: &'a ParseIndex<'a>, env: &'a Env, doc_id: DocumentId) -> Self {
        Audit {
            idx,
            env,
            doc_id,
            effects: None,
            observed: None,
        }
    }

    /// Prefer `table`'s rows wherever it has one.
    ///
    /// `EffectTable` is the authority (`lints::facts`' header says why it must
    /// be), but it is row-free by construction and its rows live in
    /// `stratum-runtime` and `stratum-stats`, which this crate does not link.
    /// A caller that *does* link them passes the table in here; the wasm build
    /// does not, and falls back to `lints::facts`.
    #[must_use]
    pub fn with_effects(mut self, table: &'a dyn EffectTable) -> Self {
        self.effects = Some(table);
        self
    }

    /// Supply what an execution actually did, which is the only thing that can
    /// move `runs_clean` off [`Tri::Unknown`] or make `R024` emit.
    #[must_use]
    pub fn with_observed(mut self, observed: &'a Observed) -> Self {
        self.observed = Some(observed);
        self
    }

    /// Run all twenty-six checks and roll them up.
    ///
    /// `generated_at_ms` is the caller's (A2 — see this module's header).
    #[must_use]
    pub fn run(&self, generated_at_ms: UnixMs) -> ReproReport {
        let doc = Doc::build(self.idx);
        self.run_with(&doc, generated_at_ms)
    }

    /// [`Audit::run`] against an already-built [`Doc`], so a caller that also
    /// lints parses the file once rather than twice.
    #[must_use]
    pub fn run_with(&self, doc: &Doc<'_>, generated_at_ms: UnixMs) -> ReproReport {
        let findings = self.findings_with(doc);
        ReproReport {
            doc: self.doc_id,
            file_hash: stratum_parse::text_hash(self.idx.source()),
            generated_at_ms,
            runs_clean: self.runs_clean(&findings),
            verified_by: self.observed.map(Observed::execution),
            verified_duration_us: self.observed.and_then(Observed::duration_us),
            // Design 03 §10.2's roll-up, code for code.
            seed_defined: tri(&findings, &["R002", "R003"]),
            inputs_resolved: self.inputs_resolved(&findings),
            no_hidden_deps: tri(&findings, &["R006", "R009", "R011"]),
            suppressed: doc.suppressions.clone(),
            findings,
        }
    }

    /// Every unsuppressed finding, in the problems pane's total order.
    #[must_use]
    pub fn findings_with(&self, doc: &Doc<'_>) -> Vec<Finding> {
        let cx = Cx {
            idx: self.idx,
            doc,
            env: self.env,
            effects: self.effects,
            observed: self.observed,
        };
        let mut out = Vec::new();
        for check in CHECKS {
            (check.run)(&cx, &mut out);
        }
        let li = &self.idx.segmentation().line_index;
        out.retain(|f| {
            f.span
                .map(|s| li.line_of(s.start))
                .is_none_or(|line| !doc.suppresses(self.idx, line, &f.lint))
        });
        sort_findings(&mut out);
        out
    }

    /// Design 03 §10.2: `Yes` only from an actual clean run "with no Error
    /// findings and no `Taint::EXTERNAL`" — the latter being exactly what
    /// `R012` reports.
    fn runs_clean(&self, findings: &[Finding]) -> Tri {
        let Some(observed) = self.observed else {
            return Tri::Unknown;
        };
        match observed.clean_run {
            CleanRun::NotVerified => Tri::Unknown,
            CleanRun::Failed => Tri::No,
            CleanRun::Succeeded => {
                let blocked = findings
                    .iter()
                    .any(|f| f.severity == Severity::Error || f.lint == "R012");
                // The run happened and it worked, so `No` would be a lie. But an
                // unseeded draw or a `shell` call means the *next* run is not
                // this run, and one green pass does not establish that. "Not
                // verified" is the honest report.
                if blocked {
                    Tri::Unknown
                } else {
                    Tri::Yes
                }
            }
        }
    }

    /// `R004` is the only check that can answer this, and it declines to run at
    /// all without a complete project listing — so "no `R004` findings" means
    /// "verified" only when the listing was complete. `R005` is a path this
    /// crate cannot resolve *even with* the listing, which is the definition of
    /// `Unknown` rather than of a tick (design 03 §10.2 renders it as the
    /// "⚠ 2 dynamic paths" line).
    fn inputs_resolved(&self, findings: &[Finding]) -> Tri {
        if !self.env.file_listing_is_complete {
            return Tri::Unknown;
        }
        if findings.iter().any(|f| f.lint == "R004") {
            return Tri::No;
        }
        if findings.iter().any(|f| f.lint == "R005") {
            return Tri::Unknown;
        }
        Tri::Yes
    }
}

/// `No` if any of `codes` fired, `Yes` otherwise. Only used for the fields whose
/// checks are decidable from the buffer alone; everything else routes through a
/// method that can return `Unknown`.
fn tri(findings: &[Finding], codes: &[&str]) -> Tri {
    if findings.iter().any(|f| codes.contains(&f.lint.as_str())) {
        Tri::No
    } else {
        Tri::Yes
    }
}

/// Severity, then position, then code — the same total order
/// [`crate::lints::lint_document`] returns, so the problems pane can merge the
/// two lists and diff the result instead of repainting.
fn sort_findings(v: &mut [Finding]) {
    v.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then_with(|| span_key(a.span).cmp(&span_key(b.span)))
            .then_with(|| a.lint.cmp(&b.lint))
            .then_with(|| a.message.cmp(&b.message))
    });
}

fn span_key(s: Option<Span>) -> (u32, u32) {
    s.map_or((u32::MAX, u32::MAX), |x| (x.start, x.end))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use camino::Utf8PathBuf;
    use stratum_effects::{EffectSet, StaticCtx};
    use stratum_parse::CommandAst;

    use super::*;

    /// A fixed stamp: the audit never reads a clock, so a test does not need one.
    const AT: UnixMs = 1_700_000_000_000;

    fn report(src: &str, env: &Env) -> ReproReport {
        let idx = ParseIndex::new(src);
        Audit::new(&idx, env, DocumentId(1)).run(AT)
    }

    fn codes(r: &ReproReport) -> Vec<&str> {
        r.findings.iter().map(|f| f.lint.as_str()).collect()
    }

    #[test]
    fn every_check_is_reachable_through_the_registry() {
        assert_eq!(registry().len(), 26);
        assert_eq!(meta("R001").map(|m| m.id), Some("R001"));
        assert_eq!(meta("R026").map(|m| m.severity), Some(Severity::Warning));
        assert!(meta("R027").is_none());
    }

    /// The honesty rule, as the roll-up sees it: with no execution and no
    /// project listing there is nothing to tick, and nothing to fail either.
    #[test]
    fn nothing_is_verified_without_evidence() {
        let r = report(
            "version 18\nsysuse auto, clear\nsummarize price\n",
            &Env::default(),
        );
        assert_eq!(r.runs_clean, Tri::Unknown);
        assert_eq!(r.verified_by, None);
        assert_eq!(r.verified_duration_us, None);
        assert_eq!(r.inputs_resolved, Tri::Unknown, "{:?}", codes(&r));
        assert_eq!(r.doc, DocumentId(1));
        assert_eq!(r.generated_at_ms, AT);
    }

    #[test]
    fn seed_defined_follows_r002_and_r003() {
        let unseeded = report("version 18\ngenerate u = runiform()\n", &Env::default());
        assert_eq!(unseeded.seed_defined, Tri::No);
        assert!(codes(&unseeded).contains(&"R002"));

        let seeded = report(
            "version 18\nset seed 20260821\ngenerate u = runiform()\n",
            &Env::default(),
        );
        assert_eq!(seeded.seed_defined, Tri::Yes, "{:?}", codes(&seeded));

        let clock = report(
            "version 18\nset seed `c(current_time)'\ngenerate u = runiform()\n",
            &Env::default(),
        );
        assert_eq!(clock.seed_defined, Tri::No);
        assert!(codes(&clock).contains(&"R003"));
    }

    /// `R004` declines to run without a complete listing, so "no findings" is
    /// only a tick when the listing was complete.
    #[test]
    fn inputs_resolved_needs_a_complete_project_listing() {
        let src = "version 18\nuse data/raw.dta, clear\n";
        let mut env = Env {
            project_files: vec![Utf8PathBuf::from("data/raw.dta")],
            ..Env::default()
        };
        assert_eq!(report(src, &env).inputs_resolved, Tri::Unknown);

        env.file_listing_is_complete = true;
        let r = report(src, &env);
        assert_eq!(r.inputs_resolved, Tri::Yes, "{:?}", codes(&r));

        env.project_files.clear();
        let missing = report(src, &env);
        assert_eq!(missing.inputs_resolved, Tri::No);
        assert!(codes(&missing).contains(&"R004"));
    }

    /// Design 03 §10.2 renders `R005` as a warning line, not a tick. `Tri` has
    /// no warning state, and "cannot verify" is `Unknown`, never `Yes`.
    #[test]
    fn a_macro_built_input_path_is_unknown_rather_than_a_tick() {
        let env = Env {
            file_listing_is_complete: true,
            globals: vec!["ROOT".to_owned()],
            ..Env::default()
        };
        let r = report("version 18\nuse $ROOT/raw.dta, clear\n", &env);
        assert!(codes(&r).contains(&"R005"), "{:?}", codes(&r));
        assert_eq!(r.inputs_resolved, Tri::Unknown);
    }

    #[test]
    fn no_hidden_deps_catches_a_macro_the_session_supplied() {
        let r = report("version 18\nsummarize `outcome'\n", &Env::default());
        assert!(codes(&r).contains(&"R006"), "{:?}", codes(&r));
        assert_eq!(r.no_hidden_deps, Tri::No);

        let defined = report(
            "version 18\nlocal outcome price\nsummarize `outcome'\n",
            &Env::default(),
        );
        assert!(!codes(&defined).contains(&"R006"), "{:?}", codes(&defined));
    }

    /// `e()` read with no estimation in the file is the headline case in
    /// `R006`'s own rule text.
    #[test]
    fn no_hidden_deps_catches_an_estimate_from_the_command_bar() {
        let r = report(
            "version 18\nsysuse auto, clear\ndisplay e(N)\n",
            &Env::default(),
        );
        assert!(codes(&r).contains(&"R006"), "{:?}", codes(&r));

        let after = report(
            "version 18\nsysuse auto, clear\nregress price mpg\ndisplay e(N)\n",
            &Env::default(),
        );
        assert!(!codes(&after).contains(&"R006"), "{:?}", codes(&after));

        let session = Env {
            e_names: vec!["N".to_owned()],
            ..Env::default()
        };
        let held = report("version 18\ndisplay e(N)\n", &session);
        assert!(!codes(&held).contains(&"R006"), "{:?}", codes(&held));
    }

    #[test]
    fn a_suppression_drops_the_finding_and_is_still_listed() {
        let src = "version 18\n*! nolint(R009)\nbrowse\n";
        let r = report(src, &Env::default());
        assert!(!codes(&r).contains(&"R009"), "{:?}", codes(&r));
        assert_eq!(r.suppressed.len(), 1);
        assert_eq!(r.suppressed[0].0, "R009");
        // Silenced, but the roll-up still refuses the tick it never earned.
        assert_eq!(r.no_hidden_deps, Tri::Yes);
    }

    #[test]
    fn r024_says_nothing_without_an_execution() {
        let src = "version 18\nsysuse auto, clear\nmyado price\n";
        assert!(!codes(&report(src, &Env::default())).contains(&"R024"));

        let observed = Observed::new(ExecutionId(41), CleanRun::NotVerified).with_contradiction(
            Contradiction {
                command: "myado".to_owned(),
                footprint: "the file `out.dta`".to_owned(),
                span: Span { start: 30, end: 39 },
            },
        );
        let idx = ParseIndex::new(src);
        let env = Env::default();
        let r = Audit::new(&idx, &env, DocumentId(1))
            .with_observed(&observed)
            .run(AT);
        let hit = r.findings.iter().find(|f| f.lint == "R024").expect("R024");
        assert!(hit.message.contains("myado"), "{}", hit.message);
        assert!(hit.message.contains("out.dta"), "{}", hit.message);
        assert_eq!(r.verified_by, Some(ExecutionId(41)));
        // A run that was not an isolated clean run proves nothing about clean state.
        assert_eq!(r.runs_clean, Tri::Unknown);
    }

    #[test]
    fn runs_clean_needs_a_clean_run_that_worked_and_nothing_that_blocks_it() {
        let clean = "version 18\nsysuse auto, clear\nsummarize price\n";
        let idx = ParseIndex::new(clean);
        let env = Env::default();

        let ok = Observed::new(ExecutionId(7), CleanRun::Succeeded).with_duration_us(1234);
        let r = Audit::new(&idx, &env, DocumentId(1))
            .with_observed(&ok)
            .run(AT);
        assert_eq!(r.runs_clean, Tri::Yes, "{:?}", codes(&r));
        assert_eq!(r.verified_duration_us, Some(1234));

        let failed = Observed::new(ExecutionId(8), CleanRun::Failed);
        let r = Audit::new(&idx, &env, DocumentId(1))
            .with_observed(&failed)
            .run(AT);
        assert_eq!(r.runs_clean, Tri::No);

        // `R012` sets `Taint::EXTERNAL`, which design 03 §10.2 says blocks the
        // tick even when the run itself succeeded.
        let external = "version 18\nsysuse auto, clear\nshell ./prepare.sh\n";
        let idx = ParseIndex::new(external);
        let r = Audit::new(&idx, &env, DocumentId(1))
            .with_observed(&ok)
            .run(AT);
        assert!(codes(&r).contains(&"R012"), "{:?}", codes(&r));
        assert_eq!(r.runs_clean, Tri::Unknown);
    }

    #[test]
    fn findings_come_back_in_one_total_order() {
        let src = "use \"/a/b.dta\", clear\ngenerate u = runiform()\nbrowse\n";
        let a = report(src, &Env::default());
        let b = report(src, &Env::default());
        assert_eq!(codes(&a), codes(&b));
        let keys: Vec<_> = a
            .findings
            .iter()
            .map(|f| (f.severity, span_key(f.span)))
            .collect();
        assert!(keys.windows(2).all(|w| w[0] <= w[1]), "{keys:?}");
    }

    /// A table that says `describe` writes a file. `lints::facts` says it does
    /// not, so a finding here can only have come from the table.
    struct SaysDescribeWrites;

    impl EffectTable for SaysDescribeWrites {
        fn effects(&self, _cmd: &CommandAst, _ctx: &StaticCtx<'_>) -> EffectSet {
            let mut e = EffectSet::new();
            e.file_writes.insert(Utf8PathBuf::from("out.txt"));
            e
        }
        fn is_known_command(&self, name: &str) -> bool {
            name == "describe"
        }
    }

    #[test]
    fn the_effect_table_is_preferred_wherever_it_has_a_row() {
        let src = "version 18\nsysuse auto, clear\ncapture describe\n";
        let idx = ParseIndex::new(src);
        let env = Env::default();
        assert!(!codes(&report(src, &env)).contains(&"R016"));

        let table = SaysDescribeWrites;
        let r = Audit::new(&idx, &env, DocumentId(1))
            .with_effects(&table)
            .run(AT);
        assert!(codes(&r).contains(&"R016"), "{:?}", codes(&r));
    }

    /// A file that trips as much of the registry as one file can. Its job is to
    /// keep the twenty-six checks running over hostile input, and to pin the
    /// false positives that the first compiling build of `checks.rs` produced:
    /// `merge`'s `1:1` and `import`'s `delimited` read as filenames, and
    /// `shell` read as an uninstalled ado because `stratum-parse`'s command
    /// table does not model it.
    #[test]
    fn the_whole_registry_runs_over_a_hostile_file() {
        let src = "use \"/Users/ana/raw.dta\", clear\n\
                   merge 1:1 id using \"other.dta\"\n\
                   drop _merge\n\
                   import delimited raw.csv\n\
                   graph export fig.png\n\
                   shell ./prepare.sh\n\
                   capture save \"/Users/ana/raw.dta\", replace\n";
        let env = Env {
            file_listing_is_complete: true,
            project_files: vec![Utf8PathBuf::from("other.dta"), Utf8PathBuf::from("raw.csv")],
            ..Env::default()
        };
        let r = report(src, &env);
        let found = codes(&r);
        for f in &r.findings {
            assert!(meta(&f.lint).is_some(), "unregistered code {}", f.lint);
            assert!(!f.message.is_empty() && !f.title.is_empty(), "{}", f.lint);
        }
        // `1:1`, `id`, `delimited` and `export` are not filenames.
        assert!(!found.contains(&"R004"), "{:?}", r.findings);
        // `shell` is built in; `R012` reports it and `R025` must not.
        assert!(found.contains(&"R012"), "{found:?}");
        assert!(!found.contains(&"R025"), "{found:?}");
        // The collision message quotes the path as the user wrote it.
        let collision = r.findings.iter().find(|f| f.lint == "R010").expect("R010");
        assert!(
            collision.message.contains("/Users/ana/raw.dta"),
            "{}",
            collision.message
        );
    }

    /// `run_with` exists so a caller that lints and audits parses once. The two
    /// entry points must not disagree.
    #[test]
    fn run_and_run_with_agree_on_an_already_built_doc() {
        let src = "version 18\nsysuse auto, clear\nsort price\nlist in 1/5\n";
        let idx = ParseIndex::new(src);
        let env = Env::default();
        let audit = Audit::new(&idx, &env, DocumentId(1));
        let doc = Doc::build(&idx);
        assert_eq!(audit.run(AT), audit.run_with(&doc, AT));
    }
}
