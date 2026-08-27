//! The deterministic lints — design 02 §11.
//!
//! Spec §16 and §21 both require the deterministic check to run first and the AI
//! second, so every one of these has to work with no model, no network and no
//! engine. That is the property the tests below are really asserting: each lint
//! is a pure function of the AST (or, for `L003`, of the raw source).

use stratum_parse::lints::{check_l003, code, LintCtx};
use stratum_parse::varlist::{SimpleVarIndex, VarlistCtx};
use stratum_parse::{lint, parse_command, ParseMode};

fn findings(src: &str, vars: Option<&VarlistCtx<'_>>) -> Vec<(String, String)> {
    let (stmt, _) = parse_command(src, ParseMode::Speculative);
    let cx = LintCtx { text: src, vars };
    lint(&stmt, &cx)
        .into_iter()
        .map(|d| (d.code, d.message))
        .collect()
}

fn codes(src: &str) -> Vec<String> {
    findings(src, None).into_iter().map(|(c, _)| c).collect()
}

#[test]
fn l001_flags_a_macro_inside_a_plain_string() {
    // 02 §1.1: `local q = `"embedded "quote""'` then `di "B13: `q'"` ERRORS,
    // because expansion is quote-blind and the substituted text re-tokenizes.
    let f = findings(r#"display "B13: `q'""#, None);
    assert!(f.iter().any(|(c, _)| c == code::L001), "{f:?}");
    // A compound double quote is already safe.
    assert!(!codes(r#"display `"B13: `q'"'"#).contains(&code::L001.to_owned()));
    // A string with no macro in it is not flagged.
    assert!(!codes(r#"display "plain""#).contains(&code::L001.to_owned()));
}

#[test]
fn l001_offers_the_compound_quote_rewrite() {
    let (stmt, _) = parse_command(r#"display "x `m'""#, ParseMode::Speculative);
    let cx = LintCtx {
        text: r#"display "x `m'""#,
        vars: None,
    };
    let d = lint(&stmt, &cx)
        .into_iter()
        .find(|d| d.code == code::L001)
        .expect("L001");
    let fix = d.suggestions.first().expect("a deterministic fix");
    assert_eq!(fix.edits[0].text, r#"`"x `m'"'"#);
}

#[test]
fn l002_flags_a_comparison_that_treats_missing_as_true() {
    // [U] 13.2.3's classic trap, and a direct consequence of 02 §8.3's encoding:
    // every missing value is above every number, so `income > 10000` is true
    // wherever income is missing.
    let f = findings("generate d = income > 10000", None);
    assert!(f.iter().any(|(c, _)| c == code::L002), "{f:?}");
    assert!(codes("generate d = income >= 10000").contains(&code::L002.to_owned()));
    // The narrow scope: variable-to-variable is usually deliberate, and warning
    // about it would train people to ignore the lint.
    assert!(!codes("generate d = income > wealth").contains(&code::L002.to_owned()));
    // So is a comparison that already guards.
    assert!(codes("generate d = income < 10000").is_empty());
}

#[test]
fn l002_suggests_the_missing_guard() {
    let src = "generate d = income > 10000";
    let (stmt, _) = parse_command(src, ParseMode::Speculative);
    let cx = LintCtx {
        text: src,
        vars: None,
    };
    let d = lint(&stmt, &cx)
        .into_iter()
        .find(|d| d.code == code::L002)
        .expect("L002");
    assert_eq!(d.suggestions[0].edits[0].text, " & !missing(income)");
}

#[test]
fn l003_flags_a_separator_line_that_continues() {
    // 02 §2.1, verified: THREE OR MORE slashes is a continuation, not a comment,
    // and it splices with no inserted separator — so the command under a
    // `//////` decoration silently disappears.
    let src = "summarize price\n//////////\nsummarize mpg\n";
    let f = check_l003(src);
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].code, code::L003);
    assert!(f[0].suggestions[0].edits[0].text.starts_with("// "));
    // Two slashes is an ordinary comment and must not be flagged.
    assert!(check_l003("// ------------\n").is_empty());
    // Nor is a real `///` continuation with content after it.
    assert!(check_l003("local t 1 ///\n    2\n").is_empty());
}

#[test]
fn l004_flags_an_increment_in_a_braceless_if() {
    // [U] 18.3.7's technical note: expansion runs before interpretation, so the
    // increment fires whether or not the branch is taken.
    let src = r#"if x > 0 display "`i++'""#;
    let f = findings(src, None);
    assert!(f.iter().any(|(c, _)| c == code::L004), "{f:?}");
    // A braced body is its own logical line and is re-expanded per execution.
    let src = r#"if x > 0 { display "`i++'" }"#;
    assert!(!codes(src).contains(&code::L004.to_owned()));
    // And an ordinary macro reference is not an increment.
    assert!(!codes(r#"if x > 0 display "`i'""#).contains(&code::L004.to_owned()));
}

#[test]
fn l005_flags_absolute_paths() {
    for src in [
        r#"use "/Users/me/data.dta", clear"#,
        r#"use "C:\data\x.dta", clear"#,
        r#"use "~/data.dta", clear"#,
    ] {
        assert!(codes(src).contains(&code::L005.to_owned()), "`{src}`");
    }
    // A project-relative path is exactly what spec §16 wants and is not flagged.
    assert!(!codes(r#"use "data/x.dta", clear"#).contains(&code::L005.to_owned()));
}

#[test]
fn l006_names_the_nearest_option() {
    // tests/golden/stata18/errors.log: `summarize price, detial` is r(198)
    // "option detial not allowed". The return code is the parser's; the
    // suggestion is this lint's, and it is one transposition away.
    let src = "summarize price, detial";
    let (stmt, _) = parse_command(src, ParseMode::Speculative);
    let cx = LintCtx {
        text: src,
        vars: None,
    };
    let d = lint(&stmt, &cx)
        .into_iter()
        .find(|d| d.code == code::L006)
        .expect("L006");
    assert_eq!(d.suggestions[0].label, "did you mean `detail`?");
    assert_eq!(d.suggestions[0].edits[0].text, "detail");
    // A real option is not flagged.
    assert!(!codes("summarize price, detail").contains(&code::L006.to_owned()));
}

#[test]
fn l007_is_damerau_levenshtein_over_the_live_index_and_needs_no_model() {
    // Spec §21's "Did you mean 'income'?", offline and instant.
    let idx = SimpleVarIndex::from_names(&["income", "price", "mpg"]);
    let vcx = VarlistCtx {
        vars: &idx,
        varabbrev: false,
    };
    let src = "summarize incme";
    let (stmt, _) = parse_command(src, ParseMode::Speculative);
    let cx = LintCtx {
        text: src,
        vars: Some(&vcx),
    };
    let d = lint(&stmt, &cx)
        .into_iter()
        .find(|d| d.code == code::L007)
        .expect("L007");
    assert_eq!(d.message, "variable incme not found");
    assert_eq!(d.suggestions[0].label, "did you mean `income`?");
    // With no variable list the lint stays silent rather than flagging
    // everything: an editor with no dataset loaded knows nothing.
    assert!(!codes("summarize incme").contains(&code::L007.to_owned()));
}

#[test]
fn l007_accepts_a_legal_abbreviation() {
    let idx = SimpleVarIndex::from_names(&["income", "price"]);
    let vcx = VarlistCtx {
        vars: &idx,
        varabbrev: true,
    };
    let f = findings("summarize inc", Some(&vcx));
    assert!(!f.iter().any(|(c, _)| c == code::L007), "{f:?}");
}

#[test]
fn l007_stays_quiet_where_the_head_is_not_a_varlist_of_existing_variables() {
    // REGRESSION. The universal grammar reads the head of `rename old new` and
    // `format price %9.2f` as a varlist PROVISIONALLY, because only the command
    // knows what the extra words mean; `generate newvar = …` names something
    // that does not exist yet. Flagging any of those is a false positive, and a
    // lint that cries wolf gets switched off.
    let idx = SimpleVarIndex::from_names(&["price", "mpg"]);
    let vcx = VarlistCtx {
        vars: &idx,
        varabbrev: false,
    };
    for src in [
        "rename price newname",
        "format price %9.2f",
        "generate newvar = price",
        "use \"newfile.dta\", clear",
    ] {
        let f = findings(src, Some(&vcx));
        assert!(
            !f.iter().any(|(c, _)| c == code::L007),
            "`{src}` must not be flagged: {f:?}"
        );
    }
    // …and the unambiguous case still is.
    let f = findings("summarize nosuchvar", Some(&vcx));
    assert!(f.iter().any(|(c, _)| c == code::L007), "{f:?}");
}

#[test]
fn findings_are_ordered_deterministically() {
    // The problems pane is diffed; an unstable order repaints it on every
    // keystroke and, worse, makes a golden test flap.
    let src = r#"generate d = income > 10000"#;
    let a = codes(src);
    let b = codes(src);
    assert_eq!(a, b);
    let mut sorted = a.clone();
    sorted.sort();
    assert_eq!(a, sorted, "findings must come back in `code` order");
}

#[test]
fn a_clean_line_produces_nothing() {
    assert!(codes("summarize price mpg, detail").is_empty());
    assert!(codes("regress price mpg weight, robust").is_empty());
}
