//! W00's acceptance gate: every wire type survives both encodings, the schema is
//! what everyone checks against, `CompletionEnv` is actually bounded, and adding
//! a `ResultPayload` variant tomorrow does not break a reader of today's bytes.
//!
//! Both encodings are tested on every value because they are not
//! interchangeable: MessagePack is the desktop transport (§10) and JSON is
//! `--protocol json` (§7.1), and internally tagged enums — which is every enum
//! here — are exactly where a self-describing and a positional format diverge.

mod fixtures;

use std::fmt::Debug;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use stratum_proto::*;

/// Round-trip one value through framed-MessagePack's encoder (`to_vec_named`,
/// per §10) and through `serde_json` (per §7.1), asserting equality both times.
#[track_caller]
fn rt<T>(label: &str, v: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let mp = rmp_serde::to_vec_named(v).unwrap_or_else(|e| panic!("{label}: msgpack encode: {e}"));
    let back: T = rmp_serde::from_slice(&mp)
        .unwrap_or_else(|e| panic!("{label}: msgpack decode: {e} (bytes {})", mp.len()));
    assert_eq!(*v, back, "{label}: msgpack round-trip changed the value");

    let js = serde_json::to_string(v).unwrap_or_else(|e| panic!("{label}: json encode: {e}"));
    let back: T =
        serde_json::from_str(&js).unwrap_or_else(|e| panic!("{label}: json decode: {e}\n  {js}"));
    assert_eq!(*v, back, "{label}: json round-trip changed the value");
}

/// Every element of a variant list, labelled by index.
#[track_caller]
fn rt_all<T>(label: &str, vs: &[T])
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    assert!(!vs.is_empty(), "{label}: empty fixture list proves nothing");
    for (i, v) in vs.iter().enumerate() {
        rt(&format!("{label}[{i}]"), v);
    }
}

macro_rules! rt {
    ($($e:expr),+ $(,)?) => {$( rt(stringify!($e), &$e); )+};
}

// ---------------------------------------------------------------------------
// §15 — the number every consumer checks in `Hello` / `RunStarted`
// ---------------------------------------------------------------------------

#[test]
fn stream_schema_is_one() {
    assert_eq!(STREAM_SCHEMA, 1);
}

// ---------------------------------------------------------------------------
// §§1–9 round-trips
// ---------------------------------------------------------------------------

#[test]
fn ids_and_spans_round_trip() {
    let (s, ep, run, e, st, d, r, b, doc, fr, vid, vidx, sec, ord) = fixtures::ids();
    rt!(s, ep, run, e, st, d, r, b, doc, fr, vid, vidx, sec, ord);
    rt!(
        fixtures::code_hash(),
        fixtures::text_hash(),
        ColumnDigest(*b"0123456789abcdef"),
        fixtures::span(),
        fixtures::line_range(),
        TextEdit {
            span: fixtures::span(),
            text_index: 3
        },
        fixtures::edit(),
    );
}

/// A3: `EPHEMERAL` and `NONE` are different ids, and neither is "real".
#[test]
fn block_id_sentinels_are_distinct() {
    assert_ne!(BlockId::EPHEMERAL, BlockId::NONE);
    assert!(!BlockId::EPHEMERAL.is_real());
    assert!(!BlockId::NONE.is_real());
    assert!(BlockId(1).is_real());
    assert!(BlockId(u64::MAX - 1).is_real());
    // The trivia sentinel must survive the wire, or A3's whole point is lost the
    // first time a BlockMap crosses a process boundary.
    rt!(BlockId::EPHEMERAL, BlockId::NONE);
}

#[test]
fn ids_display_with_their_prefix() {
    assert_eq!(ExecutionId(41).to_string(), "E41");
    assert_eq!(ResultId(41).to_string(), "R41");
    assert_eq!(DatasetStateId(17).to_string(), "D17");
    assert_eq!(BlockId::NONE.to_string(), format!("B{}", u64::MAX));
    assert_eq!(SessionEpoch::PREFIX, "epoch");
}

#[test]
fn tokens_round_trip() {
    rt_all("TokenKind", &fixtures::token_kinds());
    rt!(fixtures::token(), fixtures::canon_token());
}

#[test]
fn blocks_round_trip() {
    rt_all("RegionKind", &fixtures::region_kinds());
    rt_all("BraceOpener", &fixtures::brace_openers());
    rt_all("EndBlockOpener", &fixtures::end_block_openers());
    rt_all("DirectiveKind", &fixtures::directive_kinds());
    rt_all("Unterminated", &fixtures::unterminateds());
    rt!(
        Delimiter::Cr,
        Delimiter::Semi,
        fixtures::region_summary(),
        fixtures::cell_marker(),
        fixtures::section_span(),
        fixtures::block_map(),
        fixtures::block(),
    );
}

#[test]
fn statuses_round_trip() {
    rt_all("BlockStatus", &fixtures::block_statuses());
    rt_all("StaleReason", &fixtures::stale_reasons());
    rt_all("BrokenReason", &fixtures::broken_reasons());
    rt_all("DepKey", &fixtures::dep_keys());
    rt!(fixtures::taint(), Taint::empty(), Taint::all());
}

#[test]
fn diagnostics_round_trip() {
    rt_all("Severity", &fixtures::severities());
    rt_all("Confidence", &fixtures::confidences());
    rt_all("SuggestionKind", &fixtures::suggestion_kinds());
    rt!(
        fixtures::related(),
        fixtures::suggestion(),
        fixtures::diagnostic()
    );
}

#[test]
fn results_round_trip() {
    rt_all("CardAction", &fixtures::card_actions());
    rt_all("StyleId", &fixtures::style_ids());
    rt_all("ScalarValue", &fixtures::scalar_values());
    rt_all("VarKind", &fixtures::var_kinds());
    rt_all("CellStat", &fixtures::cell_stats());
    rt_all("Cell", &fixtures::cells());
    rt_all("Align", &fixtures::aligns());
    rt_all("ResultPayload", &fixtures::result_payloads());
    rt!(
        fixtures::asset_ref(),
        fixtures::raw_ref(),
        fixtures::layout_hint(),
        fixtures::log_payload(),
        fixtures::styled_runs(),
        fixtures::summarize_payload(),
        fixtures::summarize_detail(),
        fixtures::tabulate_payload(),
        Truncation {
            shown_cells: 2000,
            total_cells: 5001
        },
        fixtures::term(),
        fixtures::anova_table(),
        fixtures::estimation_payload(),
        fixtures::graph_ref(),
        fixtures::generic_table(),
        fixtures::data_change_summary(),
        fixtures::result_envelope(),
    );
}

#[test]
fn execution_round_trips() {
    rt_all("RunIntent", &fixtures::run_intents());
    rt_all("PlanReason", &fixtures::plan_reasons());
    rt_all("SkipReason", &fixtures::skip_reasons());
    rt_all("ExecStatus", &fixtures::exec_statuses());
    rt_all("ExecOrigin", &fixtures::exec_origins());
    rt!(
        ForwardScope::Dependents,
        ForwardScope::AllBelow,
        Isolation::InProcess,
        Isolation::Subprocess,
        CancelLevel::Interrupt,
        CancelLevel::Abort,
        fixtures::plan_item(),
        fixtures::run_plan(),
        fixtures::execution_record(),
    );
}

#[test]
fn engine_protocol_round_trips() {
    rt_all("EngineRequest", &fixtures::engine_requests());
    rt_all("EngineResponse", &fixtures::engine_responses());
    rt_all("EngineEvent", &fixtures::engine_events());
    rt_all("EngineError", &fixtures::engine_errors());
    rt_all("EngineHealth", &fixtures::engine_healths());
    rt_all("OutputStream", &fixtures::output_streams());
    rt!(
        fixtures::ai_context_want(),
        AiContextWant::all(),
        GraphFormat::Svg,
        GraphFormat::Png,
        GraphFormat::Pdf,
        fixtures::order_spec(),
        SortDir::Asc,
        SortDir::Desc,
        SessionMode::Interactive,
        SessionMode::Clean,
        InlineResultsMode::Always,
        InlineResultsMode::EditorRun,
        InlineResultsMode::Compact,
        InlineResultsMode::Off,
        fixtures::bulk_ref(),
    );
}

#[test]
fn data_round_trips() {
    rt_all("StorageType", &fixtures::storage_types());
    rt_all("DataEvent", &fixtures::data_events());
    rt!(
        fixtures::variable_info(),
        fixtures::provenance(),
        fixtures::frame_info(),
        fixtures::quick_summary(),
        fixtures::page_request(),
        RenderMode::Display,
        RenderMode::Edit,
    );
}

#[test]
fn repro_defuse_complete_capture_round_trip() {
    rt_all("Tri", &fixtures::tris());
    rt_all("SiteKind", &fixtures::site_kinds());
    rt_all("CaptureRecord", &fixtures::capture_records());
    rt!(
        fixtures::finding(),
        fixtures::repro_report(),
        fixtures::site_ref(),
        UnresolvedRef {
            pattern: "`v'_lag".into(),
            site: fixtures::site_ref()
        },
        fixtures::defuse_index(),
        fixtures::completion_env(),
        CompletionEnv::default(),
    );
}

#[test]
fn session_and_introspect_round_trip() {
    rt!(
        fixtures::session_status(),
        fixtures::session_config_wire(),
        fixtures::session_snapshot(),
        fixtures::log_hit(),
        fixtures::log_search_opts(),
        fixtures::macro_info(),
        MacroScope::Local,
        MacroScope::Global,
        fixtures::matrix_meta(),
        fixtures::stored_results_view(),
        StoredResultsView::default(),
        fixtures::estimate_handle(),
        fixtures::dataset_meta(),
        DatasetMeta::default(),
        fixtures::ai_context_snapshot(),
        AiContextSnapshot::default(),
    );
}

// ---------------------------------------------------------------------------
// §10 — `to_vec_named` is not a preference
// ---------------------------------------------------------------------------

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Positional MessagePack is smaller and is exactly what §10 forbids: the field
/// names must be on the wire, because one day an old engine will talk to a new
/// desktop.
#[test]
fn msgpack_encodes_field_names() {
    let env = fixtures::result_envelope();
    let named = rmp_serde::to_vec_named(&env).unwrap();
    let positional = rmp_serde::to_vec(&env).unwrap();

    // Names chosen so none of them also occurs inside a *value* of this
    // fixture — `"result"` does, in `RawRef::asset.path`.
    for field in [
        &b"revision"[..],
        b"dataset_state",
        b"code_hash",
        b"cmdline",
        b"started_at_ms",
        b"duration_us",
        b"payloads",
        b"layout_hint",
        b"actions",
    ] {
        assert!(
            contains(&named, field),
            "to_vec_named dropped `{}`",
            String::from_utf8_lossy(field)
        );
        assert!(
            !contains(&positional, field),
            "to_vec is supposed to be positional, but `{}` appeared",
            String::from_utf8_lossy(field)
        );
    }
    assert!(named.len() > positional.len());
}

/// The concrete failure `to_vec_named` buys us out of: a field added under §15's
/// additive rule is readable from old bytes only when the names travel.
#[test]
fn named_encoding_survives_an_added_field() {
    #[derive(Serialize)]
    struct Old {
        a: u32,
        b: u32,
    }
    #[derive(Deserialize, Debug, PartialEq)]
    struct New {
        a: u32,
        #[serde(default)]
        c: u32,
        b: u32,
    }

    let old = Old { a: 1, b: 2 };

    let named = rmp_serde::to_vec_named(&old).unwrap();
    assert_eq!(
        rmp_serde::from_slice::<New>(&named).unwrap(),
        New { a: 1, c: 0, b: 2 }
    );

    let positional = rmp_serde::to_vec(&old).unwrap();
    assert!(
        rmp_serde::from_slice::<New>(&positional).is_err(),
        "positional encoding must NOT silently accept a struct whose shape moved"
    );
}

/// §7.1: `body` is the internally tagged enum verbatim — `{"req":"…"}`,
/// `{"resp":"…"}`, `{"event":"…"}` — with no `jsonrpc`, `method` or `params`
/// anywhere (A9).
#[test]
fn json_tags_are_the_method_names() {
    let js = serde_json::to_string(&EngineRequest::Shutdown).unwrap();
    assert_eq!(js, r#"{"req":"shutdown"}"#);

    let js = serde_json::to_string(&EngineResponse::Ok).unwrap();
    assert_eq!(js, r#"{"resp":"ok"}"#);

    let js = serde_json::to_string(&fixtures::engine_events()[1]).unwrap();
    assert!(js.starts_with(r#"{"event":"block_started","#), "{js}");

    for js in [
        serde_json::to_string(&fixtures::engine_requests()[1]).unwrap(),
        serde_json::to_string(&fixtures::engine_responses()[4]).unwrap(),
    ] {
        assert!(!js.contains("jsonrpc"), "{js}");
        assert!(!js.contains(r#""method""#), "{js}");
        assert!(!js.contains(r#""params""#), "{js}");
    }
}

/// `InlineResultsMode` is the one enum on kebab-case; getting it wrong silently
/// breaks the sidecar and the layout defaults, which spell it the same way.
#[test]
fn inline_results_mode_is_kebab_case() {
    assert_eq!(
        serde_json::to_string(&InlineResultsMode::EditorRun).unwrap(),
        r#""editor-run""#
    );
}

// ---------------------------------------------------------------------------
// §5.2 / §15 — forward compatibility
// ---------------------------------------------------------------------------

/// Tomorrow's reader: every variant of today's `ResultPayload`, plus the one a
/// later schema-1 release adds. If a payload written today cannot be read by
/// this, §15's additive-only promise is a fiction.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ResultPayloadNext {
    Log(LogPayload),
    Summarize(SummarizePayload),
    Tabulate(TabulatePayload),
    Estimation(Box<EstimationPayload>),
    Graph(GraphRef),
    Table(GenericTable),
    Scalars {
        values: Vec<(String, ScalarValue)>,
    },
    DataChanged(DataChangeSummary),
    Error(Box<Diagnostic>),
    Unknown,
    /// The addition.
    Margins {
        at: Vec<String>,
        terms: Vec<Term>,
    },
}

fn tag_of_json(js: &str) -> String {
    serde_json::from_str::<serde_json::Value>(js).unwrap()["kind"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn a_new_variant_does_not_break_old_payloads() {
    for (i, old) in fixtures::result_payloads().iter().enumerate() {
        let mp = rmp_serde::to_vec_named(old).unwrap();
        let next: ResultPayloadNext = rmp_serde::from_slice(&mp)
            .unwrap_or_else(|e| panic!("payload {i}: new reader rejected old msgpack: {e}"));
        let js = serde_json::to_string(old).unwrap();
        let next_js: ResultPayloadNext = serde_json::from_str(&js)
            .unwrap_or_else(|e| panic!("payload {i}: new reader rejected old json: {e}"));
        assert_eq!(next, next_js, "payload {i}: the two encodings disagreed");
        assert_eq!(
            tag_of_json(&js),
            tag_of_json(&serde_json::to_string(&next).unwrap()),
            "payload {i}: the new reader landed on a different variant"
        );
    }
}

#[test]
fn an_old_reader_rejects_a_new_variant_loudly() {
    // §7.1's rule for third parties: an unrecognised tag is a skip, and serde's
    // "unknown variant" error is what a reader keys that skip off. What must NOT
    // happen is a silent mis-parse into a neighbouring variant.
    let novel = ResultPayloadNext::Margins {
        at: vec!["mean".into()],
        terms: vec![fixtures::term()],
    };
    let js = serde_json::to_string(&novel).unwrap();
    let err = serde_json::from_str::<ResultPayload>(&js)
        .unwrap_err()
        .to_string();
    assert!(err.contains("margins"), "{err}");

    let mp = rmp_serde::to_vec_named(&novel).unwrap();
    assert!(rmp_serde::from_slice::<ResultPayload>(&mp).is_err());
}

// ---------------------------------------------------------------------------
// A11 — `CompletionEnv` is bounded, and the bound is this test
// ---------------------------------------------------------------------------

/// A name of exactly `len` bytes, distinct per `i`.
fn padded(prefix: &str, i: usize, len: usize) -> String {
    let mut s = format!("{prefix}{i}");
    assert!(s.len() <= len, "prefix+index already exceeds {len}");
    while s.len() < len {
        s.push('z');
    }
    s
}

fn env_at_caps(name_len: usize, list_len: usize, var_len: usize, cwd_len: usize) -> CompletionEnv {
    let names = |p: &str, n: usize, w: usize| (0..n).map(|i| padded(p, i, w)).collect::<Vec<_>>();
    CompletionEnv {
        generation: u64::MAX,
        frame: padded("f", 0, name_len),
        frames: names("fr", list_len, name_len),
        varnames: names("v", var_len, name_len),
        var_total: 32_767,
        truncated: true,
        locals: names("lo", list_len, name_len),
        globals: names("gl", list_len, name_len),
        scalars: names("sc", list_len, name_len),
        matrices: names("mx", list_len, name_len),
        programs: names("pr", list_len, name_len),
        e_names: names("en", list_len, name_len),
        r_names: names("rn", list_len, name_len),
        value_labels: names("vl", list_len, name_len),
        stored_estimates: names("se", list_len, name_len),
        cwd: padded("/", 0, cwd_len).into(),
    }
}

/// The ceiling is a guarantee, not an observation.
///
/// W00 reported that the count caps and `COMPLETION_ENV_MAX_BYTES` could not both
/// hold as independent declarations, and asked for a ruling. The ruling: they are
/// two bounds on one value and whichever binds first wins, enforced by
/// `enforce_bounds` rather than by hoping the caps happen to fit. So this test
/// hands `enforce_bounds` inputs far past every cap and asserts the ceiling holds
/// anyway — which is the property the broadcast channel actually depends on.
#[test]
fn enforce_bounds_holds_the_ceiling_against_adversarial_input() {
    // Every list at ten times its cap, every name the longest Stata allows, and
    // a pathological 4 KiB cwd. Unbounded, this encodes to roughly 2.5 MB.
    let mut env = env_at_caps(
        32,
        COMPLETION_ENV_MAX_OTHER * 10,
        COMPLETION_ENV_MAX_VARS * 10,
        4096,
    );
    let unbounded = rmp_serde::to_vec_named(&env).unwrap().len();
    assert!(
        unbounded > COMPLETION_ENV_MAX_BYTES * 10,
        "the adversarial fixture is meant to be enormous, but encoded to {unbounded} bytes"
    );

    env.enforce_bounds();

    let bytes = rmp_serde::to_vec_named(&env).unwrap().len();
    eprintln!(
        "adversarial CompletionEnv: {unbounded} bytes -> {bytes} after enforce_bounds \
         (ceiling {COMPLETION_ENV_MAX_BYTES})"
    );
    assert!(
        bytes <= COMPLETION_ENV_MAX_BYTES,
        "enforce_bounds left {bytes} bytes, over the {COMPLETION_ENV_MAX_BYTES} ceiling"
    );
    assert!(env.truncated, "shedding must be visible to the popup");
    assert_eq!(
        env.var_total, 32_767,
        "var_total reports the true count, unshed"
    );

    // Shedding is by value: the user's own macros survive, the ado index does not.
    assert!(
        !env.locals.is_empty(),
        "locals are shed last and must survive"
    );
    assert!(
        !env.globals.is_empty(),
        "globals are shed last and must survive"
    );
    assert!(env.programs.is_empty(), "programs are shed first");

    // It also has to survive the wire, not merely fit on it.
    let back: CompletionEnv =
        rmp_serde::from_slice(&rmp_serde::to_vec_named(&env).unwrap()).unwrap();
    assert_eq!(env, back);
}

/// `enforce_bounds` is idempotent, so a producer may call it defensively.
#[test]
fn enforce_bounds_is_idempotent() {
    let mut once = env_at_caps(
        32,
        COMPLETION_ENV_MAX_OTHER * 4,
        COMPLETION_ENV_MAX_VARS * 4,
        4096,
    );
    once.enforce_bounds();
    let mut twice = once.clone();
    twice.enforce_bounds();
    assert_eq!(once, twice);
}

/// A realistic session is nowhere near the ceiling, so nothing is shed and the
/// full variable list ships. This is the case that must not regress: bounding the
/// pathological dataset is worthless if it costs the common one its completions.
#[test]
fn a_realistic_completion_env_is_untouched() {
    // 400 variables, a few dozen of everything else, Stata-typical 12-byte names.
    let mut env = env_at_caps(12, 40, 400, 64);
    let before = env.clone();
    env.enforce_bounds();
    assert_eq!(
        env.varnames.len(),
        before.varnames.len(),
        "nothing should be shed"
    );
    assert_eq!(env.programs.len(), before.programs.len());

    let bytes = rmp_serde::to_vec_named(&env).unwrap().len();
    assert!(
        bytes <= COMPLETION_ENV_MAX_BYTES,
        "a realistic CompletionEnv is {bytes} bytes, over the {COMPLETION_ENV_MAX_BYTES} ceiling"
    );
}

/// `enforce_bounds` computes size analytically because `stratum-proto` does not
/// link a codec. That arithmetic is only safe if it never under-estimates the
/// real encoding — an under-estimate would let a payload past the ceiling.
#[test]
fn the_analytic_bound_never_under_estimates() {
    for (name_len, list_len, var_len, cwd_len) in [
        (4, 0, 0, 2), // empty lists, shortest name and cwd the helper can build
        (12, 40, 400, 64),
        (31, 15, 15, 2),   // fixstr/fixarray boundaries
        (32, 16, 16, 256), // str8/array16 boundaries
        (32, COMPLETION_ENV_MAX_OTHER, COMPLETION_ENV_MAX_VARS, 4096),
        (255, 20, 70_000, 65_536), // str16/array32 boundaries
    ] {
        let env = env_at_caps(name_len, list_len, var_len, cwd_len);
        let actual = rmp_serde::to_vec_named(&env).unwrap().len();
        let bound = env.encoded_len_upper_bound();
        assert!(
            bound >= actual,
            "analytic bound {bound} under-estimates the real {actual} encoding \
             (name_len={name_len}, list_len={list_len}, var_len={var_len}, cwd_len={cwd_len})"
        );
    }
}

// ---------------------------------------------------------------------------
// A12 — the one flattening function
// ---------------------------------------------------------------------------

#[test]
fn to_plain_concatenates_and_only_that() {
    let runs = fixtures::styled_runs();
    assert_eq!(styled::to_plain(&runs), "        mpg |   -49.51222\n");
    assert_eq!(styled::to_plain(&[]), "");
    // Styling must not be able to move a byte: the same text under a different
    // style flattens identically.
    let restyled: Vec<StyledRun> = runs
        .iter()
        .map(|r| StyledRun {
            text: r.text.clone(),
            style: StyleId::Error,
        })
        .collect();
    assert_eq!(styled::to_plain(&runs), styled::to_plain(&restyled));
}

// ---------------------------------------------------------------------------
// §15 — the same forward-compatibility claim, under proptest
// ---------------------------------------------------------------------------
//
// The fixture-driven test above covers each variant once, with values chosen to
// be awkward. This one covers the shapes a fixture cannot: empty vectors,
// 17-significant-digit f64s, names with the tag string inside them. Strategies
// randomize the fields that decide an encoding — floats, string content,
// collection lengths, `Option` inhabitation — and keep the rest of each payload
// at its fixture value, because a fully random `EstimationPayload` costs a page
// of strategy code to test the same three serde code paths.

use proptest::prelude::*;

fn arb_name() -> impl Strategy<Value = String> {
    // Deliberately includes the tag values (`log`, `error`, `unknown`, …) as
    // possible content, so a payload whose *data* looks like a tag is covered.
    prop_oneof![
        "[a-z_][a-z0-9_]{0,12}",
        Just("kind".to_owned()),
        Just("unknown".to_owned()),
        Just(String::new()),
        "\\PC{0,8}",
    ]
}

fn arb_f64() -> impl Strategy<Value = f64> {
    // Invariant M: no NaN and no infinity ever reaches a payload.
    any::<f64>().prop_filter("finite (invariant M)", |v| v.is_finite())
}

fn arb_style_id() -> impl Strategy<Value = StyleId> {
    prop_oneof![
        Just(StyleId::Text),
        Just(StyleId::Input),
        Just(StyleId::Result),
        Just(StyleId::Error),
        Just(StyleId::ErrorToken),
        Just(StyleId::Hilite),
        Just(StyleId::Comment),
        Just(StyleId::Heading),
        Just(StyleId::Rule),
        any::<u32>().prop_map(|target_index| StyleId::Link { target_index }),
    ]
}

fn arb_styled_runs() -> impl Strategy<Value = Vec<StyledRun>> {
    prop::collection::vec(
        (arb_name(), arb_style_id()).prop_map(|(text, style)| StyledRun { text, style }),
        0..6,
    )
}

fn arb_scalar_value() -> impl Strategy<Value = ScalarValue> {
    prop_oneof![
        (arb_f64(), arb_name()).prop_map(|(value, display)| ScalarValue::Num { value, display }),
        arb_name().prop_map(|value| ScalarValue::Str { value }),
    ]
}

fn arb_cell() -> impl Strategy<Value = Cell> {
    prop_oneof![
        (arb_f64(), arb_name()).prop_map(|(value, display)| Cell::Num { value, display }),
        arb_name().prop_map(|value| Cell::Str { value }),
    ]
}

fn arb_term() -> impl Strategy<Value = Term> {
    (
        prop::array::uniform6(arb_f64()),
        arb_name(),
        any::<bool>(),
        prop::option::of(arb_f64()),
    )
        .prop_map(|(nums, name, omitted, beta)| {
            let mut t = fixtures::term();
            t.b = nums[0];
            t.se = nums[1];
            t.t = nums[2];
            t.p = nums[3];
            t.ci_lo = nums[4];
            t.ci_hi = nums[5];
            t.name = name;
            t.omitted = omitted;
            t.beta = beta;
            t
        })
}

fn arb_payload() -> impl Strategy<Value = ResultPayload> {
    prop_oneof![
        (arb_styled_runs(), any::<u32>())
            .prop_map(|(runs, lines)| ResultPayload::Log(LogPayload { runs, lines })),
        (
            any::<bool>(),
            prop::option::of(arb_name()),
            prop::collection::vec(arb_f64(), 5..6)
        )
            .prop_map(|(detail, qualifier, nums)| {
                let mut p = fixtures::summarize_payload();
                p.detail = detail;
                p.qualifier = qualifier;
                for (row, n) in p.rows.iter_mut().zip(nums) {
                    row.mean = n;
                    row.sd = n;
                    row.sparkline = None;
                }
                ResultPayload::Summarize(p)
            }),
        (
            arb_name(),
            prop::option::of(arb_name()),
            prop::collection::vec(0u64..1_000_000, 0..8)
        )
            .prop_map(|(row_var, col_var, counts)| {
                let mut p = fixtures::tabulate_payload();
                p.row_var = row_var;
                p.col_var = col_var;
                p.total = counts.iter().copied().sum();
                p.counts = counts;
                p.truncated = None;
                ResultPayload::Tabulate(p)
            }),
        (
            arb_name(),
            arb_f64(),
            any::<u64>(),
            prop::collection::vec(arb_term(), 0..3)
        )
            .prop_map(|(cmd, ci_level, n, terms)| {
                let mut p = fixtures::estimation_payload();
                p.cmd = cmd;
                p.ci_level = ci_level;
                p.n = n;
                p.terms = terms;
                p.anova = None;
                ResultPayload::Estimation(p)
            }),
        (arb_name(), any::<f32>(), any::<f32>())
            .prop_filter("finite intrinsic size", |(_, w, h)| w.is_finite()
                && h.is_finite())
            .prop_map(|(name, w, h)| {
                let mut g = fixtures::graph_ref();
                g.name = name;
                g.intrinsic_pt = (w, h);
                ResultPayload::Graph(g)
            }),
        (
            prop::option::of(arb_name()),
            prop::collection::vec(arb_name(), 0..4),
            prop::collection::vec(prop::option::of(arb_cell()), 0..6),
        )
            .prop_map(
                |(title, colnames, cells)| ResultPayload::Table(GenericTable {
                    title,
                    colnames,
                    rownames: vec![],
                    cells,
                    col_align: vec![Align::Decimal],
                })
            ),
        prop::collection::vec((arb_name(), arb_scalar_value()), 0..6)
            .prop_map(|values| ResultPayload::Scalars { values }),
        (
            any::<u64>(),
            any::<u64>(),
            prop::collection::vec(arb_name(), 0..4)
        )
            .prop_map(|(obs_before, obs_after, created)| {
                let mut d = fixtures::data_change_summary();
                d.obs_before = obs_before;
                d.obs_after = obs_after;
                d.created = created;
                ResultPayload::DataChanged(d)
            }),
        (arb_name(), arb_name(), prop::option::of(any::<u32>())).prop_map(|(code, message, rc)| {
            let mut d = fixtures::diagnostic();
            d.code = code;
            d.message = message;
            d.stata_rc = rc;
            ResultPayload::Error(d)
        }),
        Just(ResultPayload::Unknown),
    ]
}

proptest! {
    // An integration test has no `lib.rs`/`main.rs` for proptest to hang a
    // regression file off, and a counterexample here is reproducible from the
    // printed seed anyway.
    #![proptest_config(ProptestConfig { failure_persistence: None, ..ProptestConfig::default() })]

    /// The §15 promise, as a property: for ANY payload this build can produce,
    /// bytes written today are readable by a reader that has gained a variant —
    /// in both encodings, landing on the same tag, with the value intact.
    #[test]
    fn prop_new_variant_reads_old_payloads(payload in arb_payload()) {
        let mp = rmp_serde::to_vec_named(&payload).unwrap();
        let js = serde_json::to_string(&payload).unwrap();

        // Today's reader must survive its own bytes first, or the property
        // below would be measuring the wrong thing.
        prop_assert_eq!(&payload, &rmp_serde::from_slice::<ResultPayload>(&mp).unwrap());
        prop_assert_eq!(&payload, &serde_json::from_str::<ResultPayload>(&js).unwrap());

        let from_mp: ResultPayloadNext = rmp_serde::from_slice(&mp)
            .map_err(|e| TestCaseError::fail(format!("new reader, old msgpack: {e}")))?;
        let from_js: ResultPayloadNext = serde_json::from_str(&js)
            .map_err(|e| TestCaseError::fail(format!("new reader, old json: {e}")))?;
        prop_assert_eq!(&from_mp, &from_js);
        prop_assert_eq!(
            tag_of_json(&js),
            tag_of_json(&serde_json::to_string(&from_mp).unwrap())
        );
    }
}
