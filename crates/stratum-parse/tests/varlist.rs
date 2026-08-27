//! Varlist conformance — design 02 §7, against `auto.dta`'s real layout.
//!
//! Every expectation here is read off `tests/golden/stata18/semantics.log`,
//! whose `ds` output prints `auto.dta`'s twelve variables in STORAGE order
//! (column-major, six columns of two).

use stratum_parse::varlist::{
    expand_varlist, glob_match, is_reserved, parse_varlist, SimpleVarIndex, VarIndex, VarlistCtx,
    VarlistMode,
};
use stratum_parse::{parse_command, ParseMode, Span, StataError};

/// `auto.dta` in storage order.
const AUTO: &[&str] = &[
    "make",
    "price",
    "mpg",
    "rep78",
    "headroom",
    "trunk",
    "weight",
    "length",
    "turn",
    "displacement",
    "gear_ratio",
    "foreign",
];

fn resolve(spec: &str, varabbrev: bool) -> Result<Vec<String>, StataError> {
    let idx = SimpleVarIndex::from_names(AUTO);
    let cx = VarlistCtx {
        vars: &idx,
        varabbrev,
    };
    let vl = parse_varlist(
        spec,
        Span {
            start: 0,
            end: spec.len() as u32,
        },
    );
    Ok(expand_varlist(&vl, &cx, VarlistMode::Existing)?
        .into_iter()
        .map(|i| idx.name(i as usize).to_owned())
        .collect())
}

/// The same thing through the real parser, so the varlist slot is exercised too.
fn via_command(src: &str, varabbrev: bool) -> Result<Vec<String>, StataError> {
    let (stmt, diags) = parse_command(src, ParseMode::Execute);
    assert!(diags.iter().all(|d| d.stata_rc.is_none()), "{diags:#?}");
    let stratum_parse::ast::Command::Known(k) = stmt.cmd else {
        panic!("`{src}`: expected a known command")
    };
    let vl = k.slots.varlist.expect("varlist slot");
    let idx = SimpleVarIndex::from_names(AUTO);
    let cx = VarlistCtx {
        vars: &idx,
        varabbrev,
    };
    Ok(expand_varlist(&vl, &cx, VarlistMode::Existing)?
        .into_iter()
        .map(|i| idx.name(i as usize).to_owned())
        .collect())
}

#[test]
fn make_mpg_is_a_storage_order_range() {
    // The acceptance bullet, verbatim: `ds make-mpg` on auto.dta returns
    // `make price mpg` [V] — not alphabetical, not numeric.
    assert_eq!(resolve("make-mpg", true).unwrap(), ["make", "price", "mpg"]);
    assert_eq!(
        via_command("ds make-mpg", true).unwrap(),
        ["make", "price", "mpg"]
    );
    // And the golden's own example.
    assert_eq!(
        via_command("summarize price-rep78", true).unwrap(),
        ["price", "mpg", "rep78"]
    );
}

#[test]
fn ds_pri_tilde_e_resolves_and_ds_r_tilde_p_is_r111() {
    // Both from the acceptance bullet, both verified against StataMP 18.5.
    assert_eq!(via_command("ds pri~e", true).unwrap(), ["price"]);
    let e = via_command("ds r~p", true).unwrap_err();
    assert_eq!(e.rc, 111);
    assert_eq!(e.offending_token.as_deref(), Some("r~p"));
}

#[test]
fn set_varabbrev_off_disables_bare_name_abbreviation() {
    // `summarize pri` → `price` [V]; `di pri` with varabbrev off → r(111) [V].
    assert_eq!(via_command("summarize pri", true).unwrap(), ["price"]);
    assert_eq!(via_command("summarize pri", false).unwrap_err().rc, 111);
    // OPEN QUESTION Q5: `~` is documented as a wildcard, not an abbreviation, so
    // it keeps working. Recorded as a decision, not a guess left implicit.
    assert_eq!(via_command("ds pri~e", false).unwrap(), ["price"]);
}

#[test]
fn wildcards_are_storage_order_and_may_match_nothing() {
    assert_eq!(via_command("summarize m*", true).unwrap(), ["make", "mpg"]);
    // A glob that matches nothing is NOT an error ([U] 11.4.1).
    assert!(via_command("summarize zz*", true).unwrap().is_empty());
    // `?` is exactly one character, so `??*` is "two or more".
    assert!(glob_match("??*", "mpg"));
    assert!(!glob_match("??*", "m"));
}

#[test]
fn all_and_bare_star_are_every_variable_in_storage_order() {
    for spec in ["_all", "*"] {
        let got = resolve(spec, true).unwrap();
        assert_eq!(got.len(), AUTO.len(), "`{spec}`");
        assert_eq!(got, AUTO, "`{spec}`");
    }
}

#[test]
fn results_are_neither_sorted_nor_deduplicated() {
    // [U] 11.4.1: repetition is legal and meaningful, and order is the report
    // order of every command that takes a varlist.
    assert_eq!(
        via_command("summarize mpg price mpg", true).unwrap(),
        ["mpg", "price", "mpg"]
    );
    // Across items, source order; within a glob, storage order.
    assert_eq!(
        via_command("summarize foreign m*", true).unwrap(),
        ["foreign", "make", "mpg"]
    );
}

#[test]
fn a_missing_variable_is_r111_with_the_golden_wording() {
    // tests/golden/stata18/errors.log: `summarize nosuchvar` prints
    // "variable nosuchvar not found" and returns 111.
    let e = via_command("summarize nosuchvar", true).unwrap_err();
    assert_eq!(e.rc, 111);
    assert_eq!(e.message, "variable nosuchvar not found");
    assert_eq!(e.offending_token.as_deref(), Some("nosuchvar"));
    // `summarize incom` is r(111) too: no variable starts with `incom` [V].
    assert_eq!(via_command("summarize incom", true).unwrap_err().rc, 111);
}

#[test]
fn an_abbreviation_matching_several_variables_is_ambiguous() {
    let idx = SimpleVarIndex::from_names(&["income", "incorp"]);
    let cx = VarlistCtx {
        vars: &idx,
        varabbrev: true,
    };
    let vl = parse_varlist("inc", Span { start: 0, end: 3 });
    let e = expand_varlist(&vl, &cx, VarlistMode::Existing).unwrap_err();
    assert_eq!(e.rc, 111);
    assert_eq!(e.message, "ambiguous abbreviation");
}

#[test]
fn reserved_names_are_the_u_11_3_list() {
    for n in [
        "_all", "_b", "_cons", "_n", "_N", "_pi", "_rc", "_se", "byte", "str8", "_r_b",
    ] {
        assert!(is_reserved(n), "`{n}` must be reserved");
    }
    for n in ["price", "strata", "n", "b"] {
        assert!(!is_reserved(n), "`{n}` must not be reserved");
    }
}

#[test]
fn typed_and_labeled_patterns_parse() {
    use stratum_parse::ast::{VarItemKind, VarPattern};
    use stratum_proto::StorageType;
    let vl = parse_varlist("int(price mpg)", Span { start: 0, end: 14 });
    let VarItemKind::Single(a) = &vl.items[0].kind else {
        panic!("expected an atom")
    };
    let VarPattern::Typed { ty, inner } = &a.base else {
        panic!("expected a typed pattern, got {:?}", a.base)
    };
    assert_eq!(*ty, StorageType::Int);
    assert_eq!(inner.len(), 2);

    let vl = parse_varlist("rep78:origin", Span { start: 0, end: 12 });
    let VarItemKind::Single(a) = &vl.items[0].kind else {
        panic!("expected an atom")
    };
    assert!(matches!(a.base, VarPattern::Labeled { .. }));
}

#[test]
fn interactions_split_into_atoms() {
    use stratum_parse::ast::VarItemKind;
    let vl = parse_varlist("i.rep78##c.weight", Span { start: 0, end: 17 });
    let VarItemKind::Interact { atoms, full } = &vl.items[0].kind else {
        panic!("expected an interaction, got {:?}", vl.items[0].kind)
    };
    assert!(full, "`##` includes the lower-order terms");
    assert_eq!(atoms.len(), 2);
}

#[test]
fn a_typed_filter_keeps_only_matching_columns() {
    use stratum_proto::StorageType;
    let idx = SimpleVarIndex::new(vec![
        ("a".to_owned(), StorageType::Int),
        ("b".to_owned(), StorageType::Double),
        ("c".to_owned(), StorageType::Int),
    ]);
    let cx = VarlistCtx {
        vars: &idx,
        varabbrev: true,
    };
    let vl = parse_varlist("int(a b c)", Span { start: 0, end: 10 });
    let got = expand_varlist(&vl, &cx, VarlistMode::Existing).unwrap();
    assert_eq!(got, [0, 2]);
}
