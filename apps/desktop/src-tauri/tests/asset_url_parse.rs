//! The per-OS `stratum-asset://` grammar test — A21, W17's acceptance.
//!
//! Tauri v2 delivers a custom scheme as `stratum-asset://localhost/…` on macOS
//! and Linux and as `http://stratum-asset.localhost/…` on Windows. This test
//! runs on all three OSes in CI and asserts the handler receives the same
//! `(kind, session, rest)` triple from the same logical URL in whichever
//! spelling the platform produces — the property that broke silently, in
//! packaged builds only, before the authority was pinned to `localhost`.
//!
//! `src/asset.rs` is deliberately pure (no `crate::` sibling, no Tauri) so this
//! file can compile it directly; the binary compiles the same source, so the
//! two cannot drift.

#[path = "../src/asset.rs"]
mod asset;

use asset::{parse, AssetError, AssetKind};

/// The URL the platform actually hands the handler, per OS.
fn platform_spelling(logical: &str) -> String {
    let path = logical
        .strip_prefix("stratum-asset://localhost/")
        .expect("a logical asset URL");
    if cfg!(windows) {
        format!("http://stratum-asset.localhost/{path}")
    } else {
        logical.to_owned()
    }
}

/// The token header's spelling is part of the wire contract (§10.2): the
/// frontend's `fetchAsset` attaches exactly this name.
#[test]
fn the_token_header_is_the_contract_spelling() {
    assert_eq!(asset::TOKEN_HEADER, "X-Stratum-Token");
}

/// A21's exact example: the triple is identical on every platform.
#[test]
fn the_handler_receives_the_same_triple_on_this_os() {
    let logical = "stratum-asset://localhost/graph/S1/R41.svg";
    let parsed = parse(&platform_spelling(logical)).expect("the platform spelling parses");
    assert_eq!(parsed.kind, AssetKind::Graph);
    assert_eq!(parsed.session, "S1");
    assert_eq!(parsed.rest, vec!["R41.svg".to_owned()]);
}

/// Both spellings parse identically REGARDLESS of the host OS — the handler
/// never branches on platform, only the webview's spelling does.
#[test]
fn both_spellings_yield_one_triple_for_every_route() {
    for logical in [
        "stratum-asset://localhost/result/1/41/raw",
        "stratum-asset://localhost/result/1/41/table",
        "stratum-asset://localhost/graph/1/41.svg",
        "stratum-asset://localhost/graph/1/41.png",
        "stratum-asset://localhost/frame/1/default/page?state=17&row0=0&nrows=40&cols=0,3&render=display&seq=1",
        "stratum-asset://localhost/app/assets/index.js",
    ] {
        let custom = parse(logical).expect(logical);
        let http = parse(&logical.replacen(
            "stratum-asset://localhost/",
            "http://stratum-asset.localhost/",
            1,
        ))
        .expect(logical);
        assert_eq!(custom, http, "the two spellings disagreed for {logical}");
    }
}

/// The kind segment survives — the exact defect A21 records is the first path
/// segment being eaten as the URL *authority* on two of three platforms.
#[test]
fn the_kind_segment_never_becomes_an_authority() {
    let parsed = parse(&platform_spelling(
        "stratum-asset://localhost/frame/7/default/page?state=1&row0=0&nrows=40&cols=0&render=display&seq=1",
    ))
    .expect("parses");
    assert_eq!(parsed.kind, AssetKind::Frame, "the kind segment vanished");
    assert_eq!(parsed.session, "7");
    assert_eq!(parsed.rest, vec!["default".to_owned(), "page".to_owned()]);
    assert!(parsed.query.is_some());
}

/// W03's threat-model screen holds in the platform spelling too.
#[test]
fn traversal_is_rejected_in_the_platform_spelling() {
    for bad in [
        "stratum-asset://localhost/result/1/%2e%2e/raw",
        "stratum-asset://localhost/app/a%2fb",
        "stratum-asset://localhost/app/a%5cb",
        "stratum-asset://localhost/app/a%00b",
    ] {
        let spelled = platform_spelling(bad);
        assert!(
            matches!(parse(&spelled), Err(AssetError::BadSegment)),
            "{spelled} must be rejected"
        );
    }
}
