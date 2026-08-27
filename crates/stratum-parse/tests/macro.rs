//! Macro expansion conformance — design 02 §4, every verified property.
//!
//! Design 02 §14 asks for "one `input ⇒ expected expansion` per line, run
//! against a mock `ExpandHost`". `docs/ownership.toml` gives W04b
//! `crates/stratum-parse/tests/macro.rs` as an exact path and gives
//! `tests/macro/**` to nobody, so the table lives in this file — same shape,
//! one row per line — rather than as a tracked file `cargo xtask ownership`
//! would call unowned. Reported upward; see `tests/expr.rs` for the same note.

use stratum_parse::macros::{expand, stringify_number, ExpandHost, MacroEnv};
use stratum_parse::{parse_command, ParseMode, StataError};

// ─────────────────────────── the mock ExpandHost ────────────────────────────

/// The runtime, reduced to the two things expansion asks it for.
///
/// `eval_expr_to_macro_text` really does go through this crate's own parser and
/// then through 02 §4.4's stringification rule, so the `=exp` rows below test
/// the pipeline rather than a lookup table.
#[derive(Default)]
struct MockHost {
    /// Every `=exp` and `:xmf` body the expander asked about, in order.
    pub calls: Vec<String>,
}

impl ExpandHost for MockHost {
    fn eval_expr_to_macro_text(&mut self, exp: &str) -> Result<String, StataError> {
        self.calls.push(format!("={exp}"));
        let text = format!("generate __t = {exp}");
        let (stmt, diags) = parse_command(&text, ParseMode::Execute);
        if let Some(d) = diags.iter().find(|d| d.stata_rc.is_some()) {
            return Err(StataError::new(
                d.stata_rc.unwrap_or(198),
                d.message.clone(),
            ));
        }
        let stratum_parse::ast::Command::Known(k) = stmt.cmd else {
            return Err(StataError::new(198, "invalid syntax"));
        };
        let e = k
            .slots
            .assign
            .ok_or_else(|| StataError::new(198, "invalid syntax"))?;
        Ok(match eval(&e)? {
            V::Num(n) => stringify_number(n),
            // `di `="ab"+"cd"'` gives `abcd not found  r(111)` [V]: the result is
            // inserted as a BARE literal with no quoting at all.
            V::Str(s) => s,
        })
    }

    fn eval_xmf(&mut self, body: &str) -> Result<String, StataError> {
        self.calls.push(format!(":{body}"));
        Err(StataError::new(198, format!("unsupported: {body}")))
    }
}

enum V {
    Num(f64),
    Str(String),
}

fn eval(e: &stratum_parse::ast::Expr) -> Result<V, StataError> {
    use stratum_parse::ast::{BinOp, Expr};
    Ok(match e {
        Expr::Num(v, _) => V::Num(*v),
        // ADR-005: one definition of the missing-value bit pattern, in core.
        Expr::Missing(k, _) => V::Num(stratum_core::missing::missing_f64(*k)),
        Expr::Str(s, _) => V::Str(s.clone()),
        Expr::Paren(i, _) => eval(i)?,
        Expr::Unary { op, rhs, .. } => {
            let V::Num(v) = eval(rhs)? else {
                return Err(StataError::new(109, "type mismatch"));
            };
            V::Num(match op {
                stratum_parse::ast::UnOp::Neg => -v,
                stratum_parse::ast::UnOp::Pos => v,
                stratum_parse::ast::UnOp::Not => f64::from(v == 0.0),
            })
        }
        Expr::Binary { op, lhs, rhs, .. } => match (eval(lhs)?, eval(rhs)?) {
            (V::Str(a), V::Str(b)) if *op == BinOp::Add => V::Str(format!("{a}{b}")),
            (V::Num(a), V::Num(b)) => V::Num(match op {
                BinOp::Pow => stratum_core::math::powf(a, b),
                BinOp::Div => a / b,
                BinOp::Mul => a * b,
                BinOp::Sub => a - b,
                BinOp::Add => a + b,
                BinOp::Eq => f64::from(a == b),
                BinOp::Ne => f64::from(a != b),
                BinOp::Gt => f64::from(a > b),
                BinOp::Lt => f64::from(a < b),
                BinOp::Ge => f64::from(a >= b),
                BinOp::Le => f64::from(a <= b),
                BinOp::And => f64::from(a != 0.0 && b != 0.0),
                BinOp::Or => f64::from(a != 0.0 || b != 0.0),
            }),
            _ => return Err(StataError::new(109, "type mismatch")),
        },
        other => return Err(StataError::new(198, format!("mock host: {other:?}"))),
    })
}

fn ex(env: &mut MacroEnv, src: &str) -> String {
    let mut host = MockHost::default();
    expand(src, env, &mut host)
        .expect("expansion succeeded")
        .text
}

// ─────────────────────────── the verified properties ────────────────────────

#[test]
fn innermost_first() {
    // 02 §4.2: the NAME is expanded before the lookup. `local A B` / `local B C`
    // / `di "``A''"` → `C` [V].
    let mut env = MacroEnv::new();
    env.set_local("A", "B");
    env.set_local("B", "C");
    assert_eq!(ex(&mut env, r#"di "``A''""#), r#"di "C""#);

    // `local w1 one` / `` `w`k'' `` with `k` = 1 → `one` [V].
    let mut env = MacroEnv::new();
    env.set_local("w1", "one");
    env.set_local("k", "1");
    assert_eq!(ex(&mut env, "`w`k''"), "one");
}

#[test]
fn undefined_expands_to_the_empty_string_silently() {
    let mut env = MacroEnv::new();
    assert_eq!(ex(&mut env, "`undefined'"), "");
    // `$notdefined|end` → `|end` [V]: the munch stops at `|`, the name is
    // undefined, and NOTHING is reported.
    assert_eq!(ex(&mut env, "$notdefined|end"), "|end");
    assert_eq!(ex(&mut env, "di `nope' done"), "di  done");
}

#[test]
fn adjacency_the_close_quote_is_a_hard_delimiter() {
    // `` `L1'x `` → `abcx`; `` `L1x' `` → the macro `L1x` [V].
    let mut env = MacroEnv::new();
    env.set_local("L1", "abc");
    env.set_local("L1x", "zzz");
    assert_eq!(ex(&mut env, "`L1'x"), "abcx");
    assert_eq!(ex(&mut env, "`L1x'"), "zzz");
}

#[test]
fn globals_are_maximal_munch() {
    // `$G1x` reads the name `G1x`, NOT `$G1` followed by `x` [V].
    // `${G1}x` is how the boundary is forced [V].
    let mut env = MacroEnv::new();
    env.set_global("G1", "part");
    env.set_global("G1x", "whole");
    assert_eq!(ex(&mut env, "$G1x"), "whole");
    assert_eq!(ex(&mut env, "${G1}x"), "partx");
    // A global may not begin with a digit ([U] 11.3), so `$5` is a literal.
    assert_eq!(ex(&mut env, "$5"), "$5");
    // tests/golden/stata18/semantics.log: `global g "world"` / `di "hello $g"`.
    let mut env = MacroEnv::new();
    env.set_global("g", "world");
    assert_eq!(ex(&mut env, r#"di "hello $g""#), r#"di "hello world""#);
}

#[test]
fn expansion_is_quote_blind() {
    // 02 §4.2: nothing protects a macro reference except `macval()`. This is why
    // `local q = `"embedded "quote""'` then `di "B13: `q'"` ERRORS in Stata — the
    // substituted text re-tokenizes.
    let mut env = MacroEnv::new();
    env.set_local("b", "some text");
    assert_eq!(ex(&mut env, r#"di "`b'""#), r#"di "some text""#);
    env.set_local("q", r#"embedded "quote""#);
    assert_eq!(
        ex(&mut env, r#"di "B13: `q'""#),
        r#"di "B13: embedded "quote"""#,
        "the expander must NOT be quote-aware"
    );
}

#[test]
fn compound_quotes_survive_expansion_and_their_contents_expand() {
    // tests/golden/stata18/semantics.log:
    //   local b "some text" ; local c `"`b' more"' ; di `"`c'"'  →  some text more
    let mut env = MacroEnv::new();
    env.set_local("b", "some text");
    // `` `" `` is the compound-quote DELIMITER, not a macro reference: it passes
    // through so the lexer can see it, while `` `b' `` inside it expands.
    assert_eq!(
        ex(&mut env, r#"local c `"`b' more"'"#),
        r#"local c `"some text more"'"#
    );
    env.set_local("c", "some text more");
    assert_eq!(ex(&mut env, r#"di `"`c'"'"#), r#"di `"some text more"'"#);
    // Nesting: the delimiters of an inner compound quote are literal too.
    assert_eq!(ex(&mut env, r#"`"a `"b"' c"'"#), r#"`"a `"b"' c"'"#);
}

#[test]
fn macval_confines_expansion_to_the_first_level() {
    // [U] 18.3.8. Everything else is RESCANNED; `macval` is the one exception.
    let mut env = MacroEnv::new();
    env.set_local("inner", "DEEP");
    env.set_local("outer", "`inner'");
    assert_eq!(
        ex(&mut env, "`outer'"),
        "DEEP",
        "substituted text is rescanned"
    );
    assert_eq!(
        ex(&mut env, "`macval(outer)'"),
        "`inner'",
        "macval does not rescan"
    );
}

#[test]
fn increment_fires_even_on_a_branch_that_is_not_taken() {
    // [U] 18.3.7's technical note, and the direct consequence of expansion
    // happening BEFORE interpretation. This is also lint L004's subject.
    let mut env = MacroEnv::new();
    env.set_local("i", "0");
    let out = ex(&mut env, r#"if 0 di "`i++'""#);
    assert_eq!(
        out, r#"if 0 di "0""#,
        "post-increment substitutes the OLD value"
    );
    assert_eq!(
        env.local("i"),
        Some("1"),
        "the increment fires regardless of the branch"
    );
    // Pre-increment substitutes the NEW value.
    let mut env = MacroEnv::new();
    env.set_local("i", "4");
    assert_eq!(ex(&mut env, "`++i'"), "5");
    assert_eq!(env.local("i"), Some("5"));
    assert_eq!(ex(&mut env, "`--i'"), "4");
    assert_eq!(ex(&mut env, "`i--'"), "4");
    assert_eq!(env.local("i"), Some("3"));
}

#[test]
fn extended_macro_functions_match_the_golden() {
    // tests/golden/stata18/semantics.log, verbatim.
    let mut env = MacroEnv::new();
    assert_eq!(ex(&mut env, "`: word count one two three'"), "3");
    assert_eq!(ex(&mut env, "`: word 2 of one two three'"), "two");
}

/// 02 §4.4's ten measured rows: `%18.0g`, then trim.
///
/// **ESCALATION (still open — the fix is the architect's).**
/// `IMPLEMENTATION_PLAN.md` §W04b writes the `1e20/3` row as
/// `3.33333333311e+19`; design `02` §4.4 marks it `3.33333333333e+19` **[V]**.
/// The design is right. That is not asserted here, it is *derived* from the
/// committed golden by [`the_disputed_1e20_over_3_row_is_derived_from_the_golden`],
/// which reads `tests/golden/stata18/gformat.log` and re-fits StataMP 18.5's own
/// width rule. Read that test before "fixing" this row to match the plan.
///
/// Re-checked in repair round 3 and independently confirmed by review: no digit
/// count in `1..=17` renders the double `33333333333333331968` with a mantissa
/// ending in `11`, so the plan's value is unreachable by rounding at ANY width,
/// not merely at 18. The plan text is what needs amending; this row is correct.
///
/// Round 4 re-derived the width rule from `gformat.log` from scratch, outside
/// this crate, and reproduced the counters exactly: 117 rows, 168 scientific
/// cells, 0 violations of `significant digits = w - 4 - k`. No golden anywhere
/// in `tests/golden/stata18/` prints `%18.0g` directly, so derivation is the
/// only available route — and it is an interpolation. Still awaiting the
/// architect; W04b owns none of `docs/`, so the plan edit is not ours to make.
const EXP_ROWS: &[(&str, &str)] = &[
    ("1/3", ".3333333333333333"),
    ("1/7", ".1428571428571428"),
    ("-1/7", "-.1428571428571428"),
    ("0.1", ".1"),
    ("1e20/3", "3.33333333333e+19"),
    ("9007199254740993", "9007199254740992"),
    ("1e-320", "9.9998886718e-321"),
    (".", "."),
    (".a", ".a"),
    (r#""hi""#, "hi"),
];

#[test]
fn exp_stringification_reproduces_all_ten_measured_rows() {
    for (src, want) in EXP_ROWS {
        let mut env = MacroEnv::new();
        assert_eq!(&ex(&mut env, &format!("`={src}'")), want, "`={src}'");
    }
    // The `1e20/3` row does NOT round-trip; losing exactly the precision Stata
    // loses is the requirement, so the assertion is spelled out.
    let round_tripped: f64 = "3.33333333333e+19".parse().expect("parses");
    assert_ne!(
        round_tripped,
        1e20 / 3.0,
        "the row must be lossy, like Stata"
    );
}

/// Decide the disputed `1e20/3` row against StataMP 18.5, not against a document.
///
/// `gformat.log`'s `g4` program prints every corpus value at `%9.0g`, `%10.0g`,
/// `%12.0g`, `%16.0g` and `%21.0g`, so width 18 is never printed directly. It
/// does not have to be: the five widths pin the rule down completely. For every
/// scientific rendering in that table the printed text, minus any leading `-`,
/// is exactly `w - 1` characters — Stata reserves the first column for the sign.
/// The layout is `d` `.` `m`×frac `e` sign `k`×exponent, so
///
/// ```text
/// 4 + m + k = w - 1   ⇒   significant digits = m + 1 = w - 4 - k
/// ```
///
/// Reaching `w = 18` from those five widths is **interpolation, not
/// extrapolation**, which is why the fit can be trusted: 18 is bracketed by the
/// observed 16 and 21, and *both* exponent classes that matter — two-digit and
/// three-digit — are present at every one of the five widths, so neither half of
/// the rule is carried by a single lonely sample. The relation holds with zero
/// residual on all 168 scientific cells; `violations` below is asserted at 0.
///
/// At `w = 18`: a two-digit exponent gets 12 significant digits and a
/// three-digit exponent gets 11. Both halves of the fit are checked below, the
/// second against 02 §4.4's own `1e-320` row, which is `9.9998886718e-321` — 11
/// significant digits, exactly as the formula demands. `1e20/3` is the double
/// `33333333333333331968`; rounded to 12 significant digits it is
/// `3.33333333333e+19`. No digit count in `1..=17` renders it with a mantissa
/// ending in `11`, so the plan's `3.33333333311e+19` is unreachable by rounding
/// and is a transcription error.
///
/// ADR-017 counters: golden rows parsed, scientific cells checked, width-rule
/// violations, and widths carrying both exponent classes. All four are exact, so
/// editing the golden trips this test.
#[test]
fn the_disputed_1e20_over_3_row_is_derived_from_the_golden() {
    // `di "BITS|w9|w10|w12|w16|w21|"` — the header of gformat.log's g4 table.
    const WIDTHS: [usize; 5] = [9, 10, 12, 16, 21];
    let log = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/stata18/gformat.log");
    let text = std::fs::read_to_string(&log).expect("gformat.log is committed");

    let mut rows = 0usize;
    let mut sci_cells = 0usize;
    let mut violations = 0usize;
    // Widths at which a scientific rendering was actually observed, keyed by the
    // number of exponent digits, so reaching 18 is a fit and not a
    // guess: (exponent digits, width) ⇒ significant digits.
    let mut fits: Vec<(usize, usize, usize)> = Vec::new();

    for line in text.lines() {
        // A g4 data row starts with the %21x bit pattern: sign, then `X`.
        let Some(rest) = line.strip_prefix(['+', '-']) else {
            continue;
        };
        if !rest.contains('X') {
            continue;
        }
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        if cells.len() < 6 {
            continue;
        }
        rows += 1;
        for (&w, cell) in WIDTHS.iter().zip(&cells[1..6]) {
            let Some((mantissa, exponent)) = cell.split_once('e') else {
                continue; // fixed-notation rendering; the rule under test is the
                          // scientific one.
            };
            sci_cells += 1;
            let unsigned = mantissa.strip_prefix('-').unwrap_or(mantissa);
            let printed = unsigned.len() + 1 + exponent.len();
            if printed != w - 1 {
                violations += 1;
                continue;
            }
            let exp_digits = exponent
                .strip_prefix(['+', '-'])
                .expect("Stata always signs the exponent")
                .len();
            let sig = unsigned.chars().filter(char::is_ascii_digit).count();
            assert_eq!(sig, w - 4 - exp_digits, "{cell} at %{w}.0g");
            fits.push((exp_digits, w, sig));
        }
    }

    assert_eq!(rows, 117, "g4 rows in gformat.log");
    assert_eq!(sci_cells, 168, "scientific cells in gformat.log");
    assert_eq!(violations, 0, "cells breaking `printed length == w - 1`");
    // Both exponent widths that matter at %18.0g are represented in the fit.
    assert!(fits.iter().any(|&(k, ..)| k == 2), "2-digit exponents seen");
    assert!(fits.iter().any(|&(k, ..)| k == 3), "3-digit exponents seen");

    // The fit is an INTERPOLATION to 18, not an extrapolation, and neither half
    // of the rule rests on one lonely sample: every observed width carries both
    // exponent classes. Counting the widths that do is the counter that says so.
    let both_classes = WIDTHS
        .iter()
        .filter(|&&w| {
            fits.iter().any(|&(k, fw, _)| fw == w && k == 2)
                && fits.iter().any(|&(k, fw, _)| fw == w && k == 3)
        })
        .count();
    assert_eq!(both_classes, 5, "widths carrying both exponent classes");
    let (lo, hi) = (16usize, 21usize);
    assert!(WIDTHS.contains(&lo) && WIDTHS.contains(&hi));
    assert!(
        (lo..=hi).contains(&18),
        "18 is bracketed by measured widths"
    );

    // The fit, carried to the width macro stringification uses.
    let sig_at_18 = |exp_digits: usize| 18 - 4 - exp_digits;
    assert_eq!(sig_at_18(2), 12);
    assert_eq!(sig_at_18(3), 11);

    // 02 §4.4's `1e-320` row is the three-digit-exponent half of the fit, and it
    // was measured independently of the disputed row.
    assert_eq!(stringify_number(1e-320), "9.9998886718e-321");
    assert_eq!(
        "9.9998886718".chars().filter(char::is_ascii_digit).count(),
        sig_at_18(3)
    );

    // The two-digit-exponent half: the disputed row.
    let v: f64 = 1e20 / 3.0;
    assert_eq!(
        stratum_core::fmt::fmt_hex(v),
        "+1.ce97ca0f21055X+040",
        "di %21x (1e20/3), in gformat.log's own lossless notation"
    );
    assert_eq!(v.to_bits(), 0x43FC_E97C_A0F2_1055);
    assert_eq!(format!("{v:.0}"), "33333333333333331968");
    let derived = format!("{:.*e}", sig_at_18(2) - 1, v).replace("e19", "e+19");
    assert_eq!(derived, "3.33333333333e+19");
    assert_eq!(stringify_number(v), derived);

    // And the plan's value is not a rounding of anything: no significant-digit
    // count renders `1e20/3` with a mantissa ending in `11`.
    for d in 1..=17usize {
        let mantissa = format!("{:.*e}", d - 1, v);
        let mantissa = mantissa.split_once('e').expect("scientific").0;
        assert!(
            !mantissa.ends_with("11"),
            "{d} significant digits gave {mantissa}"
        );
    }
}

#[test]
fn the_result_of_an_exp_is_inserted_as_a_bare_literal() {
    // `di `="ab"+"cd"'` → `abcd not found  r(111)` [V]: the runtime reads `abcd`
    // as a VARIABLE NAME because no quoting was added.
    let mut env = MacroEnv::new();
    assert_eq!(ex(&mut env, r#"di `="ab"+"cd"'"#), "di abcd");
}

#[test]
fn the_span_map_takes_expanded_offsets_back_to_the_source() {
    let mut env = MacroEnv::new();
    env.set_local("v", "price");
    let mut host = MockHost::default();
    let e = expand("summarize `v'", &mut env, &mut host).expect("expanded");
    assert_eq!(e.text, "summarize price");
    // The literal run is 1:1.
    assert_eq!(e.map.to_source(0), 0);
    assert_eq!(e.map.to_source(5), 5);
    // Every byte the macro produced resolves INTO the reference that produced
    // it: `` `v' `` occupies input 10..13, so an error anywhere inside the
    // substituted `price` underlines the macro reference — the thing the user
    // can actually edit — rather than whatever follows it (spec §21).
    for off in 10..e.text.len() as u32 {
        let at = e.map.to_source(off);
        assert!((10..13).contains(&at), "offset {off} resolved to {at}");
    }
}

// ────────────────────────── ADR-017 counters ────────────────────────────────

#[test]
fn a_macro_free_line_does_no_substitution_work_at_all() {
    // ADR-017: the assertion is a COUNTER, not a duration. Every real do-file is
    // overwhelmingly lines like this one, and the keystroke path must not build
    // a piece table or call the host for them.
    let mut env = MacroEnv::new();
    let mut host = MockHost::default();
    let src = "summarize price mpg weight, detail";
    let e = expand(src, &mut env, &mut host).expect("expanded");
    assert_eq!(e.text, src);
    assert_eq!(e.stats.substitutions, 0);
    assert_eq!(e.stats.host_calls, 0);
    assert_eq!(e.stats.max_depth, 0);
    assert_eq!(e.stats.bytes_out as usize, src.len());
    assert!(host.calls.is_empty());
}

#[test]
fn nesting_costs_exactly_one_substitution_per_reference() {
    let mut env = MacroEnv::new();
    env.set_local("A", "B");
    env.set_local("B", "C");
    let mut host = MockHost::default();
    // `` ``A'' `` is TWO references — the inner `` `A' `` and the outer one.
    let e = expand("``A''", &mut env, &mut host).expect("expanded");
    assert_eq!(e.text, "C");
    assert_eq!(e.stats.substitutions, 2);
    assert_eq!(e.stats.host_calls, 0);
}

#[test]
fn one_host_call_per_exp_and_none_for_a_text_only_xmf() {
    let mut env = MacroEnv::new();
    let mut host = MockHost::default();
    let e = expand("`=1+1' `: word count a b c'", &mut env, &mut host).expect("expanded");
    assert_eq!(e.text, "2 3");
    // The `word count` is answered by `macros::xmf` without the engine, which is
    // what lets the whole macro suite run with no runtime linked.
    assert_eq!(e.stats.host_calls, 1);
    assert_eq!(host.calls, vec!["=1+1".to_owned()]);
}

#[test]
fn runaway_nesting_is_r920_not_a_hang() {
    let mut env = MacroEnv::new();
    // `local a `a'` refers to itself; every rescan goes one level deeper.
    env.set_local("a", "`a'");
    env.limits.max_depth = 32;
    let mut host = MockHost::default();
    let err = expand("`a'", &mut env, &mut host).expect_err("must not hang");
    assert_eq!(err.rc, 920);
}

#[test]
fn an_over_long_expansion_is_r920() {
    let mut env = MacroEnv::new();
    env.set_local("big", &"x".repeat(1000) as &str);
    env.limits.max_expanded_len = 100;
    let mut host = MockHost::default();
    let err = expand("`big'", &mut env, &mut host).expect_err("must be capped");
    assert_eq!(err.rc, 920);
}

#[test]
fn an_unmatched_backtick_is_a_literal_byte_not_an_error() {
    let mut env = MacroEnv::new();
    assert_eq!(ex(&mut env, "di `unclosed"), "di `unclosed");
}

#[test]
fn expansion_is_pure() {
    let mut env = MacroEnv::new();
    env.set_local("x", "1");
    let a = ex(&mut env, "di `x' + $y");
    let b = ex(&mut env, "di `x' + $y");
    assert_eq!(a, b);
}
