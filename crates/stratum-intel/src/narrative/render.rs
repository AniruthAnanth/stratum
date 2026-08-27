//! The rendering **security boundary** — design 07 §9.4.
//!
//! Document View is a decoration layer over the same buffer, so "rendering" is
//! two separable jobs: turning Markdown into events, and deciding what an event
//! is allowed to do. This module is the second one, and it is the half that
//! belongs in a deterministic, offline crate: every decision here is a pure
//! function of a string, with no network, no filesystem and no ambiguity.
//!
//! The threat model is concrete. Do-files arrive from co-authors, replication
//! archives and journal supplements. The UI layer is a webview. Interpreting
//! HTML, or following an arbitrary link, or loading a remote image, from a `.do`
//! file is a straightforward path from "opened a colleague's replication
//! package" to XSS-in-the-desktop-app or a tracking pixel that fires when the
//! researcher opens the file. §9.4 calls this "not negotiable".
//!
//! | Input | Verdict |
//! |---|---|
//! | raw HTML | [`escape_html`] — escaped and rendered **literally**, never interpreted |
//! | `http:` / `https:` link | [`LinkVerdict::External`] — opens in the system browser after a confirmation showing the full URL |
//! | `file:` or relative link | [`LinkVerdict::ProjectFile`] only when it resolves **inside the project root** |
//! | `javascript:`, `data:`, `vbscript:`, UNC `\\host\share` | [`LinkVerdict::Inert`] — rendered as text |
//! | remote image | [`LinkVerdict::Inert`] — no remote image loading, ever |
//!
//! # What this module deliberately does not do
//!
//! It does not parse CommonMark. §9.4 fixes the parser as `pulldown-cmark` with
//! `default-features = false` and `ENABLE_HTML` off **at compile time**, and the
//! event stream it produces is consumed by the view layer. Putting a second,
//! hand-rolled Markdown parser here would be the same mistake as a second lexer:
//! two implementations of one grammar that can disagree. The view layer feeds
//! every `Event::Html`, `Tag::Link` and `Tag::Image` it sees through the
//! functions below, and that is where the boundary is enforced.

use camino::{Utf8Path, Utf8PathBuf};

/// What may be done with a link or image target.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LinkVerdict {
    /// An `http`/`https` URL. Open in the system browser **after** showing the
    /// user the full URL and getting a confirmation.
    External(String),
    /// A project-relative path that resolves inside the project root.
    ProjectFile(Utf8PathBuf),
    /// Render the target as literal text and do nothing else.
    Inert,
}

/// Classify a link target.
///
/// `doc_dir` is the directory of the do-file the narrative lives in, and
/// `project_root` bounds what a `file:` target may reach. Both are supplied by
/// the caller: this crate resolves paths lexically and never asks the
/// filesystem whether they exist.
#[must_use]
pub fn classify_link(href: &str, doc_dir: &Utf8Path, project_root: &Utf8Path) -> LinkVerdict {
    let trimmed = href.trim();
    if trimmed.is_empty() {
        return LinkVerdict::Inert;
    }
    // Scheme detection is case-insensitive and tolerant of the whitespace and
    // control characters that browsers historically stripped before dispatching
    // — `java\nscript:` is the classic bypass.
    let squeezed: String = trimmed
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect::<String>()
        .to_ascii_lowercase();

    if squeezed.starts_with("http://") || squeezed.starts_with("https://") {
        return LinkVerdict::External(trimmed.to_owned());
    }
    // Everything else with a scheme is inert. An allowlist, not a blocklist:
    // `javascript:`, `data:`, `vbscript:` and the next one nobody has thought
    // of yet all land here without being named.
    if has_scheme(&squeezed) && !squeezed.starts_with("file:") {
        return LinkVerdict::Inert;
    }
    // UNC paths reach the network on Windows.
    if trimmed.starts_with("\\\\") || trimmed.starts_with("//") {
        return LinkVerdict::Inert;
    }
    let rel = squeezed
        .strip_prefix("file://")
        .or_else(|| squeezed.strip_prefix("file:"))
        .map_or(trimmed, |_| {
            trimmed
                .trim_start()
                .trim_start_matches("file://")
                .trim_start_matches("file:")
        });
    resolve_inside(rel, doc_dir, project_root).map_or(LinkVerdict::Inert, LinkVerdict::ProjectFile)
}

/// Classify an image target. Stricter than a link: **only** project-relative
/// files. A remote `<img>` in a shared do-file is a tracking pixel.
#[must_use]
pub fn classify_image(src: &str, doc_dir: &Utf8Path, project_root: &Utf8Path) -> LinkVerdict {
    match classify_link(src, doc_dir, project_root) {
        LinkVerdict::ProjectFile(p) => LinkVerdict::ProjectFile(p),
        _ => LinkVerdict::Inert,
    }
}

fn has_scheme(squeezed: &str) -> bool {
    match squeezed.find(':') {
        // A Windows drive letter is not a scheme.
        Some(1) => false,
        Some(i) => squeezed.get(..i).is_some_and(|s| {
            s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        }),
        None => false,
    }
}

/// Lexically resolve `rel` against `doc_dir` and require the result to stay
/// inside `project_root`.
///
/// Lexical, not canonical, because this crate cannot ask the filesystem to
/// resolve a symlink — and because a lexical check that refuses `../../etc` is
/// the right answer regardless of what the filesystem would have said.
fn resolve_inside(rel: &str, doc_dir: &Utf8Path, project_root: &Utf8Path) -> Option<Utf8PathBuf> {
    let rel = rel.split(['?', '#']).next().unwrap_or(rel);
    if rel.is_empty() {
        return None;
    }
    // Absoluteness on the bytes, not `Utf8Path::is_absolute`, which answers for
    // the build host — false for `/x` on Windows, false for `C:\x` on Unix —
    // and a do-file written on one OS is opened on the other. `\x` and `C:x`
    // are refused everywhere for the same reason: on Windows they re-anchor
    // rather than descend, and a boundary that is only right on one host is
    // not a boundary.
    let b = rel.as_bytes();
    if matches!(b, [b'/' | b'\\', ..]) || matches!(b, [c, b':', ..] if c.is_ascii_alphabetic()) {
        return None;
    }
    // Both separators on every host, matching the `rel` split below: `doc_dir`
    // is spelled with `\` when the workspace runs on Windows, and splitting it
    // on `/` alone would leave every project-relative link Inert there.
    let mut parts: Vec<&str> = doc_dir
        .as_str()
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    for part in rel.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    // The verdict spells its separators as `/` whatever `doc_dir` used; every
    // consumer of a ProjectFile path (the webview, std) accepts that.
    let joined = Utf8PathBuf::from(parts.join("/"));
    let root: Vec<&str> = project_root
        .as_str()
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    if joined
        .as_str()
        .split('/')
        .filter(|s| !s.is_empty())
        .take(root.len())
        .eq(root.iter().copied())
    {
        Some(joined)
    } else {
        None
    }
}

/// Escape the five characters that let text become markup.
///
/// Applied to every raw-HTML event so the source is shown as written. The
/// Markdown parser is compiled with HTML disabled, so this is the second layer
/// rather than the only one.
#[must_use]
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    fn dirs() -> (Utf8PathBuf, Utf8PathBuf) {
        (
            Utf8PathBuf::from("/proj/analysis"),
            Utf8PathBuf::from("/proj"),
        )
    }

    #[test]
    fn http_links_are_external_and_need_confirmation() {
        let (d, r) = dirs();
        assert_eq!(
            classify_link("https://example.org/x", &d, &r),
            LinkVerdict::External("https://example.org/x".to_owned())
        );
    }

    #[test]
    fn script_schemes_are_inert_however_they_are_spelled() {
        let (d, r) = dirs();
        for hostile in [
            "javascript:alert(1)",
            "JavaScript:alert(1)",
            "java\nscript:alert(1)",
            "  javascript:alert(1)  ",
            "data:text/html;base64,PHNjcmlwdD4=",
            "vbscript:msgbox",
            "\\\\attacker\\share\\x",
        ] {
            assert_eq!(
                classify_link(hostile, &d, &r),
                LinkVerdict::Inert,
                "{hostile}"
            );
        }
    }

    #[test]
    fn a_project_relative_file_resolves_and_an_escaping_one_does_not() {
        let (d, r) = dirs();
        assert_eq!(
            classify_link("figures/fig1.png", &d, &r),
            LinkVerdict::ProjectFile(Utf8PathBuf::from("proj/analysis/figures/fig1.png"))
        );
        assert_eq!(
            classify_link("../raw/data.csv", &d, &r),
            LinkVerdict::ProjectFile(Utf8PathBuf::from("proj/raw/data.csv"))
        );
        assert_eq!(
            classify_link("../../etc/passwd", &d, &r),
            LinkVerdict::Inert
        );
        assert_eq!(classify_link("/etc/passwd", &d, &r), LinkVerdict::Inert);
    }

    /// Every spelling here parses differently on the two hosts —
    /// `is_absolute("/x")` is false on Windows, and on Unix `C:\x` is one
    /// ordinary filename — so these assertions are host-independent on purpose:
    /// they hold (and the byte-oriented check is exercised) wherever the tests
    /// run, with real Windows being the host the refusals protect.
    #[test]
    fn windows_spellings_of_absolute_paths_are_inert_on_every_host() {
        let (d, r) = dirs();
        for hostile in [
            "C:\\evil.do",
            "c:/evil.do",
            "C:evil.do",
            "\\etc\\passwd",
            "/etc/passwd",
            "file:///etc/passwd",
        ] {
            assert_eq!(
                classify_link(hostile, &d, &r),
                LinkVerdict::Inert,
                "{hostile}"
            );
        }
    }

    /// What the workspace actually supplies on Windows. String literals, so the
    /// case runs on every host; under a `/`-only split every project-relative
    /// link would be Inert here.
    #[test]
    fn a_windows_doc_dir_resolves_project_relative_links() {
        let d = Utf8PathBuf::from(r"C:\proj\analysis");
        let r = Utf8PathBuf::from(r"C:\proj");
        assert_eq!(
            classify_link("figures/fig1.png", &d, &r),
            LinkVerdict::ProjectFile(Utf8PathBuf::from("C:/proj/analysis/figures/fig1.png"))
        );
        assert_eq!(
            classify_link(r"figures\fig1.png", &d, &r),
            LinkVerdict::ProjectFile(Utf8PathBuf::from("C:/proj/analysis/figures/fig1.png"))
        );
        assert_eq!(
            classify_link("../../etc/passwd", &d, &r),
            LinkVerdict::Inert
        );
    }

    #[test]
    fn images_are_never_remote() {
        let (d, r) = dirs();
        assert_eq!(
            classify_image("https://tracker.example/pixel.gif", &d, &r),
            LinkVerdict::Inert
        );
        assert!(matches!(
            classify_image("figures/fig1.png", &d, &r),
            LinkVerdict::ProjectFile(_)
        ));
    }

    #[test]
    fn html_is_escaped_and_never_interpreted() {
        assert_eq!(
            escape_html("<script>alert(\"x\" & 'y')</script>"),
            "&lt;script&gt;alert(&quot;x&quot; &amp; &#39;y&#39;)&lt;/script&gt;"
        );
    }

    #[test]
    fn a_windows_drive_letter_is_not_a_scheme() {
        assert!(!has_scheme("c:/users/x"));
        assert!(has_scheme("https://x"));
        assert!(has_scheme("javascript:x"));
    }
}
