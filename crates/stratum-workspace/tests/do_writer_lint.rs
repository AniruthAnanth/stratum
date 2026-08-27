//! ARCHITECTURE §6.3 / ADR-010, machine-checked across the whole repository.
//!
//! > `.do` source. Ever. **Exactly four** code paths may write a `.do` file, all
//! > of them inside `stratum-workspace` (W26), all of them reached through one
//! > function `workspace::write_document`, and CI lints for any
//! > `fs::write`/`File::create` with a `.do` path outside that function.
//!
//! W26's acceptance bullet asks for that lint. The obvious home is
//! `scripts/check-topology.sh`, which is W00's file, so it lives here instead —
//! as a test, which is strictly better than a `grep` in a shell script because
//! it can mask out comments and string bodies before it reasons about the source
//! and can therefore afford a rule precise enough to be worth failing CI over.
//! It walks up to the workspace root and scans **every** crate, `xtask` and the
//! Tauri host, not just this crate.
//!
//! # The rule, exactly
//!
//! §6.3 scopes the lint to a *function*: a write sink is a violation when a
//! `.do` path is in scope at the sink. So for every shipped Rust source in the
//! repository we take the innermost enclosing `fn` of each file-opening call and
//! of each `.do`-extension literal, and a function that holds both is an
//! offender unless it lives in `crates/stratum-workspace/src/write.rs`. A
//! module-level `.do` literal counts against every function in its file, because
//! a `const` is in scope throughout.
//!
//! Two things this deliberately does not do. It does not follow a path through a
//! function argument — `fs::write(caller_supplied, ..)` in a crate that never
//! names a `.do` is invisible to it, as it is to any textual lint. And it exempts
//! `tests/`, `benches/` and `#[cfg(test)]` bodies, which must be able to lay down
//! a `.do` fixture; `roundtrip.rs` does nothing else. One harness keeps its test
//! code under `src/` rather than under `tests/` and is exempt by name for the
//! same reason — see [`HARNESS_ONLY`], and the test below it that asserts the
//! one property making that safe. The second half of the file
//! closes most of the first gap for the crate that actually holds document text:
//! inside `stratum-workspace`, `write.rs` is the only module that may open a file
//! at all, whatever the extension.
//!
//! The masker and the scope walker are exercised on planted violations at the
//! bottom, because a lint whose own matcher has rotted reports a clean tree.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// What counts as opening a file for writing, and what counts as a `.do` path
// ---------------------------------------------------------------------------

/// Calls that can *create or replace* a file. Matched against masked source, so
/// the same text inside a string or a doc comment (this list, quoted in a test
/// below) does not match itself.
///
/// `write_all` is deliberately absent: its receiver is a socket at least as
/// often as a file, and a check that fires on every framed-protocol write gets
/// suppressed rather than fixed.
const SINKS: &[&str] = &[
    "fs::write(",
    "File::create(",
    "File::create_new(",
    "OpenOptions",
    "fs::copy(",
    "fs::rename(",
    "fs::hard_link(",
];

/// Does this string-literal body *end* in the `.do` extension?
///
/// A path literal stops at its extension: `"analysis.do"`, `"{name}.do"`,
/// `".do"`. Prose that merely mentions the extension does not — `xtask
/// conformance`'s "no `*.do` case in {corpus}" sits in the same function as a
/// `.jsonl` writer, and treating a diagnostic as a path makes the lint cry wolf
/// on the one crate that legitimately writes a directory full of output.
/// `.doc`, `.dot` and `.done` are not `.do` either, which `ends_with` gives for
/// free.
fn is_do_path_literal(body: &str) -> bool {
    body.trim_end().ends_with(".do")
}

/// Looser: does this *line* name a `.do` path anywhere in it? Only the
/// file-granular frontend scan uses this, where there is no masker to tell a
/// literal from prose.
fn mentions_do_path(line: &str) -> bool {
    let mut rest = line;
    while let Some(at) = rest.find(".do") {
        rest = &rest[at + 3..];
        match rest.chars().next() {
            Some(c) if c.is_ascii_alphanumeric() || c == '_' => {}
            _ => return true,
        }
    }
    false
}

/// `set_extension("do")` / `extension() == Some("do")` name a `.do` path without
/// ever writing the dot.
fn bare_do_extension(masked_line: &str, body: &str) -> bool {
    body == "do" && (masked_line.contains("set_extension") || masked_line.contains("extension"))
}

// ---------------------------------------------------------------------------
// Masking: blank comments and literal bodies, keep every byte offset
// ---------------------------------------------------------------------------

/// Source with comment text and string/char bodies replaced by spaces (newlines
/// survive, so offsets and line numbers are unchanged), plus the bodies that
/// were blanked, keyed by offset.
struct Masked {
    masked: String,
    literals: Vec<(usize, String)>,
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn blank(out: &mut [u8], range: std::ops::Range<usize>) {
    for b in &mut out[range] {
        if *b != b'\n' {
            *b = b' ';
        }
    }
}

fn mask(src: &str) -> Masked {
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut literals = Vec::new();
    let mut i = 0usize;

    while i < b.len() {
        // A raw string swallows quotes, backslashes and braces, so it has to be
        // recognised before any of them.
        if (b[i] == b'r' || b[i] == b'b') && (i == 0 || !is_ident_byte(b[i - 1])) {
            let mut j = i;
            if b[j] == b'b' {
                j += 1;
            }
            if j < b.len() && b[j] == b'r' {
                j += 1;
                let hashes_start = j;
                while j < b.len() && b[j] == b'#' {
                    j += 1;
                }
                if j < b.len() && b[j] == b'"' {
                    let hashes = j - hashes_start;
                    let body_start = j + 1;
                    let close = format!("\"{}", "#".repeat(hashes));
                    let end = src[body_start..]
                        .find(&close)
                        .map(|k| body_start + k)
                        .unwrap_or(b.len());
                    literals.push((body_start, src[body_start..end].to_owned()));
                    blank(&mut out, body_start..end);
                    i = (end + close.len()).min(b.len());
                    continue;
                }
            }
        }

        match b[i] {
            b'/' if i + 1 < b.len() && b[i + 1] == b'/' => {
                let start = i;
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                blank(&mut out, start..i);
            }
            b'/' if i + 1 < b.len() && b[i + 1] == b'*' => {
                let start = i;
                let mut depth = 0usize; // Rust block comments nest.
                while i < b.len() {
                    if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
                        depth += 1;
                        i += 2;
                    } else if b[i] == b'*' && i + 1 < b.len() && b[i + 1] == b'/' {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
                blank(&mut out, start..i);
            }
            b'"' => {
                let body_start = i + 1;
                let mut j = body_start;
                while j < b.len() {
                    match b[j] {
                        b'\\' => j += 2,
                        b'"' => break,
                        _ => j += 1,
                    }
                }
                let end = j.min(b.len());
                literals.push((body_start, src[body_start..end].to_owned()));
                blank(&mut out, body_start..end);
                i = (end + 1).min(b.len());
            }
            b'\'' => {
                // `'a` is a lifetime, `'{'` is a char literal, and only the
                // second one hides a brace from the scope walker.
                let rest = &src[i + 1..];
                let consumed = if let Some(escaped) = rest.strip_prefix('\\') {
                    escaped.find('\'').map(|k| k + 2)
                } else {
                    rest.chars().next().and_then(|c| {
                        let w = c.len_utf8();
                        (rest.as_bytes().get(w) == Some(&b'\'')).then_some(w)
                    })
                };
                match consumed {
                    Some(n) => {
                        literals.push((i + 1, src[i + 1..i + 1 + n].to_owned()));
                        blank(&mut out, i + 1..i + 1 + n);
                        i += n + 2;
                    }
                    None => i += 1,
                }
            }
            _ => i += 1,
        }
    }

    Masked {
        // Every replacement is one ASCII byte for one ASCII byte, so this cannot
        // split a multi-byte character.
        masked: String::from_utf8(out).expect("masking is byte-for-byte ASCII"),
        literals,
    }
}

// ---------------------------------------------------------------------------
// Scopes: `#[cfg(test)]` items to skip, `fn` bodies to attribute a hit to
// ---------------------------------------------------------------------------

/// Byte span of the item that starts at or after `from`: `{`..matching `}`, or
/// `None` when the item ends at a `;` first (a `use`, a bodiless trait method).
fn item_span(masked: &str, from: usize) -> Option<std::ops::Range<usize>> {
    let b = masked.as_bytes();
    let mut i = from;
    while i < b.len() && b[i] != b'{' && b[i] != b';' {
        i += 1;
    }
    if i >= b.len() || b[i] == b';' {
        return None;
    }
    let open = i;
    let mut depth = 0usize;
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open..i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    Some(open..b.len())
}

/// Spans of every `#[cfg(test)]`-gated item. `#[cfg(not(test))]` is shipped code
/// and is not one of them.
fn test_gated_spans(masked: &str) -> Vec<std::ops::Range<usize>> {
    let mut spans: Vec<std::ops::Range<usize>> = Vec::new();
    for (at, _) in masked.match_indices("#[cfg(") {
        let line_end = masked[at..]
            .find('\n')
            .map(|k| at + k)
            .unwrap_or(masked.len());
        let attr = &masked[at..line_end];
        if attr.contains("not(test") || !has_word(attr, "test") {
            continue;
        }
        if spans.iter().any(|s| s.contains(&at)) {
            continue;
        }
        if let Some(span) = item_span(masked, line_end) {
            spans.push(span);
        }
    }
    spans
}

fn has_word(hay: &str, word: &str) -> bool {
    hay.match_indices(word).any(|(at, _)| {
        let before = hay.as_bytes()[..at]
            .last()
            .is_none_or(|b| !is_ident_byte(*b));
        let after = hay
            .as_bytes()
            .get(at + word.len())
            .is_none_or(|b| !is_ident_byte(*b));
        before && after
    })
}

/// Spans of every `fn` body, innermost last when nested.
fn fn_spans(masked: &str) -> Vec<std::ops::Range<usize>> {
    let mut spans = Vec::new();
    for (at, _) in masked.match_indices("fn") {
        if !has_word(&masked[at..(at + 2).min(masked.len())], "fn") {
            continue;
        }
        let before = masked.as_bytes()[..at].last();
        let after = masked.as_bytes().get(at + 2);
        if before.is_some_and(|b| is_ident_byte(*b)) || after.is_some_and(|b| is_ident_byte(*b)) {
            continue;
        }
        if let Some(span) = item_span(masked, at + 2) {
            spans.push(span);
        }
    }
    spans
}

/// The innermost `fn` body containing `offset`, identified by its start offset.
/// `None` means module level.
fn enclosing_fn(spans: &[std::ops::Range<usize>], offset: usize) -> Option<usize> {
    spans
        .iter()
        .filter(|s| s.contains(&offset))
        .min_by_key(|s| s.end - s.start)
        .map(|s| s.start)
}

// ---------------------------------------------------------------------------
// The analysis
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Hit {
    offset: usize,
    line: usize,
    text: String,
}

struct Analysis {
    sinks: Vec<Hit>,
    do_paths: Vec<Hit>,
}

fn line_of(src: &str, offset: usize) -> (usize, &str) {
    let start = src[..offset].rfind('\n').map(|k| k + 1).unwrap_or(0);
    let end = src[offset..]
        .find('\n')
        .map(|k| offset + k)
        .unwrap_or(src.len());
    (src[..offset].matches('\n').count() + 1, &src[start..end])
}

fn analyse(src: &str) -> Analysis {
    let Masked { masked, literals } = mask(src);
    let skip = test_gated_spans(&masked);
    let shipped = |o: &usize| !skip.iter().any(|s| s.contains(o));

    let mut sinks = Vec::new();
    for sink in SINKS {
        for (at, _) in masked.match_indices(sink) {
            if shipped(&at) {
                let (line, text) = line_of(src, at);
                sinks.push(Hit {
                    offset: at,
                    line,
                    text: text.trim().to_owned(),
                });
            }
        }
    }

    let mut do_paths = Vec::new();
    for (at, body) in &literals {
        if !shipped(at) {
            continue;
        }
        let (line, text) = line_of(src, *at);
        let (_, masked_line) = line_of(&masked, *at);
        if is_do_path_literal(body) || bare_do_extension(masked_line, body) {
            do_paths.push(Hit {
                offset: *at,
                line,
                text: text.trim().to_owned(),
            });
        }
    }

    sinks.sort_by_key(|h| h.offset);
    do_paths.sort_by_key(|h| h.offset);
    Analysis { sinks, do_paths }
}

/// §6.3, per function: a `fn` that both opens a file and names a `.do` path.
fn do_writers(src: &str) -> Vec<String> {
    let Analysis { sinks, do_paths } = analyse(src);
    if sinks.is_empty() || do_paths.is_empty() {
        return Vec::new();
    }
    let spans = fn_spans(&mask(src).masked);

    // A `.do` const at module level is in scope in every function of the file.
    let module_level: Vec<&Hit> = do_paths
        .iter()
        .filter(|h| enclosing_fn(&spans, h.offset).is_none())
        .collect();

    let mut out = Vec::new();
    for sink in &sinks {
        let scope = enclosing_fn(&spans, sink.offset);
        let mut named: Vec<&Hit> = do_paths
            .iter()
            .filter(|h| scope.is_some() && enclosing_fn(&spans, h.offset) == scope)
            .collect();
        named.extend(module_level.iter().copied());
        if named.is_empty() {
            continue;
        }
        let paths = named
            .iter()
            .map(|h| format!("line {} `{}`", h.line, h.text))
            .collect::<Vec<_>>()
            .join(", ");
        out.push(format!(
            "line {}: `{}` is in the same function as a `.do` path ({paths})",
            sink.line, sink.text
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Walking the repository
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    let mut dir = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    loop {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file()
            && fs::read_to_string(&manifest)
                .unwrap_or_default()
                .lines()
                .any(|l| l.trim() == "[workspace]")
        {
            return dir;
        }
        assert!(
            dir.pop(),
            "no [workspace] Cargo.toml above {}",
            env!("CARGO_MANIFEST_DIR")
        );
    }
}

/// Directories with no first-party source in them. `.stratum` and `gen` hold
/// generated trees; the rest are build output and vendored code.
const SKIP_DIRS: &[&str] = &["target", "node_modules", ".git", "dist", "gen", ".stratum"];

fn walk(dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) {
                walk(&path, exts, out);
            }
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| exts.contains(&e))
        {
            out.push(path);
        }
    }
}

/// Rust sources that ship. Test trees are exempt by design (see the module
/// docs); `#[cfg(test)]` bodies are dropped later, inside [`analyse`].
fn shipped_rust(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk(root, &["rs"], &mut files);
    files.retain(|p| {
        !p.strip_prefix(root).unwrap_or(p).components().any(|c| {
            matches!(
                c.as_os_str().to_string_lossy().as_ref(),
                "tests" | "benches" | "examples"
            )
        })
    });
    files.sort();
    files
}

fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// The one sanctioned writer (ARCHITECTURE §6.3).
const SANCTIONED: &str = "crates/stratum-workspace/src/write.rs";

/// Test harnesses that live under `src/` rather than under `tests/`, and are
/// exempt for the same reason the module docs already exempt `tests/`,
/// `benches/` and `#[cfg(test)]` bodies: a harness has to be able to lay down a
/// `.do` file.
///
/// Exactly one entry, and the justification is specific rather than a category.
/// `stratum-difftest` (W23, ADR-013) drives Stata as an *oracle*: per committed
/// case it copies `tests/difftest/cases/<case>/case.do` and `prologue.do` into a
/// fresh `tempfile::tempdir()`, writes a two-line `driver.do` beside them, and
/// runs Stata on the result. Every one of those paths is the harness's own
/// scaffolding inside a directory it created and drops; none of them is, or can
/// become, a document a researcher opened. §6.3 exists so the product never
/// rewrites the user's source, and this crate is not in the product — it is
/// absent from `[workspace] default-members`, and `cargo xtask layering` asserts
/// that list against `cargo metadata` in both directions.
///
/// The exemption is only as safe as that last sentence, so
/// `nothing_in_the_workspace_links_an_exempt_harness` below asserts the property
/// it rests on rather than trusting it: no other manifest names the crate, so
/// none of these sinks is reachable from any build a user runs.
///
/// `xtask` is deliberately NOT here, although it is excluded from
/// `default-members` too. It is the tool contributors run *against a real
/// checkout*, so a `.do` writer in it would write into somebody's working tree —
/// which is the thing §6.3 forbids, whoever the somebody is.
const HARNESS_ONLY: &[&str] = &["crates/stratum-difftest/"];

// ---------------------------------------------------------------------------
// The lints
// ---------------------------------------------------------------------------

#[test]
fn write_rs_is_the_only_module_in_the_workspace_that_writes_a_do_file() {
    let root = repo_root();
    let files = shipped_rust(&root);

    // A walk that silently found nothing is the failure mode that makes a lint
    // like this worthless, so the scan's own reach is asserted first.
    assert!(
        files.len() > 40,
        "scanned only {} rust files from {} — the walk is broken, not the tree clean",
        files.len(),
        root.display()
    );
    let seen: BTreeSet<String> = files.iter().map(|p| rel(&root, p)).collect();
    for expected in [
        SANCTIONED,
        "crates/stratum-proto/src/lib.rs",
        "xtask/src/main.rs",
    ] {
        assert!(
            seen.contains(expected),
            "the walk missed {expected}; scan is misconfigured"
        );
    }

    // The exempt harnesses are walked, not skipped, so the reach assertion above
    // stays honest about what the walk sees.
    for prefix in HARNESS_ONLY {
        assert!(
            seen.iter().any(|f| f.starts_with(prefix)),
            "{prefix} is exempt from this lint but the walk found nothing under it — \
             an exemption that names a path which no longer exists is a hole waiting \
             for a rename to widen it"
        );
    }

    let mut offenders = Vec::new();
    for path in &files {
        let name = rel(&root, path);
        if name == SANCTIONED || HARNESS_ONLY.iter().any(|p| name.starts_with(p)) {
            continue;
        }
        let src = fs::read_to_string(path).unwrap_or_default();
        for hit in do_writers(&src) {
            offenders.push(format!("{name}:{hit}"));
        }
    }
    assert!(
        offenders.is_empty(),
        "ARCHITECTURE §6.3: only `{SANCTIONED}`'s `write_document` may write a `.do` file.\n{}",
        offenders.join("\n")
    );
}

/// What makes [`HARNESS_ONLY`] safe, asserted rather than assumed.
///
/// A `.do` writer in a crate nothing links is unreachable from every build a
/// user runs. The day something links it, the exemption stops being about a
/// test harness and starts being a hole in §6.3 — so the dependency edge is what
/// this test forbids, and it names the exemption in its own failure.
#[test]
fn nothing_in_the_workspace_links_an_exempt_harness() {
    let root = repo_root();
    let mut manifests = Vec::new();
    walk(&root, &["toml"], &mut manifests);
    manifests.retain(|p| p.file_name().is_some_and(|n| n == "Cargo.toml"));
    manifests.sort();
    assert!(
        manifests.len() > 10,
        "found only {} manifests; the walk is broken, not the workspace small",
        manifests.len()
    );

    for prefix in HARNESS_ONLY {
        // `crates/stratum-difftest/` -> `stratum-difftest`, the name a
        // dependency edge would have to spell.
        let crate_name = prefix
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .expect("a name");
        let mut linkers = Vec::new();
        for manifest in &manifests {
            let name = rel(&root, manifest);
            if name.starts_with(prefix) {
                continue;
            }
            let src = fs::read_to_string(manifest).unwrap_or_default();
            // A dependency entry, not a mention: every reference to this crate
            // in another manifest today is prose in a comment explaining why the
            // edge does not exist, and a `contains` would fire on all of them.
            if src.lines().map(str::trim).any(|l| {
                l.strip_prefix(crate_name)
                    .is_some_and(|r| r.trim_start().starts_with('='))
            }) {
                linkers.push(name);
            }
        }
        assert!(
            linkers.is_empty(),
            "{crate_name} is exempt from the §6.3 `.do`-writer lint because nothing links \
             it, and now {linkers:?} does. Either drop the edge or drop the exemption — \
             the writers in {prefix} stage throwaway do-files to drive Stata and are not \
             safe in anything a user can run."
        );
    }
}

#[test]
fn write_rs_is_the_only_module_in_this_crate_that_opens_a_file() {
    let root = repo_root();
    let mut files = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &["rs"],
        &mut files,
    );
    files.sort();
    assert!(
        files.len() > 5,
        "scanned only {} modules; the walk is broken",
        files.len()
    );

    let mut offenders = Vec::new();
    for path in &files {
        let name = rel(&root, path);
        if name == SANCTIONED {
            continue;
        }
        let src = fs::read_to_string(path).unwrap();
        for hit in analyse(&src).sinks {
            offenders.push(format!("{name}:{}: {}", hit.line, hit.text));
        }
    }
    // Stricter than §6.3 on purpose: this crate is the one that holds document
    // text, so a fifth writer would be added *here*, by somebody who reached for
    // `fs::write` with the text already in hand. Sidecars, layout and keymap all
    // land through `write::write_bytes_atomic`.
    assert!(
        offenders.is_empty(),
        "only `write.rs` may open a file in stratum-workspace; route it through \
         `write::write_bytes_atomic`.\n{}",
        offenders.join("\n")
    );
}

/// The frontend half, coarser on purpose: file-granular, and comments are *not*
/// stripped, so the check errs towards a false alarm a human resolves rather
/// than a silent miss. The frontend reaches the filesystem only through the
/// Tauri plugin, and §8.3 already pins those imports to `platform/bridge.ts`.
#[test]
fn the_frontend_never_writes_a_do_file() {
    const TS_SINKS: &[&str] = &[
        "writeTextFile",
        "writeBinaryFile",
        "writeFile(",
        "createWriteStream",
    ];

    let root = repo_root();
    let mut files = Vec::new();
    walk(&root.join("apps/desktop/src"), &["ts", "tsx"], &mut files);
    files.retain(|p| !p.to_string_lossy().contains(".test."));
    files.sort();

    let mut offenders = Vec::new();
    for path in &files {
        let src = fs::read_to_string(path).unwrap_or_default();
        if !TS_SINKS.iter().any(|s| src.contains(s)) {
            continue;
        }
        for (n, line) in src.lines().enumerate() {
            if mentions_do_path(line) {
                offenders.push(format!("{}:{}: {}", rel(&root, path), n + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a `.do` file is written by `stratum_workspace::write_document`, never by the \
         frontend (ARCHITECTURE §6.3).\n{}",
        offenders.join("\n")
    );
}

// ---------------------------------------------------------------------------
// The lint's own teeth
// ---------------------------------------------------------------------------

#[test]
fn the_lint_catches_a_planted_fifth_writer() {
    let planted = r#"
        pub fn export(dir: &Path) -> std::io::Result<()> {
            let out = dir.join("notebook.do");
            std::fs::write(&out, render_with_results())
        }
    "#;
    let hits = do_writers(planted);
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(hits[0].contains("notebook.do"), "{hits:?}");
}

#[test]
fn the_lint_catches_a_writer_that_builds_the_extension() {
    let planted = r#"
        fn stash(base: &Path) -> std::io::Result<()> {
            let mut p = base.to_path_buf();
            p.set_extension("do");
            let mut f = std::fs::File::create(&p)?;
            f.write_all(b"list\n")
        }
    "#;
    assert_eq!(do_writers(planted).len(), 1);
}

#[test]
fn the_lint_catches_a_module_level_do_constant() {
    let planted = r#"
        const ENTRY: &str = "analysis.do";
        fn save(root: &Path) -> std::io::Result<()> {
            std::fs::write(root.join(ENTRY), "")
        }
    "#;
    assert_eq!(do_writers(planted).len(), 1);
}

#[test]
fn the_lint_does_not_fire_on_an_unrelated_writer_in_the_same_file() {
    // `mock_engine.rs` is exactly this shape: it names a `.do` in a fabricated
    // event and, hundreds of lines away, opens an SDP1 segment. Function scope
    // is what keeps that from being a false alarm.
    let benign = r#"
        fn event() -> Event {
            Event { source: Utf8PathBuf::from("auto.do") }
        }
        fn segment(dir: &Path) -> std::io::Result<File> {
            std::fs::OpenOptions::new().write(true).create(true).open(dir.join("seg.bin"))
        }
    "#;
    assert!(do_writers(benign).is_empty());
}

#[test]
fn the_lint_ignores_comments_and_doc_comments() {
    let benign = r#"
        /// Never `fs::write("x.do", ..)`; go through `write_document`.
        // let _ = std::fs::write("y.do", "");
        fn f() {}
    "#;
    let a = analyse(benign);
    assert!(a.sinks.is_empty(), "{:?}", a.sinks);
    assert!(a.do_paths.is_empty(), "{:?}", a.do_paths);
}

#[test]
fn the_lint_ignores_a_cfg_test_module_but_not_the_code_above_it() {
    let src = r#"
        fn ship(p: &Path) -> std::io::Result<()> { std::fs::write(p.join("a.do"), "") }
        #[cfg(test)]
        mod tests {
            fn fixture(p: &Path) { std::fs::write(p.join("b.do"), "").unwrap(); }
        }
    "#;
    let hits = do_writers(src);
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(
        hits[0].contains("a.do") && !hits[0].contains("b.do"),
        "{hits:?}"
    );
}

#[test]
fn the_lint_is_not_fooled_by_a_brace_inside_a_literal() {
    // A `'}'` char literal or a `"}"` string inside the test module would end
    // the skipped span early and leak the fixture write back into the scan.
    let src = r#"
        #[cfg(test)]
        mod tests {
            const CLOSE: char = '}';
            const ALSO: &str = "}";
            fn fixture(p: &Path) { std::fs::write(p.join("b.do"), "").unwrap(); }
        }
    "#;
    assert!(do_writers(src).is_empty(), "{:?}", do_writers(src));
}

#[test]
fn the_lint_is_not_fooled_by_a_raw_string() {
    let src = r####"
        #[cfg(test)]
        mod tests {
            const SNIPPET: &str = r#"if x { "}" } // std::fs::write("q.do", "")"#;
            fn fixture(p: &Path) { std::fs::write(p.join("b.do"), "").unwrap(); }
        }
    "####;
    assert!(do_writers(src).is_empty(), "{:?}", do_writers(src));
}

#[test]
fn only_a_real_do_extension_counts() {
    assert!(is_do_path_literal("analysis.do"));
    assert!(is_do_path_literal("/p/01 clean.do"));
    assert!(is_do_path_literal("{name}.do"));
    assert!(is_do_path_literal(".do"));
    assert!(!is_do_path_literal("report.docx"));
    assert!(!is_do_path_literal("notes.dot"));
    assert!(!is_do_path_literal("everything.done"));
    assert!(!is_do_path_literal("stratum-asset://localhost/result"));
}

#[test]
fn a_diagnostic_that_mentions_do_files_is_not_a_do_path() {
    // The live case: `xtask conformance`'s `run` writes `{name}.jsonl` into
    // `--out` and, forty lines earlier, says the corpus holds no `*.do` case.
    let benign = r#"
        fn run(out: &Path, name: &str, body: &str) -> anyhow::Result<()> {
            anyhow::ensure!(!cases.is_empty(), "no `*.do` case in {corpus}");
            std::fs::write(out.join(format!("{name}.jsonl")), body)?;
            Ok(())
        }
    "#;
    assert!(do_writers(benign).is_empty(), "{:?}", do_writers(benign));

    // …but the same function writing `{name}.do` is the violation §6.3 names.
    let planted = benign.replace("{name}.jsonl", "{name}.do");
    assert_eq!(do_writers(&planted).len(), 1);
}
