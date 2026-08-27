//! One inhabited value for every type in `stratum-proto`, and every variant of
//! every enum that crosses the wire.
//!
//! This module is the reason `roundtrip.rs` can claim to cover "every wire
//! type": adding a type or a variant without adding it here leaves an obvious
//! hole, and adding it here is what makes the round-trip, the field-name and the
//! forward-compatibility tests see it.
//!
//! Values are deliberately awkward — non-ASCII text, `f64`s that need all 17
//! significant digits, `u64::MAX` ids — because a fixture of zeroes and empty
//! vectors passes a round-trip test that a real payload would fail.

#![allow(dead_code)]

use camino::Utf8PathBuf;
use stratum_proto::*;

pub fn path() -> Utf8PathBuf {
    Utf8PathBuf::from("/Users/ana/proj/analysis/01 clean.do")
}

pub fn span() -> Span {
    Span {
        start: 12,
        end: 4_294_967_295,
    }
}

pub fn line_range() -> LineRange {
    LineRange { start: 3, end: 9 }
}

pub fn code_hash() -> CodeHash {
    CodeHash(*b"\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\xfa\xfb\xfc\xfd\xfe\xff")
}

pub fn text_hash() -> TextHash {
    TextHash(*b"0123456789abcdef")
}

/// A value that needs `float_roundtrip` to survive JSON, plus a Stata missing
/// sentinel's neighbourhood, plus a plain integer-valued double.
pub fn awkward_f64s() -> [f64; 3] {
    [0.1 + 0.2, 8.988_465_674_311_58e307, 42.0]
}

pub fn edit() -> Edit {
    Edit {
        span: span(),
        text: "gen lnwage = ln(wage)\n".into(),
    }
}

// ---------------------------------------------------------------------------
// §1 identity
// ---------------------------------------------------------------------------

pub fn ids() -> (
    SessionId,
    SessionEpoch,
    RunId,
    ExecutionId,
    StateId,
    DatasetStateId,
    ResultId,
    BlockId,
    DocumentId,
    FrameId,
    VarId,
    VarIdx,
    SectionId,
    OrderId,
) {
    (
        SessionId(7),
        SessionEpoch(2),
        RunId(u64::MAX),
        ExecutionId(41),
        StateId(9_007_199_254_740_993),
        DatasetStateId(17),
        ResultId(41),
        BlockId(3),
        DocumentId(1),
        FrameId(0),
        VarId(88),
        VarIdx(4),
        SectionId(2),
        OrderId(11),
    )
}

// ---------------------------------------------------------------------------
// §1.2 tokens
// ---------------------------------------------------------------------------

pub fn token_kinds() -> Vec<TokenKind> {
    use TokenKind::*;
    vec![
        Ident,
        Number,
        StrLit,
        CompoundQuote,
        MacroRef,
        Op,
        Comma,
        Colon,
        LParen,
        RParen,
        LBrace,
        RBrace,
        LBracket,
        RBracket,
        Comment,
        Whitespace,
        StatementBreak,
        Continuation,
        Directive,
        Unknown,
    ]
}

pub fn token() -> Token {
    Token {
        kind: TokenKind::MacroRef,
        span: span(),
    }
}

pub fn canon_token() -> CanonToken {
    CanonToken {
        kind: TokenKind::CompoundQuote,
        text: "`\"a b\"'".into(),
    }
}

// ---------------------------------------------------------------------------
// §2 blocks
// ---------------------------------------------------------------------------

pub fn region_kinds() -> Vec<RegionKind> {
    vec![
        RegionKind::Simple,
        RegionKind::Brace {
            opener: BraceOpener::Foreach,
        },
        RegionKind::EndBlock {
            opener: EndBlockOpener::Program,
            name: Some("mycmd".into()),
        },
        RegionKind::EndBlock {
            opener: EndBlockOpener::Mata,
            name: None,
        },
        RegionKind::Directive {
            directive: DirectiveKind::DelimitSemi,
        },
        RegionKind::Trivia { has_marker: true },
        RegionKind::Unterminated {
            expected: Unterminated::CompoundQuote,
        },
    ]
}

pub fn brace_openers() -> Vec<BraceOpener> {
    use BraceOpener::*;
    vec![
        Foreach,
        Forvalues,
        While,
        IfElseChain,
        Capture,
        Quietly,
        Noisily,
        Anonymous,
        Other,
    ]
}

pub fn end_block_openers() -> Vec<EndBlockOpener> {
    use EndBlockOpener::*;
    vec![Program, Input, Mata, Python, Java]
}

pub fn directive_kinds() -> Vec<DirectiveKind> {
    vec![
        DirectiveKind::DelimitCr,
        DirectiveKind::DelimitSemi,
        DirectiveKind::Other,
    ]
}

pub fn unterminateds() -> Vec<Unterminated> {
    use Unterminated::*;
    vec![CloseBrace, End, BlockComment, CompoundQuote]
}

pub fn region_summary() -> RegionSummary {
    RegionSummary {
        index: 4,
        span: span(),
        outer_span: Span {
            start: 0,
            end: 4_294_967_295,
        },
        lines: line_range(),
        code_lines: LineRange { start: 4, end: 9 },
        kind: RegionKind::Brace {
            opener: BraceOpener::Foreach,
        },
        entry_delimiter: Delimiter::Semi,
        exit_delimiter: Delimiter::Cr,
        code_hash: code_hash(),
        hash_ordinal: 1,
        canonical: Some("foreach".into()),
        is_estimation: false,
        has_macro_in_head: true,
        section: Some(SectionId(2)),
    }
}

pub fn cell_marker() -> CellMarker {
    CellMarker {
        span: span(),
        line: 12,
        title: "Descriptives — wave ①".into(),
        section: SectionId(2),
    }
}

pub fn section_span() -> SectionSpan {
    SectionSpan {
        id: SectionId(2),
        span: span(),
        title: "Clean".into(),
        lines: line_range(),
    }
}

pub fn block_map() -> BlockMap {
    BlockMap {
        doc: DocumentId(1),
        generation: 12,
        doc_version: 340,
        // A3: the trivia region carries NONE, not EPHEMERAL.
        blocks: vec![BlockId(3), BlockId::NONE, BlockId::EPHEMERAL],
        regions: vec![region_summary()],
        markers: vec![cell_marker()],
        sections: vec![section_span()],
        retired: vec![BlockId(2)],
        diagnostics: vec![diagnostic()],
        end_delimiter: Delimiter::Cr,
    }
}

pub fn block() -> Block {
    Block {
        id: BlockId(3),
        region: region_summary(),
        doc: DocumentId(1),
    }
}

// ---------------------------------------------------------------------------
// §3 status
// ---------------------------------------------------------------------------

pub fn dep_keys() -> Vec<DepKey> {
    vec![
        DepKey::Var {
            frame: "default".into(),
            name: "income".into(),
        },
        DepKey::RowMembership {
            frame: "default".into(),
        },
        DepKey::RowOrder {
            frame: "default".into(),
        },
        DepKey::VarLayout {
            frame: "default".into(),
        },
        DepKey::Macro {
            name: "controls".into(),
        },
        DepKey::Scalar { name: "n".into() },
        DepKey::Matrix { name: "b".into() },
        DepKey::Program {
            name: "mycmd".into(),
        },
        DepKey::Estimates,
        DepKey::RClass,
        DepKey::SClass,
        DepKey::Rng,
        DepKey::Setting {
            name: "type".into(),
        },
        DepKey::Cwd,
        DepKey::File { path: path() },
    ]
}

pub fn stale_reasons() -> Vec<StaleReason> {
    vec![
        StaleReason::CodeChanged,
        StaleReason::EpochReset,
        StaleReason::InputChanged {
            key: DepKey::Var {
                frame: "default".into(),
                name: "income".into(),
            },
            at: Some(ExecutionId(44)),
        },
        StaleReason::FileChanged { path: path() },
        StaleReason::UpstreamPending {
            block: BlockId(2),
            via: DepKey::RClass,
        },
        StaleReason::UpstreamOpaque { block: BlockId(1) },
        StaleReason::RngShifted,
    ]
}

pub fn broken_reasons() -> Vec<BrokenReason> {
    vec![
        BrokenReason::UnresolvedName {
            name: "incme".into(),
            suggestion: Some("income".into()),
        },
        BrokenReason::UnknownCommand {
            name: "regres".into(),
            suggestion: None,
        },
        BrokenReason::MissingFile { path: path() },
    ]
}

pub fn taint() -> Taint {
    Taint::EXTERNAL | Taint::UNBOUNDED_LOOP | Taint::MACRO_VARLIST
}

pub fn block_statuses() -> Vec<BlockStatus> {
    vec![
        BlockStatus::NeverRun,
        BlockStatus::Queued { position: 3 },
        BlockStatus::Running {
            exec: ExecutionId(41),
            started_ms: 1_755_800_000_000,
        },
        BlockStatus::Current {
            exec: ExecutionId(41),
            dataset: DatasetStateId(17),
            duration_us: 80_000,
        },
        BlockStatus::CurrentUnverifiable {
            exec: ExecutionId(42),
            dataset: DatasetStateId(18),
            duration_us: 1,
            taint: taint(),
        },
        BlockStatus::Stale {
            reason: StaleReason::CodeChanged,
            since: Some(ExecutionId(40)),
        },
        BlockStatus::Stale {
            reason: StaleReason::RngShifted,
            since: None,
        },
        BlockStatus::Failed {
            exec: ExecutionId(43),
            rc: 111,
        },
        BlockStatus::Interrupted {
            exec: ExecutionId(44),
            rolled_back: true,
        },
        BlockStatus::Broken {
            reason: BrokenReason::UnresolvedName {
                name: "incme".into(),
                suggestion: Some("income".into()),
            },
        },
    ]
}

// ---------------------------------------------------------------------------
// §4 diagnostics
// ---------------------------------------------------------------------------

pub fn severities() -> Vec<Severity> {
    vec![
        Severity::Error,
        Severity::Warning,
        Severity::Note,
        Severity::Help,
    ]
}

pub fn confidences() -> Vec<Confidence> {
    vec![
        Confidence::Exact,
        Confidence::Probable,
        Confidence::Speculative,
    ]
}

pub fn suggestion_kinds() -> Vec<SuggestionKind> {
    use SuggestionKind::*;
    vec![
        Rename,
        InsertOption,
        RemoveOption,
        Rewrite,
        InsertLine,
        ChangePath,
        Explain,
    ]
}

pub fn related() -> Related {
    Related {
        span: span(),
        file: Some(path()),
        message: "first defined here".into(),
    }
}

pub fn suggestion() -> Suggestion {
    Suggestion {
        label: "Did you mean `income`?".into(),
        kind: SuggestionKind::Rename,
        edits: vec![edit()],
    }
}

pub fn diagnostic() -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        code: "STATA0111".into(),
        stata_rc: Some(111),
        message: "variable incme not found".into(),
        file: Some(path()),
        span: Some(span()),
        offending_token: Some("incme".into()),
        block: Some(BlockId(3)),
        related: vec![related()],
        suggestions: vec![suggestion()],
        notes: vec!["r(111);".into()],
        confidence: Confidence::Probable,
    }
}

// ---------------------------------------------------------------------------
// §5 results
// ---------------------------------------------------------------------------

pub fn asset_ref() -> AssetRef {
    AssetRef {
        path: "result/7/41/raw".into(),
        mime: "text/plain; charset=utf-8".into(),
        bytes: 8192,
    }
}

pub fn raw_ref() -> RawRef {
    RawRef {
        bytes: 12_345,
        lines: 210,
        head: "      Source |       SS           df       MS\n".into(),
        truncated: true,
        asset: asset_ref(),
    }
}

pub fn layout_hint() -> LayoutHint {
    LayoutHint {
        rows: 12,
        cols: 7,
        est_px: 384,
    }
}

pub fn card_actions() -> Vec<CardAction> {
    vec![
        CardAction::RawOutput,
        CardAction::CopyTable,
        CardAction::Export {
            formats: vec!["csv".into(), "tex".into(), "md".into()],
        },
        CardAction::HideOutput,
        CardAction::PlotCoefficients,
        CardAction::RunMargins,
        CardAction::CompareModel {
            with: vec![ResultId(39), ResultId(40)],
        },
        CardAction::Diagnostics,
        CardAction::AiExplain,
        CardAction::AiCheckModel,
        CardAction::AiSuggestNext,
    ]
}

pub fn style_ids() -> Vec<StyleId> {
    use StyleId::*;
    vec![
        Text,
        Input,
        Result,
        Error,
        ErrorToken,
        Hilite,
        Comment,
        Heading,
        Rule,
        Link { target_index: 3 },
    ]
}

pub fn styled_runs() -> Vec<StyledRun> {
    vec![
        StyledRun {
            text: "        mpg |".into(),
            style: StyleId::Text,
        },
        StyledRun {
            text: "   -49.51222".into(),
            style: StyleId::Result,
        },
        StyledRun {
            text: "\n".into(),
            style: StyleId::Text,
        },
    ]
}

pub fn log_payload() -> LogPayload {
    LogPayload {
        runs: styled_runs(),
        lines: 1,
    }
}

pub fn scalar_values() -> Vec<ScalarValue> {
    vec![
        ScalarValue::Num {
            value: awkward_f64s()[0],
            display: ".3".into(),
        },
        ScalarValue::Str {
            value: "regress".into(),
        },
    ]
}

pub fn var_kinds() -> Vec<VarKind> {
    use VarKind::*;
    vec![Numeric, String, Labeled, Binary]
}

pub fn summarize_detail() -> SummarizeDetail {
    SummarizeDetail {
        skewness: 0.948_802_8,
        kurtosis: 3.975_005,
        variance: 33.472_04,
        percentiles: [12.0, 14.0, 14.0, 18.0, 20.0, 25.0, 29.0, 34.0, 41.0],
        smallest4: [12.0, 12.0, 14.0, 14.0],
        largest4: [34.0, 35.0, 35.0, 41.0],
        display_stats: [".9488028".into(), "3.975005".into(), "33.47204".into()],
        display_percentiles: [
            "12".into(),
            "14".into(),
            "14".into(),
            "18".into(),
            "20".into(),
            "25".into(),
            "29".into(),
            "34".into(),
            "41".into(),
        ],
        display_smallest4: ["12".into(), "12".into(), "14".into(), "14".into()],
        display_largest4: ["34".into(), "35".into(), "35".into(), "41".into()],
    }
}

pub fn summarize_payload() -> SummarizePayload {
    SummarizePayload {
        detail: true,
        weight: Some("[aw=pop]".into()),
        qualifier: Some("if income>0".into()),
        rows: vec![SummarizeRow {
            var: "mpg".into(),
            label: Some("Mileage (mpg)".into()),
            format: "%8.0gc".into(),
            obs: 74,
            missing: 0,
            mean: 21.297_297_297_297_3,
            sd: 5.785_503_209_735_141,
            min: 12.0,
            max: 41.0,
            sum: 1576.0,
            display: SummarizeDisplay {
                obs: "74".into(),
                mean: "21.2973".into(),
                sd: "5.785503".into(),
                min: "12".into(),
                max: "41".into(),
            },
            detail: Some(summarize_detail()),
            var_kind: VarKind::Numeric,
            sparkline: Some(vec![
                1, 4, 9, 12, 14, 11, 8, 6, 4, 3, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ]),
        }],
    }
}

pub fn cell_stats() -> Vec<CellStat> {
    use CellStat::*;
    vec![Freq, RowPct, ColPct, CellPct, Expected]
}

pub fn tabulate_payload() -> TabulatePayload {
    TabulatePayload {
        row_var: "foreign".into(),
        col_var: Some("rep78".into()),
        row_label: Some("Car origin".into()),
        col_label: None,
        row_keys: vec![
            (0.0, Some("Domestic".into())),
            (1.0, Some("Foreign".into())),
        ],
        col_keys: vec![(1.0, None), (2.0, None)],
        counts: vec![2, 8, 0, 0],
        row_totals: vec![10, 0],
        col_totals: vec![2, 8],
        total: 10,
        requested: vec![CellStat::Freq, CellStat::RowPct],
        tests: vec![AssocTest {
            name: "Pearson chi2(4)".into(),
            stat: 12.126_1,
            df: Some(4.0),
            p: 0.016_4,
            display: "12.1261".into(),
        }],
        truncated: Some(Truncation {
            shown_cells: 2000,
            total_cells: 5_001,
        }),
    }
}

pub fn term() -> Term {
    Term {
        eq: 0,
        name: "1b.foreign".into(),
        display: "Domestic".into(),
        b: -49.512_222_222_222_22,
        se: 7.281_2,
        t: -6.80,
        p: 0.000,
        ci_lo: -64.032_1,
        ci_hi: -34.992_3,
        display_num: [
            "-49.51222".into(),
            "7.28120".into(),
            "-6.80".into(),
            "0.000".into(),
            "-64.0321".into(),
            "-34.9923".into(),
        ],
        beta: Some(-0.532_1),
        omitted: false,
        base: true,
        empty: false,
    }
}

pub fn anova_table() -> AnovaTable {
    AnovaTable {
        mss: 1591.99516,
        df_m: 1.0,
        ms_m: 1591.99516,
        rss: 852.81538,
        df_r: 72.0,
        ms_r: 11.8446581,
        tss: 2444.81054,
        df_t: 73.0,
        ms_t: 33.4905553,
        display: [
            "1591.99516".into(),
            "1".into(),
            "1591.99516".into(),
            "852.815384".into(),
            "72".into(),
            "11.8446581".into(),
            "2444.81054".into(),
            "73".into(),
            "33.4905553".into(),
        ],
    }
}

pub fn estimation_payload() -> EstimationPayload {
    EstimationPayload {
        cmd: "regress".into(),
        cmdline: "regress mpg weight foreign".into(),
        depvar: "mpg".into(),
        n: 74,
        rank: 3,
        eq_names: vec![String::new()],
        terms: vec![term()],
        scalars: vec![("N".into(), 74.0), ("r2".into(), 0.663_2)],
        macros: vec![
            ("cmd".into(), "regress".into()),
            ("depvar".into(), "mpg".into()),
        ],
        anova: Some(anova_table()),
        vce: "cluster rep78".into(),
        ci_level: 95.0,
        estimates_name: Some("m1".into()),
        sample_hash: 0xDEAD_BEEF_CAFE_F00D,
        diagnostics: vec![ModelFlag {
            code: "E014".into(),
            message: "1 variable omitted because of collinearity".into(),
            vars: vec!["length".into()],
            severity: Severity::Warning,
        }],
        cond_number: Some(1.2e7),
    }
}

pub fn graph_ref() -> GraphRef {
    GraphRef {
        name: "Graph".into(),
        asset: AssetRef {
            path: "graph/7/41.svg".into(),
            mime: "image/svg+xml".into(),
            bytes: 1_512_000,
        },
        intrinsic_pt: (468.0, 324.0),
        scheme: "stratum".into(),
        source_cmd: "twoway scatter mpg weight".into(),
    }
}

pub fn cells() -> Vec<Cell> {
    vec![
        Cell::Num {
            value: awkward_f64s()[1],
            display: "8.99e+307".into(),
        },
        Cell::Str {
            value: "n/a".into(),
        },
    ]
}

pub fn aligns() -> Vec<Align> {
    vec![Align::Left, Align::Right, Align::Decimal]
}

pub fn generic_table() -> GenericTable {
    GenericTable {
        title: Some("e(V)".into()),
        colnames: vec!["mpg".into(), "weight".into()],
        rownames: vec!["mpg".into(), "weight".into()],
        cells: vec![
            Some(cells()[0].clone()),
            None,
            Some(cells()[1].clone()),
            None,
        ],
        col_align: aligns(),
    }
}

pub fn data_change_summary() -> DataChangeSummary {
    DataChangeSummary {
        frame: "default".into(),
        obs_before: 74,
        obs_after: 74,
        vars_before: 12,
        vars_after: 13,
        created: vec!["lnwage".into()],
        modified: vec![],
        dropped: vec!["tmp".into()],
        renamed: vec![("rep78".into(), "repair".into())],
        notes: vec!["(1 missing value generated)".into()],
    }
}

pub fn result_payloads() -> Vec<ResultPayload> {
    vec![
        ResultPayload::Log(log_payload()),
        ResultPayload::Summarize(summarize_payload()),
        ResultPayload::Tabulate(tabulate_payload()),
        ResultPayload::Estimation(estimation_payload()),
        ResultPayload::Graph(graph_ref()),
        ResultPayload::Table(generic_table()),
        ResultPayload::Scalars {
            values: vec![
                ("r(N)".into(), scalar_values()[0].clone()),
                ("r(cmd)".into(), scalar_values()[1].clone()),
            ],
        },
        ResultPayload::DataChanged(data_change_summary()),
        ResultPayload::Error(diagnostic()),
        ResultPayload::Unknown,
    ]
}

pub fn result_envelope() -> ResultEnvelope {
    ResultEnvelope {
        result: ResultId(41),
        revision: 2,
        exec: ExecutionId(41),
        block: Some(BlockId(3)),
        dataset_state: DatasetStateId(17),
        code_hash: code_hash(),
        cmdline: "regress mpg weight foreign".into(),
        started_at_ms: 1_755_800_000_123,
        duration_us: 80_412,
        rc: 0,
        payloads: result_payloads(),
        raw: raw_ref(),
        layout_hint: layout_hint(),
        actions: card_actions(),
    }
}

// ---------------------------------------------------------------------------
// §6 execution
// ---------------------------------------------------------------------------

pub fn run_intents() -> Vec<RunIntent> {
    vec![
        RunIntent::CurrentBlock {
            doc: DocumentId(1),
            cursor: 120,
        },
        RunIntent::RunAndAdvance {
            doc: DocumentId(1),
            cursor: 120,
        },
        RunIntent::Selection {
            doc: DocumentId(1),
            span: span(),
        },
        RunIntent::FromHere {
            doc: DocumentId(1),
            block: BlockId(3),
            scope: ForwardScope::Dependents,
        },
        RunIntent::EverythingAbove {
            doc: DocumentId(1),
            block: BlockId(3),
        },
        RunIntent::ToCursor {
            doc: DocumentId(1),
            cursor: 400,
        },
        RunIntent::CurrentSection {
            doc: DocumentId(1),
            cursor: 400,
        },
        RunIntent::AllStale { doc: DocumentId(1) },
        RunIntent::WholeFile { doc: DocumentId(1) },
        RunIntent::CleanRun {
            entry: DocumentId(1),
            isolation: Isolation::Subprocess,
        },
        RunIntent::ProjectEntryPoint {
            project_root: Utf8PathBuf::from("/Users/ana/proj"),
            isolation: Isolation::InProcess,
        },
        RunIntent::CommandBar {
            text: "summarize mpg, detail".into(),
        },
    ]
}

pub fn plan_reasons() -> Vec<PlanReason> {
    use PlanReason::*;
    vec![Requested, DependencyOf, Stale, Prefix]
}

pub fn skip_reasons() -> Vec<SkipReason> {
    use SkipReason::*;
    vec![Unaffected, AlreadyCurrent, NotExecutable]
}

pub fn plan_item() -> PlanItem {
    PlanItem {
        block: BlockId(3),
        span: span(),
        code_hash: code_hash(),
        reason: PlanReason::DependencyOf,
    }
}

pub fn run_plan() -> RunPlan {
    RunPlan {
        run: RunId(9),
        items: vec![plan_item()],
        epoch_reset: true,
        clean_state: false,
        skipped: vec![(BlockId(4), SkipReason::Unaffected)],
        stale_upstream: vec![BlockId(1), BlockId(2)],
    }
}

pub fn exec_statuses() -> Vec<ExecStatus> {
    vec![
        ExecStatus::Queued,
        ExecStatus::Running,
        ExecStatus::Succeeded,
        ExecStatus::Failed {
            rc: 111,
            message: "variable incme not found".into(),
            span: Some(span()),
        },
        ExecStatus::Interrupted {
            rolled_back: true,
            at: None,
        },
        ExecStatus::Skipped {
            reason: SkipReason::AlreadyCurrent,
        },
    ]
}

pub fn exec_origins() -> Vec<ExecOrigin> {
    use ExecOrigin::*;
    vec![Editor, CommandBar, Selection, DoFile, CleanRun, Cli, Api]
}

pub fn execution_record() -> ExecutionRecord {
    ExecutionRecord {
        exec: ExecutionId(41),
        seq: 918,
        session: SessionId(7),
        epoch: SessionEpoch(2),
        run: RunId(9),
        block: BlockId::EPHEMERAL,
        doc: Some(DocumentId(1)),
        origin: ExecOrigin::CommandBar,
        code_hash: code_hash(),
        source: "regress mpg weight foreign".into(),
        input_state: StateId(90),
        output_state: StateId(91),
        input_dataset: DatasetStateId(17),
        output_dataset: DatasetStateId(17),
        result: Some(ResultId(41)),
        status: ExecStatus::Succeeded,
        started_at_ms: 1_755_800_000_123,
        duration_us: 80_412,
        stale_on_arrival: true,
        taint: taint(),
    }
}

// ---------------------------------------------------------------------------
// §8 data
// ---------------------------------------------------------------------------

pub fn storage_types() -> Vec<StorageType> {
    use StorageType::*;
    vec![Byte, Int, Long, Float, Double, Str { width: 244 }, StrL]
}

pub fn provenance() -> Provenance {
    Provenance {
        file: Some(path()),
        line: 42,
        col: 5,
        statement: "gen lnwage = ln(wage)".into(),
        exec: ExecutionId(41),
        confidence: Confidence::Exact,
    }
}

pub fn variable_info() -> VariableInfo {
    VariableInfo {
        idx: VarIdx(4),
        id: VarId(88),
        name: "lnwage".into(),
        ty: StorageType::Double,
        label: "log hourly wage".into(),
        format: "%9.0g".into(),
        value_label: Some("origin".into()),
        n_missing: 3,
        provenance: Some(provenance()),
    }
}

pub fn frame_info() -> FrameInfo {
    FrameInfo {
        name: "default".into(),
        n_obs: 74,
        n_vars: 12,
        sorted_by: vec!["foreign".into(), "mpg".into()],
        changed: true,
        state: DatasetStateId(17),
    }
}

pub fn quick_summary() -> QuickSummary {
    QuickSummary {
        var: "mpg".into(),
        state: DatasetStateId(17),
        n: 74,
        n_missing: 0,
        mean: Some(21.297_297_297_297_3),
        median: Some(20.0),
        sd: Some(5.785_503_209_735_141),
        min: Some(12.0),
        max: Some(41.0),
        display: vec![
            ("Mean".into(), "21.2973".into()),
            ("Std. dev.".into(), "5.785503".into()),
        ],
        sparkline: Some(vec![1, 4, 9, 12]),
        deferred: false,
    }
}

pub fn data_events() -> Vec<DataEvent> {
    vec![
        DataEvent::FrameChanged {
            frame: "default".into(),
            state: DatasetStateId(18),
        },
        DataEvent::VarAdded {
            frame: "default".into(),
            var: variable_info(),
        },
        DataEvent::VarDropped {
            frame: "default".into(),
            name: "tmp".into(),
        },
        DataEvent::VarModified {
            frame: "default".into(),
            name: "mpg".into(),
            idx: VarIdx(4),
        },
        DataEvent::VarRenamed {
            frame: "default".into(),
            from: "rep78".into(),
            to: "repair".into(),
        },
        DataEvent::TypeChanged {
            frame: "default".into(),
            name: "mpg".into(),
            from: StorageType::Int,
            to: StorageType::Double,
        },
        DataEvent::ObsCountChanged {
            frame: "default".into(),
            n_obs: 74,
        },
        DataEvent::SortChanged {
            frame: "default".into(),
            keys: vec!["mpg".into()],
        },
        DataEvent::FrameCreated {
            frame: "wide".into(),
        },
        DataEvent::FrameDropped {
            frame: "wide".into(),
        },
        DataEvent::CurrentFrame {
            frame: "default".into(),
        },
    ]
}

pub fn page_request() -> PageRequest {
    PageRequest {
        frame: "default".into(),
        state: DatasetStateId(17),
        row0: 10_000_000,
        nrows: 40,
        cols: vec![VarIdx(0), VarIdx(4)],
        order: Some(OrderId(11)),
        render: RenderMode::Display,
        seq: 88,
    }
}

// ---------------------------------------------------------------------------
// §9 repro / defuse / complete / capture
// ---------------------------------------------------------------------------

pub fn tris() -> Vec<Tri> {
    vec![Tri::Yes, Tri::No, Tri::Unknown]
}

pub fn finding() -> Finding {
    Finding {
        lint: "R001".into(),
        severity: Severity::Warning,
        title: "No seed is set".into(),
        message: "This do-file draws random numbers but never calls `set seed`.".into(),
        detail: Some("Add `set seed 20260821` before the first draw.".into()),
        evidence: vec![related()],
        block: Some(BlockId(3)),
        span: Some(span()),
        fix: Some(suggestion()),
        confidence: Confidence::Exact,
    }
}

pub fn repro_report() -> ReproReport {
    ReproReport {
        doc: DocumentId(1),
        file_hash: text_hash(),
        generated_at_ms: 1_755_800_000_123,
        runs_clean: Tri::Unknown,
        verified_by: None,
        verified_duration_us: None,
        seed_defined: Tri::No,
        inputs_resolved: Tri::Yes,
        no_hidden_deps: Tri::Unknown,
        findings: vec![finding()],
        suppressed: vec![("R001".into(), span())],
    }
}

pub fn site_kinds() -> Vec<SiteKind> {
    use SiteKind::*;
    vec![
        Generate, Replace, Egen, Rename, Merge, Import, Recode, Encode, Loop, Drop, Read,
    ]
}

pub fn site_ref() -> SiteRef {
    SiteRef {
        file: 0,
        line: 42,
        col: 5,
        span: span(),
        block: Some(BlockId(3)),
        statement: "gen lnwage = ln(wage)".into(),
        kind: SiteKind::Generate,
        confidence: Confidence::Probable,
    }
}

pub fn defuse_index() -> DefUseIndex {
    DefUseIndex {
        generation: 4,
        files: vec![path()],
        defs: vec![("lnwage".into(), vec![site_ref()])],
        uses: vec![("wage".into(), vec![site_ref()])],
        unresolved: vec![UnresolvedRef {
            pattern: "`v'_lag".into(),
            site: site_ref(),
        }],
    }
}

pub fn completion_env() -> CompletionEnv {
    CompletionEnv {
        generation: 12,
        frame: "default".into(),
        frames: vec!["default".into(), "wide".into()],
        varnames: vec!["mpg".into(), "weight".into(), "foreign".into()],
        var_total: 32_767,
        truncated: true,
        locals: vec!["controls".into()],
        globals: vec!["ROOT".into()],
        scalars: vec!["n".into()],
        matrices: vec!["b".into()],
        programs: vec!["mycmd".into()],
        e_names: vec!["e(N)".into(), "e(r2)".into()],
        r_names: vec!["r(mean)".into()],
        value_labels: vec!["origin".into()],
        stored_estimates: vec!["m1".into()],
        cwd: Utf8PathBuf::from("/Users/ana/proj"),
    }
}

pub fn capture_records() -> Vec<CaptureRecord> {
    vec![
        CaptureRecord::Scalar {
            name: "e(N)".into(),
            value: "74.000000000000000".into(),
        },
        CaptureRecord::Macro {
            name: "e(cmd)".into(),
            value: "regress".into(),
        },
        CaptureRecord::Matrix {
            name: "e(b)".into(),
            rows: 1,
            cols: 2,
            rownames: vec!["y1".into()],
            colnames: vec!["mpg".into(), "_cons".into()],
        },
        CaptureRecord::Cell {
            name: "e(V)[mpg,weight]".into(),
            value: "-.00000012345678901".into(),
        },
        CaptureRecord::Coef {
            name: "mpg".into(),
            value: "-49.512222222222221".into(),
        },
        CaptureRecord::Var {
            name: "mpg".into(),
            stype: "int".into(),
            format: "%8.0g".into(),
            vlabel: None,
        },
        CaptureRecord::Obs {
            var: "mpg".into(),
            i: 1,
            value: "22".into(),
        },
    ]
}

// ---------------------------------------------------------------------------
// §9.1 session / introspect
// ---------------------------------------------------------------------------

pub fn engine_healths() -> Vec<EngineHealth> {
    vec![
        EngineHealth::Starting,
        EngineHealth::Ready,
        EngineHealth::Busy {
            exec: ExecutionId(41),
        },
        EngineHealth::Crashed {
            signal: Some(-11),
            last_statement: Some("regress mpg weight".into()),
            log_tail: "…\n".into(),
        },
        EngineHealth::Stopped,
    ]
}

pub fn session_status() -> SessionStatus {
    SessionStatus {
        session: SessionId(7),
        epoch: SessionEpoch(2),
        health: EngineHealth::Busy {
            exec: ExecutionId(41),
        },
        current: Some(ExecutionId(41)),
        queued: 3,
        state: StateId(90),
        dataset_state: DatasetStateId(17),
        frame: "default".into(),
        n_obs: 74,
        n_vars: 12,
        mode: SessionMode::Interactive,
    }
}

pub fn session_config_wire() -> SessionConfigWire {
    SessionConfigWire {
        cwd: Some(Utf8PathBuf::from("/Users/ana/proj")),
        seed: Some(20_260_821),
        linesize: 80,
        level: 95.0,
        varabbrev: false,
        more: false,
        max_memory_bytes: Some(8 << 30),
        ado_personal: false,
        write_sandbox: Some(Utf8PathBuf::from("/Users/ana/proj/out")),
    }
}

pub fn session_snapshot() -> SessionSnapshot {
    SessionSnapshot {
        status: session_status(),
        docs: vec![block_map()],
        statuses: vec![(DocumentId(1), vec![(BlockId(3), BlockStatus::NeverRun)])],
        recent_results: vec![(BlockId(3), ResultId(41))],
        completion_env: completion_env(),
        log_lines: 5_000_000,
        from_seq: 918,
    }
}

pub fn log_hit() -> LogHit {
    LogHit {
        line: 4_000_000,
        col: 12,
        len: 5,
        preview: "…incme…".into(),
    }
}

pub fn log_search_opts() -> LogSearchOpts {
    LogSearchOpts {
        regex: true,
        case_sensitive: false,
        max_hits: 1000,
    }
}

pub fn macro_info() -> MacroInfo {
    MacroInfo {
        name: "controls".into(),
        scope: MacroScope::Local,
        value: "weight foreign".into(),
        truncated: false,
        defined_at: Some(ExecutionId(40)),
    }
}

pub fn matrix_meta() -> MatrixMeta {
    MatrixMeta {
        rows: 1,
        cols: 2,
        rownames: vec!["y1".into()],
        colnames: vec!["mpg".into(), "_cons".into()],
    }
}

pub fn stored_results_view() -> StoredResultsView {
    StoredResultsView {
        r_scalars: vec![("r(N)".into(), 74.0)],
        r_macros: vec![("r(cmd)".into(), "summarize".into())],
        r_matrices: vec![("r(table)".into(), matrix_meta())],
        e_scalars: vec![("e(N)".into(), 74.0)],
        e_macros: vec![("e(cmd)".into(), "regress".into())],
        e_matrices: vec![("e(b)".into(), matrix_meta())],
        s_macros: vec![("s(level)".into(), "95".into())],
        e_b_colnames: vec!["weight".into(), "_cons".into()],
    }
}

pub fn estimate_handle() -> EstimateHandle {
    EstimateHandle {
        name: "m1".into(),
        cmd: "regress".into(),
        depvar: "mpg".into(),
        n: 74,
        sample_hash: 0xDEAD_BEEF_CAFE_F00D,
        result: Some(ResultId(41)),
        stored_at: Some(ExecutionId(41)),
    }
}

pub fn dataset_meta() -> DatasetMeta {
    DatasetMeta {
        frame: "default".into(),
        state: DatasetStateId(17),
        n_obs: 74,
        n_vars: 12,
        sorted_by: vec!["foreign".into()],
        label: "1978 automobile data".into(),
        source_path: Some(path()),
        vars: vec![variable_info()],
        truncated: true,
    }
}

pub fn ai_context_snapshot() -> AiContextSnapshot {
    AiContextSnapshot {
        session: SessionId(7),
        generation: 12,
        dataset: Some(dataset_meta()),
        macros: vec![macro_info()],
        stored: Some(stored_results_view()),
        estimates: vec![estimate_handle()],
        recent_errors: vec![diagnostic()],
        recent_commands: vec!["regress mpg weight".into()],
        var_summaries: vec![quick_summary()],
    }
}

// ---------------------------------------------------------------------------
// §7 engine protocol
// ---------------------------------------------------------------------------

pub fn order_spec() -> OrderSpec {
    OrderSpec {
        keys: vec![(VarIdx(4), SortDir::Desc), (VarIdx(0), SortDir::Asc)],
        filter: Some("if income>0".into()),
        state: DatasetStateId(17),
    }
}

pub fn ai_context_want() -> AiContextWant {
    AiContextWant::DATASET_META | AiContextWant::STORED_RESULTS | AiContextWant::VAR_SUMMARIES
}

pub fn engine_requests() -> Vec<EngineRequest> {
    vec![
        EngineRequest::Hello {
            client: "stratum-desktop 0.1.0".into(),
            schema: STREAM_SCHEMA,
        },
        EngineRequest::SessionOpen {
            project_root: Utf8PathBuf::from("/Users/ana/proj"),
            mode: SessionMode::Clean,
            config: session_config_wire(),
        },
        EngineRequest::SessionClose {
            session: SessionId(7),
        },
        EngineRequest::Status {
            session: SessionId(7),
        },
        EngineRequest::DocOpen {
            session: SessionId(7),
            doc: DocumentId(1),
            path: Some(path()),
            text: "sysuse auto\n".into(),
        },
        EngineRequest::DocChange {
            session: SessionId(7),
            doc: DocumentId(1),
            version: 340,
            edits: vec![edit()],
        },
        EngineRequest::DocClose {
            session: SessionId(7),
            doc: DocumentId(1),
        },
        EngineRequest::ExecSubmit {
            session: SessionId(7),
            intent: RunIntent::WholeFile { doc: DocumentId(1) },
            inline_mode: InlineResultsMode::EditorRun,
        },
        EngineRequest::ExecCancel {
            session: SessionId(7),
            run: RunId(9),
            level: CancelLevel::Abort,
        },
        EngineRequest::Blocks {
            session: SessionId(7),
            doc: DocumentId(1),
        },
        EngineRequest::Statuses {
            session: SessionId(7),
            doc: DocumentId(1),
        },
        EngineRequest::Ledger {
            session: SessionId(7),
            from_seq: 918,
            limit: 200,
        },
        EngineRequest::Variables {
            session: SessionId(7),
            frame: "default".into(),
        },
        EngineRequest::VarStats {
            session: SessionId(7),
            frame: "default".into(),
            var: "mpg".into(),
        },
        EngineRequest::Frames {
            session: SessionId(7),
        },
        EngineRequest::DataPage {
            session: SessionId(7),
            request: page_request(),
        },
        EngineRequest::DataOrderSet {
            session: SessionId(7),
            frame: "default".into(),
            spec: order_spec(),
        },
        EngineRequest::DataOrderDrop {
            session: SessionId(7),
            order: OrderId(11),
        },
        EngineRequest::GraphRender {
            session: SessionId(7),
            result: ResultId(41),
            format: GraphFormat::Pdf,
            width_pt: 468.0,
        },
        EngineRequest::LogRange {
            session: SessionId(7),
            from_line: 0,
            to_line: 400,
        },
        EngineRequest::LogSearch {
            session: SessionId(7),
            query: "incme".into(),
            opts: log_search_opts(),
        },
        EngineRequest::ReproReport {
            session: SessionId(7),
            doc: DocumentId(1),
            verify: true,
        },
        EngineRequest::DefUse {
            session: SessionId(7),
        },
        EngineRequest::CompletionEnv {
            session: SessionId(7),
        },
        EngineRequest::CompletionEnvPage {
            session: SessionId(7),
            from: 2048,
            count: 512,
        },
        EngineRequest::AiContext {
            session: SessionId(7),
            want: ai_context_want(),
        },
        EngineRequest::Shutdown,
    ]
}

pub fn engine_errors() -> Vec<EngineError> {
    vec![
        EngineError::UnknownSession {
            session: SessionId(7),
        },
        EngineError::UnknownDocument { doc: DocumentId(1) },
        EngineError::BlockMismatch {
            doc: DocumentId(1),
            engine_version: 340,
            client_version: 341,
        },
        EngineError::PartialStatement { span: span() },
        EngineError::Busy,
        EngineError::SchemaMismatch {
            engine: 1,
            client: 2,
        },
        EngineError::Internal {
            message: "ledger write failed".into(),
        },
    ]
}

pub fn bulk_ref() -> BulkRef {
    BulkRef {
        segment: 3,
        offset: 1 << 20,
        len: 65_536,
        epoch: 2,
    }
}

pub fn engine_responses() -> Vec<EngineResponse> {
    vec![
        EngineResponse::Hello {
            engine: "stratum-engine 0.1.0".into(),
            schema: STREAM_SCHEMA,
            target: "aarch64-apple-darwin".into(),
        },
        EngineResponse::SessionOpened {
            session: SessionId(7),
            epoch: SessionEpoch(2),
        },
        EngineResponse::Ok,
        EngineResponse::Status {
            status: session_status(),
        },
        EngineResponse::BlockMap(block_map()),
        EngineResponse::Statuses {
            doc: DocumentId(1),
            statuses: vec![(BlockId(3), BlockStatus::NeverRun)],
        },
        EngineResponse::Submitted { plan: run_plan() },
        EngineResponse::Ledger {
            records: vec![execution_record()],
            next_seq: 919,
        },
        EngineResponse::Variables {
            frame: "default".into(),
            vars: vec![variable_info()],
        },
        EngineResponse::VarStats(quick_summary()),
        EngineResponse::Frames {
            frames: vec![frame_info()],
            current: "default".into(),
        },
        EngineResponse::Bulk { bulk: bulk_ref() },
        EngineResponse::LogRange {
            from_line: 0,
            runs: styled_runs(),
            line_starts: vec![0, 13, 25],
        },
        EngineResponse::LogSearch {
            hits: vec![log_hit()],
            total: 3,
        },
        EngineResponse::ReproReport(repro_report()),
        EngineResponse::DefUse(defuse_index()),
        EngineResponse::CompletionEnv(completion_env()),
        EngineResponse::DataOrder {
            order: OrderId(11),
            n_rows: 10_000_000,
            state: DatasetStateId(17),
        },
        EngineResponse::AiContext(ai_context_snapshot()),
        EngineResponse::Error(EngineError::Busy),
    ]
}

pub fn output_streams() -> Vec<OutputStream> {
    vec![
        OutputStream::Results,
        OutputStream::Error,
        OutputStream::Trace,
    ]
}

pub fn engine_events() -> Vec<EngineEvent> {
    vec![
        EngineEvent::RunStarted {
            seq: 900,
            schema: STREAM_SCHEMA,
            run: RunId(9),
            session: SessionId(7),
            stratum_version: "0.1.0".into(),
            source: Some(path()),
            clean_state: true,
            cwd: Utf8PathBuf::from("/Users/ana/proj"),
            started_at_ms: 1_755_800_000_000,
            seed: Some(20_260_821),
            plan_len: 14,
        },
        EngineEvent::BlockStarted {
            seq: 901,
            run: RunId(9),
            exec: ExecutionId(41),
            block: BlockId(3),
            doc: Some(DocumentId(1)),
            span: span(),
            code_hash: code_hash(),
            dataset_state_in: DatasetStateId(17),
            text: "regress mpg weight foreign".into(),
        },
        EngineEvent::Output {
            seq: 902,
            exec: ExecutionId(41),
            stream: OutputStream::Results,
            runs: styled_runs(),
        },
        EngineEvent::OutputTruncated {
            seq: 903,
            exec: ExecutionId(41),
            dropped_bytes: 12_582_912,
        },
        EngineEvent::Result {
            seq: 904,
            exec: ExecutionId(41),
            envelope: result_envelope(),
        },
        EngineEvent::Diagnostic {
            seq: 905,
            exec: Some(ExecutionId(41)),
            diagnostic: diagnostic(),
        },
        EngineEvent::Progress {
            seq: 906,
            exec: ExecutionId(41),
            done: 40_000,
            total: Some(1_000_000),
            label: "reading auto.dta".into(),
        },
        EngineEvent::StateChanged {
            seq: 907,
            exec: ExecutionId(41),
            dataset_state: DatasetStateId(18),
            state: StateId(91),
            frame: "default".into(),
            n_obs: 74,
            n_vars: 13,
            events: data_events(),
        },
        EngineEvent::BlockFinished {
            seq: 908,
            run: RunId(9),
            exec: ExecutionId(41),
            block: BlockId(3),
            result: Some(ResultId(41)),
            status: ExecStatus::Succeeded,
            rc: 0,
            duration_us: 80_412,
            dataset_state_out: DatasetStateId(18),
        },
        EngineEvent::StatusChanged {
            seq: 909,
            doc: DocumentId(1),
            changed: vec![(
                BlockId(3),
                BlockStatus::Current {
                    exec: ExecutionId(41),
                    dataset: DatasetStateId(18),
                    duration_us: 80_412,
                },
            )],
        },
        EngineEvent::BlockMapChanged {
            seq: 910,
            map: block_map(),
        },
        EngineEvent::RunFinished {
            seq: 911,
            run: RunId(9),
            rc: 0,
            blocks_run: 14,
            blocks_failed: 0,
            duration_us: 1_204_112,
            finished_at_ms: 1_755_800_001_204,
        },
        EngineEvent::CompletionEnvChanged {
            seq: 912,
            env: completion_env(),
        },
        EngineEvent::EngineHealth {
            seq: 913,
            health: EngineHealth::Ready,
        },
    ]
}
