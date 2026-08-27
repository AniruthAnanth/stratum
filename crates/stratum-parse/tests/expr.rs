//! Expression conformance — design 02 §8, every verified row.
//!
//! # Why the corpus file is `data/precedence.txt` and not `tests/expr/precedence.txt`
//!
//! The plan's acceptance names `tests/expr/precedence.txt`. `docs/ownership.toml`
//! gives W04b `crates/stratum-parse/tests/expr.rs` — an EXACT path, not a glob —
//! and gives `crates/stratum-parse/tests/expr/**` to nobody, so that file would
//! be a tracked file owned by NOBODY, which R0 makes fatal. Measured, not
//! assumed: staging it and running `cargo xtask ownership` reports
//! "1 tracked file(s) owned by NOBODY: crates/stratum-parse/tests/expr/
//! precedence.txt" and exits non-zero.
//!
//! `crates/stratum-parse/data/**` IS a glob grant to W04b, so the corpus lives
//! there under the plan's own file name and is pulled in with `include_str!`.
//! The acceptance's substance — an external, one-row-per-line corpus carrying
//! every verified row of 02 §8.1 — holds; only the directory differs, and the
//! difference is one line to undo if the architect grants `tests/expr/**`.
//! Precedent: W04 left `fuzz/Cargo.toml` uncreated for the same reason and
//! said so.
//!
//! # Why there is an evaluator in a parser test
//!
//! 02 §8.1's evidence column is *answers*, not tree shapes: `2^3^2` = **64**,
//! `!0/2` = **.5**, `1e300*1e300/1e300` = **1e300**. Asserting a tree shape
//! would let a wrong `prec()` table pass as long as the test author derived the
//! shape from the same wrong table. Reducing the parse to a number and comparing
//! it against what StataMP 18.5 printed cannot do that. The evaluator is 120
//! lines and implements only 02 §§8.2–8.3, which is exactly the semantics under
//! test.

use stratum_parse::ast::{BinOp, Expr, UnOp};
use stratum_parse::{parse_command, ParseMode};

/// 02 §8.3, verified by reading raw bits with `%21x`: `.` is `2^1023`.
///
/// Taken from `stratum-core` rather than rebuilt here. There is ONE definition
/// of the missing-value encoding in this workspace (ADR-005), and a test that
/// carried its own copy could agree with itself while disagreeing with the
/// engine.
use stratum_core::missing::{is_missing, missing_f64 as missing, SYSMISS};

/// 02 §8.3: any arithmetic touching a missing yields `.`, collapsing extended
/// missings; overflow and division by zero yield `.`, never `inf`.
fn canon(x: f64) -> f64 {
    // `!is_finite()` and not `is_nan()`: `log(0)` is -inf, which is BELOW
    // SYSMISS and would sail through an `is_missing` check. Stata prints `.`
    // for it [V], and `di exp(1000)` is `.` rather than `inf` for the same
    // reason — the whole non-finite range canonicalises.
    if !x.is_finite() || is_missing(x) {
        SYSMISS
    } else {
        x
    }
}

#[derive(Clone, PartialEq, Debug)]
enum Val {
    Real(f64),
    Str(String),
}

impl Val {
    fn num(&self) -> f64 {
        match self {
            Val::Real(v) => *v,
            Val::Str(_) => SYSMISS,
        }
    }
}

// ────────────────────────────── the corpus ──────────────────────────────────

/// The corpus, one `expression => answer` per line — see `data/precedence.txt`.
///
/// `include_str!` and not a runtime read: a missing, renamed or unreadable
/// corpus must fail the BUILD. A `std::fs::read_to_string(...).unwrap_or_default()`
/// would turn a lost file into a test that passes with zero rows.
const PRECEDENCE: &str = include_str!("../data/precedence.txt");

/// Every non-comment row of `data/precedence.txt`, counted by hand.
///
/// Exact and not a floor (ADR-017: assert a counter). `rows >= 50` would let
/// nine rows be deleted and still pass; this makes any change to the corpus
/// size a deliberate edit of two files.
const PRECEDENCE_ROWS: usize = 59;

#[test]
fn every_verified_row_of_the_precedence_table() {
    let mut rows = 0usize;
    for line in PRECEDENCE.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (src, want) = line
            .split_once("=>")
            .expect("row shape is `expr => answer`");
        let (src, want) = (src.trim(), want.trim());
        let got = show(&eval(&parse_one(src)));
        assert_eq!(got, want, "`{src}`");
        rows += 1;
    }
    // A silently empty — or quietly shortened — corpus would otherwise pass
    // every assertion above.
    assert_eq!(rows, PRECEDENCE_ROWS, "the corpus changed size");
}

#[test]
fn pow_is_left_associative_in_the_tree_as_well_as_the_answer() {
    // The answer test above would also pass if `^` were non-associative and the
    // parser happened to bracket left. This pins the shape.
    let Expr::Binary { op, lhs, .. } = parse_one("2^3^2") else {
        panic!("expected a binary node")
    };
    assert_eq!(op, BinOp::Pow);
    assert!(
        matches!(lhs.as_ref(), Expr::Binary { op: BinOp::Pow, .. }),
        "`^` must group to the LEFT"
    );
}

#[test]
fn the_six_relational_levels_are_distinct() {
    // 02 §8.1: `!=` `>` `<` `<=` `>=` `==` are SIX levels, not one. The table is
    // the contract, so it is checked directly as well as through answers.
    let levels = [
        BinOp::Ne.prec(),
        BinOp::Gt.prec(),
        BinOp::Lt.prec(),
        BinOp::Le.prec(),
        BinOp::Ge.prec(),
        BinOp::Eq.prec(),
    ];
    let mut sorted = levels.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 6, "relational operators share a level");
    assert_eq!(levels, [50, 45, 40, 35, 30, 25]);
    // …and they are all looser than every arithmetic operator.
    assert!(BinOp::Ne.prec() < BinOp::Add.prec());
    // …and tighter than the logical ones.
    assert!(BinOp::Eq.prec() > BinOp::And.prec());
    assert!(BinOp::And.prec() > BinOp::Or.prec());
}

#[test]
fn subscripts_and_stored_results_parse() {
    assert!(matches!(parse_one("price[_n-1]"), Expr::Index { .. }));
    assert!(matches!(parse_one("r(mean)"), Expr::Stored { .. }));
    assert!(matches!(parse_one("_b[price]"), Expr::Coef { .. }));
    assert!(matches!(parse_one("M[1,2]"), Expr::MatElem { .. }));
    assert!(matches!(parse_one("_N"), Expr::Sys(..)));
    assert!(matches!(parse_one("L.gnp"), Expr::Term(..)));
}

#[test]
fn an_unknown_function_is_not_a_parse_error() {
    // 02 §8.5 and decision D7: `r(133) unknown function` comes from the RUNTIME,
    // so an ado-file calling a v2 function still highlights and folds.
    let (stmt, diags) = parse_command("display nosuchfn(1)", ParseMode::Execute);
    assert!(
        diags.iter().all(|d| d.stata_rc != Some(198)),
        "unknown functions must not be rejected at parse time: {diags:#?}"
    );
    let _ = stmt;
}

// ───────────────────────── the reference evaluator ──────────────────────────

fn parse_one(src: &str) -> Expr {
    // `display <exp>` is the shortest command whose slot is a bare expression.
    let text = format!("display {src}");
    let (stmt, diags) = parse_command(&text, ParseMode::Execute);
    let errs: Vec<_> = diags.iter().filter(|d| d.stata_rc.is_some()).collect();
    assert!(errs.is_empty(), "`{src}` failed to parse: {errs:#?}");
    let stratum_parse::ast::Command::Known(k) = stmt.cmd else {
        panic!("`{src}`: expected a known command");
    };
    let rest = k.slots.rest.expect("display keeps its tail in `rest`");
    let mut cur_text = rest.text;
    // Re-parse the tail on its own: `display`'s tail is not a varlist, so the
    // universal grammar deliberately hands it back verbatim.
    cur_text.insert_str(0, "");
    parse_bare(&cur_text)
}

fn parse_bare(src: &str) -> Expr {
    // `generate` has an ASSIGN slot, which is the one slot the universal grammar
    // parses as a whole expression.
    let text = format!("generate __t = {src}");
    let (stmt, diags) = parse_command(&text, ParseMode::Execute);
    let errs: Vec<_> = diags.iter().filter(|d| d.stata_rc.is_some()).collect();
    assert!(errs.is_empty(), "`{src}` failed to parse: {errs:#?}");
    let stratum_parse::ast::Command::Known(k) = stmt.cmd else {
        panic!("`{src}`: expected a known command");
    };
    k.slots.assign.expect("the `= exp` slot")
}

fn eval(e: &Expr) -> Val {
    match e {
        Expr::Num(v, _) => Val::Real(*v),
        Expr::Missing(k, _) => Val::Real(missing(*k)),
        Expr::Str(s, _) => Val::Str(s.clone()),
        Expr::Paren(inner, _) => eval(inner),
        Expr::Unary { op, rhs, .. } => {
            let v = eval(rhs).num();
            Val::Real(match op {
                UnOp::Neg => canon(-v),
                UnOp::Pos => canon(v),
                // `!` of any nonzero — missing included — is 0 [V].
                UnOp::Not => f64::from(v == 0.0),
            })
        }
        Expr::Binary { op, lhs, rhs, .. } => binary(*op, eval(lhs), eval(rhs)),
        Expr::Call { name, args, .. } => call(name, args),
        other => panic!("the reference evaluator does not cover {other:?}"),
    }
}

fn binary(op: BinOp, a: Val, b: Val) -> Val {
    use BinOp::*;
    // String operands: `+` concatenates, `*` repeats, relationals compare
    // BYTE-WISE on the UTF-8 bytes (02 §8.2).
    if let (Val::Str(x), Val::Str(y)) = (&a, &b) {
        return match op {
            Add => Val::Str(format!("{x}{y}")),
            Eq => Val::Real(f64::from(x == y)),
            Ne => Val::Real(f64::from(x != y)),
            Gt => Val::Real(f64::from(x.as_bytes() > y.as_bytes())),
            Lt => Val::Real(f64::from(x.as_bytes() < y.as_bytes())),
            Ge => Val::Real(f64::from(x.as_bytes() >= y.as_bytes())),
            Le => Val::Real(f64::from(x.as_bytes() <= y.as_bytes())),
            _ => Val::Real(SYSMISS),
        };
    }
    if let (Val::Str(x), Val::Real(n)) = (&a, &b) {
        if op == Mul {
            return Val::Str(x.repeat(*n as usize));
        }
    }
    let (x, y) = (a.num(), b.num());
    let arith = |v: f64| {
        Val::Real(if is_missing(x) || is_missing(y) {
            SYSMISS
        } else {
            canon(v)
        })
    };
    match op {
        Pow => arith(stratum_core::math::powf(x, y)),
        Div => arith(x / y),
        Mul => arith(x * y),
        Sub => arith(x - y),
        Add => arith(x + y),
        // Comparisons NEVER produce missing (02 §8.3).
        Eq => Val::Real(f64::from(x == y)),
        Ne => Val::Real(f64::from(x != y)),
        Gt => Val::Real(f64::from(x > y)),
        Lt => Val::Real(f64::from(x < y)),
        Ge => Val::Real(f64::from(x >= y)),
        Le => Val::Real(f64::from(x <= y)),
        And => Val::Real(f64::from(x != 0.0 && y != 0.0)),
        Or => Val::Real(f64::from(x != 0.0 || y != 0.0)),
    }
}

fn call(name: &str, args: &[Expr]) -> Val {
    let n = |i: usize| eval(&args[i]).num();
    Val::Real(match name {
        // `max`/`min` IGNORE missing: `max(1, .)` is 1 [V].
        "max" => args
            .iter()
            .map(|a| eval(a).num())
            .filter(|v| !is_missing(*v))
            .fold(f64::NEG_INFINITY, f64::max),
        "min" => args
            .iter()
            .map(|a| eval(a).num())
            .filter(|v| !is_missing(*v))
            .fold(f64::INFINITY, f64::min),
        "cond" => return eval(&args[if n(0) != 0.0 { 1 } else { 2 }]),
        "missing" => f64::from(args.iter().any(|a| is_missing(eval(a).num()))),
        // Stata rounds HALF AWAY FROM ZERO: round(2.5)=3, round(-2.5)=-2 is
        // NOT away-from-zero — it is -2 [V], so the rule is round-half-up on the
        // number line, which `(x + 0.5).floor()` gives exactly.
        "round" => (n(0) + 0.5).floor(),
        "int" => n(0).trunc(),
        "ceil" => n(0).ceil(),
        "floor" => n(0).floor(),
        // `mod(-7, 3)` is 2 [V] — Stata's mod is Euclidean, not `%`.
        "mod" => n(0) - n(1) * (n(0) / n(1)).floor(),
        "sqrt" => canon(stratum_core::math::sqrt(n(0))),
        "log" | "ln" => canon(stratum_core::math::ln(n(0))),
        "exp" => canon(stratum_core::math::exp(n(0))),
        "sum" => n(0),
        other => panic!("the reference evaluator does not cover {other}()"),
    })
}

/// Format the way `local x = exp` does: `%18.0g`, then trim (02 §4.4).
fn show(v: &Val) -> String {
    match v {
        Val::Str(s) => s.clone(),
        Val::Real(x) => {
            if is_missing(*x) {
                let tag = stratum_core::missing::tag_of(*x).unwrap_or(0);
                return if tag == 0 {
                    ".".to_owned()
                } else {
                    format!(".{}", (b'a' + tag - 1) as char)
                };
            }
            // The crate's own rule, so the test cannot disagree with the
            // implementation about what `%18.0g` means.
            stratum_parse::macros::stringify_number(*x)
        }
    }
}
