//! `stratum-asset://` — the URL grammar and request policy (CONTRACTS §10.1,
//! §10.2, audit A21; threat model W03).
//!
//! # Why this module is pure
//!
//! Everything here is string → value: no Tauri, no I/O, no `crate::` sibling.
//! That is what lets `tests/asset_url_parse.rs` compile THIS file with a
//! one-line `#[path]` and assert, on all three OSes, that the handler receives
//! the same `(kind, session, rest)` triple from the same URL. The Tauri-facing
//! glue — resolving a `BulkRef`, asking the engine for a page, building the
//! `http::Response` — lives in `ipc.rs`, which calls [`parse`] first and never
//! touches a path segment this module has not decoded and screened.
//!
//! # The grammar, and why the authority is pinned (A21)
//!
//! ```text
//! stratum-asset://localhost/result/{session}/{result}/raw
//! stratum-asset://localhost/result/{session}/{result}/table
//! stratum-asset://localhost/graph/{session}/{result}.svg|.png
//! stratum-asset://localhost/frame/{session}/{frame}/page?…
//! stratum-asset://localhost/app/{…}
//! ```
//!
//! Tauri v2 maps a custom scheme to `http://stratum-asset.localhost/…` on
//! Windows (WebView2 cannot register a real scheme), while macOS and Linux
//! deliver the custom scheme itself. Without the pinned `localhost` authority
//! the first path segment becomes the *authority* on two of three platforms and
//! the kind segment silently vanishes — a defect that only appears in a
//! packaged build. Both spellings are accepted here; the path that follows is
//! identical everywhere, which is the entire point.

/// The custom-scheme spelling (macOS, Linux, and the frontend's builders).
pub const ASSET_SCHEME: &str = "stratum-asset://localhost/";
/// The WebView2 spelling (Windows). Both are in the CSP (CONTRACTS §10.2).
pub const ASSET_HTTP: &str = "http://stratum-asset.localhost/";

/// The header every request must carry (CONTRACTS §10.2).
pub const TOKEN_HEADER: &str = "X-Stratum-Token";

/// The URL space's first segment.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AssetKind {
    Result,
    Graph,
    Frame,
    App,
}

impl AssetKind {
    fn of(segment: &str) -> Option<Self> {
        match segment {
            "result" => Some(Self::Result),
            "graph" => Some(Self::Graph),
            "frame" => Some(Self::Frame),
            "app" => Some(Self::App),
            _ => None,
        }
    }
}

/// Why a request was refused. Every variant maps to an HTTP status in the
/// handler; none of them carries attacker-controlled text back out.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum AssetError {
    #[error("not a stratum-asset URL")]
    WrongScheme,
    #[error("unknown asset kind")]
    UnknownKind,
    #[error("path is incomplete")]
    Incomplete,
    #[error("a path segment failed the threat-model screen")]
    BadSegment,
    #[error("malformed percent-encoding")]
    BadEncoding,
}

/// The parse result the handler receives — the `(kind, session, rest)` triple
/// of A21, with `session` still a raw (decoded) segment. `app/{…}` has no
/// session; its whole path is `rest`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ParsedAsset {
    pub kind: AssetKind,
    /// Empty for [`AssetKind::App`].
    pub session: String,
    /// Decoded segments after the session (after the kind, for `app`).
    pub rest: Vec<String>,
    /// The raw query string, if any. Percent-decoding of individual values is
    /// the consumer's job ([`parse_page_query`] does it for frame pages).
    pub query: Option<String>,
}

/// Percent-decode one path segment, exactly once, and screen it (W03):
/// a decoded segment containing `..`, `/`, `\` or NUL is rejected, as is a
/// malformed escape. Decoding twice is how `%252e%252e` becomes `..`; this
/// function is the only decoder and the handler never re-decodes.
pub fn decode_segment(raw: &str) -> Result<String, AssetError> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hi = bytes.get(i + 1).copied().ok_or(AssetError::BadEncoding)?;
                let lo = bytes.get(i + 2).copied().ok_or(AssetError::BadEncoding)?;
                let nibble = |b: u8| -> Result<u8, AssetError> {
                    match b {
                        b'0'..=b'9' => Ok(b - b'0'),
                        b'a'..=b'f' => Ok(b - b'a' + 10),
                        b'A'..=b'F' => Ok(b - b'A' + 10),
                        _ => Err(AssetError::BadEncoding),
                    }
                };
                out.push(nibble(hi)? << 4 | nibble(lo)?);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    let decoded = String::from_utf8(out).map_err(|_| AssetError::BadEncoding)?;
    if decoded.is_empty()
        || decoded.contains("..")
        || decoded.contains('/')
        || decoded.contains('\\')
        || decoded.contains('\0')
    {
        return Err(AssetError::BadSegment);
    }
    Ok(decoded)
}

/// Parse a request URL into the `(kind, session, rest)` triple.
///
/// Accepts both spellings ([`ASSET_SCHEME`] and [`ASSET_HTTP`]); everything
/// after the pinned authority parses identically. This is the property
/// `tests/asset_url_parse.rs` asserts on all three OSes.
///
/// # Errors
/// [`AssetError`] — wrong scheme, unknown kind, too-short path, or a segment
/// that failed [`decode_segment`]'s screen.
pub fn parse(url: &str) -> Result<ParsedAsset, AssetError> {
    let after = url
        .strip_prefix(ASSET_SCHEME)
        .or_else(|| url.strip_prefix(ASSET_HTTP))
        .ok_or(AssetError::WrongScheme)?;

    let (path, query) = match after.split_once('?') {
        Some((p, q)) => (p, Some(q.to_owned())),
        None => (after, None),
    };

    let mut segments = path.split('/').filter(|s| !s.is_empty());
    let kind_raw = segments.next().ok_or(AssetError::Incomplete)?;
    let kind = AssetKind::of(kind_raw).ok_or(AssetError::UnknownKind)?;

    let mut rest: Vec<String> = Vec::new();
    for raw in segments {
        rest.push(decode_segment(raw)?);
    }

    let session = if kind == AssetKind::App {
        String::new()
    } else {
        if rest.is_empty() {
            return Err(AssetError::Incomplete);
        }
        rest.remove(0)
    };

    // Every non-app route has at least one segment after the session.
    if kind != AssetKind::App && rest.is_empty() {
        return Err(AssetError::Incomplete);
    }

    Ok(ParsedAsset {
        kind,
        session,
        rest,
        query,
    })
}

/// A numeric id from a segment that may carry a single alphabetic prefix —
/// `"7"`, `"S1"` and `"R41"` all resolve; `"R41.svg"` resolves after the
/// caller strips its extension.
#[must_use]
pub fn numeric_id(segment: &str) -> Option<u64> {
    let digits = segment.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// Constant-shape token check. Both sides are lowercase hex of 32 bytes; a
/// missing or wrong-length header fails without comparing.
#[must_use]
pub fn token_ok(header: Option<&str>, expected_hex: &str) -> bool {
    let Some(got) = header else { return false };
    if got.len() != expected_hex.len() {
        return false;
    }
    // Length is fixed, comparison covers every byte: no early-exit on content.
    got.bytes()
        .zip(expected_hex.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// The `frame/{s}/{f}/page?…` query, decoded (CONTRACTS §8.1 / A13).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct PageQuery {
    pub state: u64,
    pub row0: u64,
    pub nrows: u32,
    pub cols: Vec<u32>,
    pub order: Option<u32>,
    /// `"display"` or `"edit"`.
    pub render: String,
    pub seq: u32,
}

/// Parse the page query string produced by the frontend's `framePageUrl`
/// (`state=…&row0=…&nrows=…&cols=0,3&render=display&seq=…[&order=…]`).
///
/// # Errors
/// [`AssetError::BadEncoding`] when a numeric field does not parse. Unknown
/// keys are ignored: the URL is also the frontend's cache key and may grow.
pub fn parse_page_query(query: &str) -> Result<PageQuery, AssetError> {
    let mut q = PageQuery {
        render: "display".to_owned(),
        ..PageQuery::default()
    };
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "state" => q.state = value.parse().map_err(|_| AssetError::BadEncoding)?,
            "row0" => q.row0 = value.parse().map_err(|_| AssetError::BadEncoding)?,
            "nrows" => q.nrows = value.parse().map_err(|_| AssetError::BadEncoding)?,
            "seq" => q.seq = value.parse().map_err(|_| AssetError::BadEncoding)?,
            "order" => q.order = Some(value.parse().map_err(|_| AssetError::BadEncoding)?),
            "render" => q.render = value.to_owned(),
            "cols" => {
                q.cols = value
                    .split(',')
                    .filter(|c| !c.is_empty())
                    .map(|c| c.parse().map_err(|_| AssetError::BadEncoding))
                    .collect::<Result<_, _>>()?;
            }
            _ => {}
        }
    }
    Ok(q)
}

/// MIME for a route, from the URL space table in §10.1.
#[must_use]
pub fn mime_for(parsed: &ParsedAsset) -> &'static str {
    match parsed.kind {
        AssetKind::Result => match parsed.rest.last().map(String::as_str) {
            Some("raw") => "text/plain; charset=utf-8",
            _ => "application/octet-stream",
        },
        AssetKind::Graph => match parsed.rest.last().map(String::as_str) {
            Some(name) if name.ends_with(".svg") => "image/svg+xml",
            Some(name) if name.ends_with(".png") => "image/png",
            _ => "application/octet-stream",
        },
        AssetKind::Frame => "application/octet-stream",
        AssetKind::App => match parsed.rest.last().map(String::as_str) {
            Some(name) if name.ends_with(".html") => "text/html; charset=utf-8",
            Some(name) if name.ends_with(".js") || name.ends_with(".mjs") => {
                "text/javascript; charset=utf-8"
            }
            Some(name) if name.ends_with(".css") => "text/css; charset=utf-8",
            Some(name) if name.ends_with(".svg") => "image/svg+xml",
            Some(name) if name.ends_with(".png") => "image/png",
            Some(name) if name.ends_with(".wasm") => "application/wasm",
            Some(name) if name.ends_with(".json") => "application/json",
            _ => "application/octet-stream",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_a21_triple_is_identical_for_both_spellings() {
        for url in [
            "stratum-asset://localhost/graph/S1/R41.svg",
            "http://stratum-asset.localhost/graph/S1/R41.svg",
        ] {
            let p = parse(url).expect(url);
            assert_eq!(p.kind, AssetKind::Graph);
            assert_eq!(p.session, "S1");
            assert_eq!(p.rest, vec!["R41.svg".to_owned()]);
            assert_eq!(p.query, None);
        }
    }

    #[test]
    fn every_route_in_the_10_1_table_parses() {
        let raw = parse("stratum-asset://localhost/result/1/2/raw").unwrap();
        assert_eq!(raw.kind, AssetKind::Result);
        assert_eq!(raw.session, "1");
        assert_eq!(raw.rest, vec!["2", "raw"]);
        assert_eq!(mime_for(&raw), "text/plain; charset=utf-8");

        let table = parse("stratum-asset://localhost/result/1/2/table").unwrap();
        assert_eq!(mime_for(&table), "application/octet-stream");

        let page =
            parse("stratum-asset://localhost/frame/1/default/page?state=17&row0=0&nrows=40&cols=0,3&render=display&seq=1")
                .unwrap();
        assert_eq!(page.kind, AssetKind::Frame);
        assert_eq!(page.session, "1");
        assert_eq!(page.rest, vec!["default", "page"]);
        let q = parse_page_query(page.query.as_deref().unwrap()).unwrap();
        assert_eq!(q.state, 17);
        assert_eq!(q.nrows, 40);
        assert_eq!(q.cols, vec![0, 3]);
        assert_eq!(q.order, None);
        assert_eq!(q.render, "display");

        let app = parse("stratum-asset://localhost/app/assets/main.js").unwrap();
        assert_eq!(app.kind, AssetKind::App);
        assert_eq!(app.session, "");
        assert_eq!(app.rest, vec!["assets", "main.js"]);
    }

    #[test]
    fn traversal_shapes_are_rejected_not_normalised() {
        for url in [
            "stratum-asset://localhost/result/1/../2/raw",
            "stratum-asset://localhost/result/1/%2e%2e/raw",
            "stratum-asset://localhost/result/1/%252e%252e/raw", // double-encoded: decodes once to `%2e%2e`… still screened below
            "stratum-asset://localhost/app/%2e%2e/secret",
            "stratum-asset://localhost/app/a%2fb", // encoded `/`
            "stratum-asset://localhost/app/a%5cb", // encoded `\`
            "stratum-asset://localhost/app/a%00b", // NUL
            "stratum-asset://localhost/result/1/2/%",
        ] {
            let out = parse(url);
            if url.contains("%252e") {
                // One decode of `%252e%252e` yields the literal `%2e%2e`, which
                // contains neither `..` nor a separator — and the handler never
                // decodes twice, so it can only ever be a (missing) literal name.
                assert!(out.is_ok(), "{url}");
                continue;
            }
            assert!(out.is_err(), "{url} must be rejected, got {out:?}");
        }
    }

    #[test]
    fn wrong_scheme_and_short_paths_are_errors() {
        assert_eq!(parse("https://example.com/x"), Err(AssetError::WrongScheme));
        assert_eq!(
            parse("stratum-asset://localhost/result/1"),
            Err(AssetError::Incomplete)
        );
        assert_eq!(
            parse("stratum-asset://localhost/nope/1/2"),
            Err(AssetError::UnknownKind)
        );
    }

    #[test]
    fn the_token_check_needs_exactly_the_expected_hex() {
        let expected = "aa".repeat(32);
        assert!(token_ok(Some(&expected), &expected));
        assert!(!token_ok(None, &expected));
        assert!(!token_ok(Some("aa"), &expected));
        let mut wrong = expected.clone();
        wrong.replace_range(0..1, "b");
        assert!(!token_ok(Some(&wrong), &expected));
    }

    #[test]
    fn numeric_ids_strip_a_single_alpha_prefix() {
        assert_eq!(numeric_id("7"), Some(7));
        assert_eq!(numeric_id("S1"), Some(1));
        assert_eq!(numeric_id("R41"), Some(41));
        assert_eq!(numeric_id("R"), None);
        assert_eq!(numeric_id(""), None);
    }
}
