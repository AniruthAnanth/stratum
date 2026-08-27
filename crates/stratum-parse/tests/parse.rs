//! Command parsing — design 02 §§6, 9, 10, and the golden error codes.

use stratum_parse::ast::{BlockCommand, Command, ForeachSource, ObsRef, Prefix, WeightKind};
use stratum_parse::cmdsig::{CmdFlags, CommandTable, SlotMask};
use stratum_parse::{
    all_commands, parse_command, parse_command_counted, parse_speculative, table, ParseMode,
};

fn known(src: &str) -> stratum_parse::ast::KnownCommand {
    let (stmt, diags) = parse_command(src, ParseMode::Execute);
    assert!(
        diags.iter().all(|d| d.stata_rc.is_none()),
        "`{src}` produced errors: {diags:#?}"
    );
    match stmt.cmd {
        Command::Known(k) => *k,
        other => panic!("`{src}`: expected a known command, got {other:?}"),
    }
}

fn errors(src: &str) -> Vec<(u32, String)> {
    let (_, diags) = parse_command(src, ParseMode::Execute);
    diags
        .into_iter()
        .filter_map(|d| d.stata_rc.map(|rc| (rc, d.message)))
        .collect()
}

// ─────────────────── the two tables must never disagree ─────────────────────

#[test]
fn generated_table_agrees_with_core() {
    // W04's `CommandTable::core()` is the provisional wave-1 table the SEGMENTER
    // resolves with; this crate's generated table is what the PARSER resolves
    // with. If the two disagree about `min_abbrev`, the gutter labels a region
    // with one command and the executor runs another — the exact failure mode
    // decision D3 (one declarative table) exists to prevent.
    let core = CommandTable::core();
    let full = table();
    for row in core.rows() {
        let id = full
            .canonical_id(row.canonical)
            .unwrap_or_else(|| panic!("`{}` is in core but not in commands.ron", row.canonical));
        let mine = full.get(id);
        assert_eq!(mine.canonical, row.canonical);
        assert_eq!(
            mine.min_abbrev, row.min_abbrev,
            "`{}`: min_abbrev drifted",
            row.canonical
        );
        assert_eq!(mine.tier, row.tier, "`{}`: tier drifted", row.canonical);
        assert!(
            mine.flags.contains(row.flags),
            "`{}`: the generated table dropped a flag core declares",
            row.canonical
        );
    }
    // The generated table is a strict superset and carries the real slot masks.
    assert!(full.rows().len() > core.rows().len());
    assert!(!full.is_provisional());
    assert!(full
        .rows()
        .iter()
        .any(|r| !r.slots.is_empty() && !r.options.is_empty()));
}

#[test]
fn every_v1_command_of_plan_section_1_has_a_signature() {
    // IMPLEMENTATION_PLAN §1's Pass-1 surface, word for word. A command the
    // executor is expected to run and the table has never heard of parses to
    // `Unknown` and fails with r(199) at run time, which is a silent scope cut.
    const PASS1: &[&str] = &[
        "use",
        "sysuse",
        "clear",
        "save",
        "describe",
        "list",
        "count",
        "generate",
        "replace",
        "drop",
        "keep",
        "rename",
        "sort",
        "gsort",
        "by",
        "bysort",
        "label",
        "format",
        "summarize",
        "tabulate",
        "correlate",
        "pwcorr",
        "ttest",
        "regress",
        "predict",
        "test",
        "testparm",
        "estimates",
        "display",
        "local",
        "global",
        "scalar",
        "matrix",
        "macro",
        "foreach",
        "forvalues",
        "while",
        "program",
        "syntax",
        "args",
        "capture",
        "quietly",
        "noisily",
        "version",
        "set",
        "histogram",
        "twoway",
        "scatter",
        "do",
        "run",
        "include",
    ];
    for name in PASS1 {
        assert!(
            table().canonical(name).is_some(),
            "`{name}` is in IMPLEMENTATION_PLAN §1 but not in commands.ron"
        );
    }
}

#[test]
fn slot_masks_are_coherent() {
    for sig in all_commands() {
        // A command that takes an `= exp` must take a varlist or a rest to put
        // the target in.
        if sig.slots.contains(SlotMask::ASSIGN) {
            assert!(
                sig.slots.intersects(
                    SlotMask::VARLIST
                        .union(SlotMask::NEWVARLIST)
                        .union(SlotMask::REST)
                ),
                "{}: ASSIGN with nowhere to assign",
                sig.canonical
            );
        }
        // Weights need a WEIGHT slot to arrive in, and vice versa.
        assert_eq!(
            sig.slots.contains(SlotMask::WEIGHT),
            !sig.weights.is_empty(),
            "{}: weight slot and weight mask disagree",
            sig.canonical
        );
        // Every estimation command must be byable-or-not consistently typed and
        // must have a varlist to estimate over.
        if sig.flags.contains(CmdFlags::ESTIMATION) {
            assert!(sig.slots.contains(SlotMask::VARLIST), "{}", sig.canonical);
        }
    }
}

// ───────────────────────── the universal grammar ────────────────────────────

#[test]
fn if_and_in_may_appear_in_either_order() {
    // [U] 11.1, verified. `count if foreign == 1 in 1/40` is in
    // tests/golden/stata18/semantics.log.
    for src in [
        "summarize price if rep78 == 3 in 1/50",
        "summarize price in 1/50 if rep78 == 3",
    ] {
        let k = known(src);
        assert!(k.slots.if_.is_some(), "`{src}`");
        let r = k.slots.in_.expect("in");
        assert_eq!(r.from, ObsRef::Num(1));
        assert_eq!(r.to, ObsRef::Num(50));
    }
}

#[test]
fn options_need_not_be_contiguous_and_may_be_re_entered() {
    // [U] 11.1.7's technical note: a SECOND comma returns to the command line.
    let k = known("summarize price mpg, detail, if foreign == 1, noformat");
    assert!(k.slots.if_.is_some(), "the re-entered `if` must be found");
    let names: Vec<_> = k
        .slots
        .options
        .items
        .iter()
        .map(|o| (o.canonical, o.negated))
        .collect();
    assert_eq!(names, [(Some("detail"), false), (Some("format"), true)]);
}

#[test]
fn in_range_endpoints_follow_u_11_1_4() {
    let k = known("list in -5/l");
    let r = k.slots.in_.expect("in");
    assert_eq!(r.from, ObsRef::Num(-5));
    assert_eq!(r.to, ObsRef::Last);
    let k = known("list in f/10");
    let r = k.slots.in_.expect("in");
    assert_eq!(r.from, ObsRef::First);
    assert_eq!(r.to, ObsRef::Num(10));
}

#[test]
fn weights_parse_and_are_checked_against_the_signature() {
    let k = known("summarize price [aweight = weight]");
    let w = k.slots.weight.expect("weight");
    assert_eq!(w.kind, WeightKind::AWeight);
    // `summarize` does not take pweights.
    let errs = errors("summarize price [pweight = weight]");
    assert!(errs.iter().any(|(rc, _)| *rc == 101), "{errs:?}");
}

#[test]
fn a_subscript_is_not_a_weight() {
    // The bracket that follows a name with NO whitespace is an observation
    // subscript; the one that stands alone is a weight clause.
    let k = known("generate lag = price[_n-1]");
    assert!(k.slots.weight.is_none());
    assert!(k.slots.assign.is_some());
}

#[test]
fn prefixes_chain() {
    let (stmt, diags) = parse_command(
        "by rep78, sort: quietly summarize price",
        ParseMode::Execute,
    );
    assert!(diags.iter().all(|d| d.stata_rc.is_none()), "{diags:#?}");
    assert_eq!(stmt.prefixes.len(), 2);
    let Prefix::By(by) = &stmt.prefixes[0] else {
        panic!("expected a by prefix")
    };
    assert!(by.sort);
    assert_eq!(by.group.items.len(), 1);
    assert!(matches!(stmt.prefixes[1], Prefix::Quietly { .. }));
    assert!(matches!(stmt.cmd, Command::Known(_)));
}

#[test]
fn bysort_separates_grouping_from_sort_only_keys() {
    let (stmt, _) = parse_command("bysort foreign (price): gen rank = _n", ParseMode::Execute);
    let Prefix::By(by) = &stmt.prefixes[0] else {
        panic!("expected a by prefix")
    };
    assert!(by.sort);
    assert_eq!(by.group.items.len(), 1);
    assert_eq!(by.extra_sort.items.len(), 1);
}

#[test]
fn colon_optional_prefixes_work_both_ways() {
    for src in ["quietly summarize price", "quietly: summarize price"] {
        let (stmt, _) = parse_command(src, ParseMode::Execute);
        assert!(
            matches!(stmt.prefixes[0], Prefix::Quietly { .. }),
            "`{src}`"
        );
    }
    let (stmt, _) = parse_command("version 17: summarize price", ParseMode::Execute);
    let Prefix::Version { ver, .. } = &stmt.prefixes[0] else {
        panic!("expected a version prefix")
    };
    assert_eq!(ver, "17");
}

#[test]
fn a_bare_by_without_a_colon_is_not_a_prefix() {
    // Consuming the rest of the line as a grouping varlist would swallow the
    // real command.
    let (stmt, _) = parse_command("by rep78", ParseMode::Execute);
    assert!(stmt.prefixes.is_empty());
}

// ────────────────────────────── block commands ──────────────────────────────

#[test]
fn foreach_reads_all_six_sources() {
    let cases: &[(&str, fn(&ForeachSource) -> bool)] = &[
        ("foreach x in a b c {", |s| {
            matches!(s, ForeachSource::In(_))
        }),
        ("foreach x of local L {", |s| {
            matches!(s, ForeachSource::OfLocal(_))
        }),
        ("foreach x of global G {", |s| {
            matches!(s, ForeachSource::OfGlobal(_))
        }),
        ("foreach x of varlist a b {", |s| {
            matches!(s, ForeachSource::OfVarlist(_))
        }),
        ("foreach x of newlist n1 n2 {", |s| {
            matches!(s, ForeachSource::OfNewlist(_))
        }),
        ("foreach x of numlist 1/10 {", |s| {
            matches!(s, ForeachSource::OfNumlist(_))
        }),
    ];
    for (src, want) in cases {
        let (stmt, _) = parse_command(src, ParseMode::Execute);
        let Command::Block(b) = stmt.cmd else {
            panic!("`{src}`: expected a block")
        };
        let BlockCommand::Foreach {
            loopvar, source, ..
        } = *b
        else {
            panic!("`{src}`: expected foreach")
        };
        assert_eq!(loopvar, "x");
        assert!(want(&source), "`{src}`: wrong source {source:?}");
    }
}

#[test]
fn a_loop_body_is_a_span_never_a_parsed_tree() {
    // 02 §6.2: Stata re-expands the body per iteration; that is how `` `x' ``
    // picks up the new value and it is what makes `foreach` cheap.
    let src = "forvalues i = 1/3 { summarize price }";
    let (stmt, _) = parse_command(src, ParseMode::Execute);
    let Command::Block(b) = stmt.cmd else {
        panic!("expected a block")
    };
    let BlockCommand::Forvalues { range, body, .. } = *b else {
        panic!("expected forvalues")
    };
    assert_eq!((range.from, range.step, range.to), (1.0, Some(1.0), 3.0));
    assert_eq!(
        src[body.start as usize..body.end as usize].trim(),
        "summarize price"
    );
}

#[test]
fn if_else_chains_become_arms() {
    let src = "if x > 1 { di 1 } else if x > 0 { di 2 } else { di 3 }";
    let (stmt, _) = parse_command(src, ParseMode::Execute);
    let Command::Block(b) = stmt.cmd else {
        panic!("expected a block")
    };
    let BlockCommand::IfElse { arms } = *b else {
        panic!("expected if/else")
    };
    assert_eq!(arms.len(), 3);
    assert!(arms[0].0.is_some());
    assert!(arms[1].0.is_some());
    assert!(arms[2].0.is_none(), "the last arm is the bare `else`");
}

#[test]
fn braced_capture_quietly_noisily_are_blocks_not_prefixes() {
    for (src, ok) in [
        ("capture { di 1 }", 0),
        ("quietly { di 1 }", 1),
        ("noisily { di 1 }", 2),
    ] {
        let (stmt, _) = parse_command(src, ParseMode::Execute);
        assert!(stmt.prefixes.is_empty(), "`{src}` must not be a prefix");
        let Command::Block(b) = stmt.cmd else {
            panic!("`{src}`: expected a block")
        };
        let matched = matches!(
            (ok, b.as_ref()),
            (0, BlockCommand::Capture { .. })
                | (1, BlockCommand::Quietly { .. })
                | (2, BlockCommand::Noisily { .. })
        );
        assert!(matched, "`{src}`: wrong block {b:?}");
    }
}

#[test]
fn program_define_defines_but_program_drop_does_not() {
    for src in [
        "program myprog",
        "program define myprog",
        "program define myprog, rclass",
    ] {
        let (stmt, _) = parse_command(src, ParseMode::Execute);
        let Command::Block(b) = stmt.cmd else {
            panic!("`{src}`: expected a block")
        };
        let BlockCommand::Program { name, .. } = b.as_ref() else {
            panic!("`{src}`: expected a program definition")
        };
        assert_eq!(name, "myprog");
    }
    for src in ["program drop myprog", "program dir", "program list"] {
        let (stmt, _) = parse_command(src, ParseMode::Execute);
        assert!(
            matches!(stmt.cmd, Command::Known(_)),
            "`{src}` must NOT define a program"
        );
    }
}

#[test]
fn delimit_is_a_directive() {
    use stratum_proto::DirectiveKind;
    let (stmt, _) = parse_command("#delimit ;", ParseMode::Execute);
    assert_eq!(stmt.cmd, Command::Directive(DirectiveKind::DelimitSemi));
    let (stmt, _) = parse_command("#delimit cr", ParseMode::Execute);
    assert_eq!(stmt.cmd, Command::Directive(DirectiveKind::DelimitCr));
}

// ──────────────────────── the golden error surface ──────────────────────────

#[test]
fn unknown_options_match_the_golden_wording_and_code() {
    // tests/golden/stata18/errors.log, verbatim:
    //   summarize price, nosuchoption  ->  option nosuchoption not allowed / 198
    //   summarize price, detial        ->  option detial not allowed       / 198
    for (src, word) in [
        ("summarize price, nosuchoption", "nosuchoption"),
        ("summarize price, detial", "detial"),
    ] {
        let errs = errors(src);
        assert!(
            errs.contains(&(198, format!("option {word} not allowed"))),
            "`{src}` gave {errs:?}"
        );
    }
}

#[test]
fn an_unrecognised_command_parses_and_keeps_its_tail() {
    // Decision D7: `command foo is unrecognized  r(199)` is the RUNTIME's error.
    // The parser must preserve the line so the editor can still fold and
    // highlight the user's ado-file.
    let (stmt, diags) = parse_command("foo bar baz", ParseMode::Execute);
    assert!(diags.iter().all(|d| d.stata_rc.is_none()), "{diags:#?}");
    let Command::Unknown { name, rest, .. } = stmt.cmd else {
        panic!("expected Unknown")
    };
    assert_eq!(name, "foo");
    assert_eq!(rest.text, "bar baz");
}

#[test]
fn no_abbreviation_in_the_shipped_table_is_ambiguous() {
    // The stronger property, and the one that actually protects users: every
    // legal abbreviation of every shipped command resolves to exactly one
    // command. `CommandLookup::Ambiguous` — and the r(199) "did you mean" path
    // it feeds — is therefore unreachable with the current table, which is why
    // this asserts the invariant rather than staging a collision.
    use stratum_parse::CommandLookup;
    for sig in all_commands() {
        if sig.min_abbrev == 0 {
            continue;
        }
        assert!(sig.canonical.is_ascii(), "{}", sig.canonical);
        for len in sig.min_abbrev as usize..=sig.canonical.len() {
            let word = &sig.canonical[..len];
            let id = match table().resolve(word) {
                CommandLookup::Exact(id) | CommandLookup::Abbrev(id) => id,
                other => panic!(
                    "`{word}` (an abbreviation of `{}`) gave {other:?}",
                    sig.canonical
                ),
            };
            let hit = table().get(id);
            assert!(
                hit.canonical == sig.canonical || hit.canonical == word,
                "`{word}` abbreviates `{}` but resolved to `{}`",
                sig.canonical,
                hit.canonical
            );
        }
    }
}

// ───────────────────────── speculative parsing ──────────────────────────────

#[test]
fn speculative_parsing_sees_through_a_macro_line() {
    // The editor's path: nothing has been expanded, and the parse must still
    // produce structure. 02 §10.
    let s = parse_speculative("summarize `varlist' if `cond', detail");
    assert!(matches!(s.stmt.cmd, Command::Known(_)));
    assert!(s.has_holes, "the macro references must survive as holes");
    assert!(
        s.diags.iter().all(|d| d.stata_rc.is_none()),
        "speculative mode must not raise r(198): {:#?}",
        s.diags
    );
}

#[test]
fn an_unexpanded_macro_in_execute_mode_is_an_error() {
    // The executor's text HAS been expanded. A leftover reference means
    // expansion is broken, and saying so beats parsing a hole into a tree that
    // is about to run.
    let errs = errors("generate x = `y'");
    assert!(errs.iter().any(|(rc, _)| *rc == 198), "{errs:?}");
}

// ─────────────────────────── ADR-017 counters ───────────────────────────────

#[test]
fn parsing_is_linear_in_the_number_of_tokens() {
    // ADR-017: assert a COUNTER, not a duration. The slot splitter finds
    // qualifiers by scanning forward, and the failure mode of that shape is a
    // rescan per option turning quadratic on a long line. Comparing the
    // reads-per-token ratio at two sizes is what catches it; a wall clock on a
    // busy laptop would not.
    let ratio = |n: usize| {
        let opts = (0..n).map(|_| "detail").collect::<Vec<_>>().join(" ");
        let src = format!("summarize price mpg weight if foreign == 1 in 1/40, {opts}");
        let ntok = stratum_parse::tokens(&src, stratum_parse::LexMode::Expanded).len() as f64;
        let (_, _, reads) = parse_command_counted(&src, ParseMode::Execute);
        f64::from(reads) / ntok
    };
    let small = ratio(8);
    let large = ratio(128);
    assert!(
        large <= small * 1.5,
        "reads/token grew from {small:.2} to {large:.2}: the splitter went superlinear"
    );
    // And the absolute cost stays modest on a realistic line.
    let src = "summarize price mpg if foreign == 1 in 1/40, detail";
    let ntok = stratum_parse::tokens(src, stratum_parse::LexMode::Expanded).len() as u32;
    let (_, _, reads) = parse_command_counted(src, ParseMode::Execute);
    assert!(reads <= ntok * 12, "{reads} reads for {ntok} tokens");
}

#[test]
fn parsing_is_pure() {
    let src = "bysort foreign (price): egen rank = rank(price) if !missing(price), unique";
    let a = parse_command(src, ParseMode::Execute);
    let b = parse_command(src, ParseMode::Execute);
    assert_eq!(a.0, b.0);
    assert_eq!(a.1, b.1);
}

#[test]
fn a_raw_tail_command_keeps_its_equals_sign() {
    // REGRESSION. `local x = 1` is legal Stata. Reading the `=` as the universal
    // grammar's ASSIGN slot — which `local` does not declare — rejected the line
    // with r(198) and dropped the value. A token is a qualifier only if the
    // COMMAND accepts that slot; `SlotMask::REST` means "this tail belongs to
    // the command's own mini-parser".
    for src in [
        "local x = 1",
        "global g = 2 + 2",
        "scalar s = 3",
        "matrix M = (1,2\\3,4)",
        "label variable price \"Price, in dollars\"",
        "macro drop _all",
        "set seed 12345",
    ] {
        let k = known(src);
        assert!(k.slots.assign.is_none(), "`{src}`: the tail must stay raw");
        assert!(
            k.slots.rest.is_some(),
            "`{src}`: the tail must be preserved"
        );
    }
    // The command that DOES declare the slot still gets it.
    assert!(known("generate x = 1").slots.assign.is_some());
    assert!(known("replace x = 1 if y > 0").slots.assign.is_some());
}

#[test]
fn every_golden_do_file_parses_without_panicking() {
    // The four committed StataMP 18.5 capture scripts are ~250 real command
    // lines of the Pass-1 surface. Parsing them is a corpus test that needs no
    // Stata installed (R4), and it is what caught `local x = 1`.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/stata18");
    let mut regions = 0usize;
    let mut known_cmds = 0usize;
    for name in [
        "core_surface.do",
        "semantics.do",
        "errors.do",
        "gformat.do",
        "extended_surface.do",
    ] {
        let Ok(src) = std::fs::read_to_string(root.join(name)) else {
            continue;
        };
        for r in &stratum_parse::segment(&src).regions {
            let text = &src[r.span.start as usize..r.span.end as usize];
            if text.trim().is_empty() {
                continue;
            }
            let s = parse_speculative(text);
            regions += 1;
            if matches!(s.stmt.cmd, Command::Known(_) | Command::Block(_)) {
                known_cmds += 1;
            }
        }
    }
    assert!(regions > 150, "the golden corpus shrank: {regions} regions");
    let rate = known_cmds as f64 / regions as f64;
    assert!(
        rate > 0.80,
        "only {known_cmds} of {regions} golden lines resolved to a command"
    );
}

#[test]
fn the_whole_pipeline_runs_in_order() {
    // Design 02 §1: Region -> expand -> lex -> parse. This is the sequence
    // `stratum-runtime` drives per logical line, and the ordering is the thing
    // this crate exists to model — so it is asserted end to end rather than only
    // in its two halves.
    use stratum_parse::macros::{expand, MacroEnv, NoHost};
    let mut env = MacroEnv::new();
    env.set_local("v", "price mpg");
    env.set_local("opt", ", detail");
    env.set_global("cmd", "summarize");

    let e = expand("$cmd `v' if foreign == 1 `opt\'", &mut env, &mut NoHost).expect("expanded");
    assert_eq!(e.text, "summarize price mpg if foreign == 1 , detail");

    let (stmt, diags) = parse_command(&e.text, ParseMode::Execute);
    assert!(diags.iter().all(|d| d.stata_rc.is_none()), "{diags:#?}");
    let Command::Known(k) = stmt.cmd else {
        panic!("expected a known command")
    };
    assert_eq!(table().get(k.id).canonical, "summarize");
    assert_eq!(k.slots.varlist.expect("varlist").items.len(), 2);
    assert!(k.slots.if_.is_some());
    assert_eq!(k.slots.options.items[0].canonical, Some("detail"));

    // A span in the EXPANDED text composes back to the original line through
    // `Expansion::map` — spec §21's underline.
    let composed = e.map.span_to_source(k.name_span);
    assert_eq!(composed[0].start, 0, "the command word came from `$cmd`");
}

// ─── the fuzz properties, as proptests so they run on stable in CI ──────────

proptest::proptest! {
    /// `fuzz/fuzz_targets/fuzz_parse.rs`'s invariants, on stable.
    ///
    /// cargo-fuzz needs a nightly toolchain and CI is pinned to stable, so the
    /// properties the fuzz target asserts are ALSO asserted here — the same
    /// arrangement W04 used for `fuzz_segment`.
    #[test]
    fn parsing_arbitrary_text_never_panics_and_stays_pure(src in ".{0,120}") {
        let mut counts = [0usize; 2];
        for (slot, mode) in [ParseMode::Execute, ParseMode::Speculative].into_iter().enumerate() {
            let (stmt, diags) = parse_command(&src, mode);
            counts[slot] = diags.iter().filter(|d| d.stata_rc.is_some()).count();
            // Every span must be a valid slice of the input, or an underline
            // panics instead of highlighting (spec §21).
            for s in [stmt.span, stmt.src] {
                proptest::prop_assert!(s.start <= s.end && s.end as usize <= src.len());
                proptest::prop_assert!(src.is_char_boundary(s.start as usize));
                proptest::prop_assert!(src.is_char_boundary(s.end as usize));
            }
            let (again, diags2) = parse_command(&src, mode);
            proptest::prop_assert_eq!(stmt, again);
            proptest::prop_assert_eq!(diags, diags2);
        }
        // One grammar, two modes: the tolerant one suppresses findings the
        // editor cannot act on and invents none of its own.
        proptest::prop_assert!(counts[1] <= counts[0], "{:?}", counts);
    }

    /// `fuzz/fuzz_targets/fuzz_expand.rs`'s invariants, on stable.
    #[test]
    fn expanding_arbitrary_text_terminates_and_stays_in_bounds(src in ".{0,120}") {
        use stratum_parse::macros::{expand, MacroEnv, NoHost};
        let mut env = MacroEnv::new();
        env.set_local("a", "`a'`b'");
        env.set_local("b", "$a");
        env.set_global("a", "`a'");
        env.limits.max_depth = 24;
        env.limits.max_expanded_len = 1 << 16;
        let mut host = NoHost;
        if let Ok(e) = expand(&src, &mut env.clone(), &mut host) {
            let again = expand(&src, &mut env.clone(), &mut host).expect("pure");
            proptest::prop_assert_eq!(&e.text, &again.text);
            proptest::prop_assert!(e.text.len() <= env.limits.max_expanded_len as usize);
            proptest::prop_assert!(e.stats.max_depth <= env.limits.max_depth);
            for off in 0..=e.text.len() as u32 {
                proptest::prop_assert!(e.map.to_source(off) as usize <= src.len());
            }
        }
    }
}

// ───────────────────────────── the ado corpus ───────────────────────────────

/// Parse every `.ado` under `$STRATA_ADO_DIR`, asserting no panics and a bounded
/// unknown-command rate.
///
/// Not required in CI (the plan says so) and skipped when the variable is unset,
/// because R4 forbids any unit from depending on Stata being installed. Run it
/// with `STRATA_ADO_DIR=/Applications/Stata/ado/base cargo test -p stratum-parse
/// --test parse -- --ignored --nocapture`.
#[test]
#[ignore = "needs $STRATA_ADO_DIR; not required in CI (IMPLEMENTATION_PLAN W04b)"]
fn ado_corpus_parses_without_panicking() {
    let Ok(dir) = std::env::var("STRATA_ADO_DIR") else {
        return;
    };
    let mut files = 0usize;
    let mut regions = 0usize;
    let mut unknown = 0usize;
    let mut stack = vec![std::path::PathBuf::from(dir)];
    while let Some(p) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&p) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "ado") {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&path) else {
                continue;
            };
            files += 1;
            let seg = stratum_parse::segment(&src);
            for r in &seg.regions {
                let text = &src[r.span.start as usize..r.span.end as usize];
                // Speculative mode: an ado-file is full of unexpanded macros.
                let s = parse_speculative(text);
                regions += 1;
                if matches!(s.stmt.cmd, Command::Unknown { .. }) {
                    unknown += 1;
                }
            }
        }
    }
    assert!(files > 0, "$STRATA_ADO_DIR held no .ado files");
    // The rate stays in integers end to end. Two rules meet on this line and
    // both point the same way: C12 (ARCHITECTURE §8.7) keeps a float precision
    // spec out of a format string, and ADR-017 wants the gate to be a counter
    // rather than a rounded quantity. Cross-multiplying the bound instead of
    // dividing means no float rounding sits between the corpus and the
    // assertion, so the same corpus can never straddle the threshold twice.
    let denom = regions.max(1);
    let tenths = unknown * 1000 / denom; // exact tenths of a percent, truncated
    println!(
        "corpus: {files} files, {regions} regions, {unknown} unknown ({}.{} %)",
        tenths / 10,
        tenths % 10
    );
    // StataCorp's own library uses a great many commands this build has never
    // heard of, so the bound is loose on purpose — it is a smoke test for
    // "the table resolves the common surface", not a coverage target.
    assert!(
        unknown * 100 < denom * 55,
        "unknown-command rate {unknown}/{regions} exceeds 55 %"
    );
}
