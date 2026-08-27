//! The second half of W10's `MenuHost::accelerator` acceptance bullet:
//! "**A CI grep asserts no U+2318 (Command) glyph and no literal Ctrl-plus
//! prefix exists anywhere under `apps/desktop/src`.**" The two banned strings
//! are spelled as an escape and a `concat!` below so that this file could sit
//! inside the scanned root without tripping its own rule.
//!
//! WHY the assertion lives here, in the trait crate, rather than in
//! `scripts/check-topology.sh`: that script implements the numbered
//! ARCHITECTURE §8 invariants and is W00's file (R0). This one is not a §8
//! item — it is the machine-checkable half of 08 §5.4 and §12's "UI design must
//! not hardcode accelerator glyphs; it calls `MenuHost::accelerator(ActionId,
//! KeymapPreset)`". The rule exists to protect *this* trait's monopoly, so the
//! test belongs beside the trait. `cargo nextest run --workspace` runs on all
//! three CI runners, so this is a CI check on every platform, not just macOS.
//!
//! WHY a text scan and not something smarter: the failure mode is a *second*
//! source of truth for the glyph — a tooltip, a command-palette row or a
//! docstring that spells a chord out while the native menu beside it says
//! something else, because the two were rendered by different code. A type
//! cannot see that; only the source text can.
//!
//! Deliberately no carve-out for `*.test.ts` or for comments. The plan bolds
//! "anywhere", and a scan with exceptions grows exceptions. The Rust side is
//! free to spell the glyphs out — `menus.rs` and `tests/accelerators.rs` do,
//! because they are the authority being asserted against. The frontend is the
//! consumer, and a consumer that knows the answer has stopped asking.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// U+2318 PLACE OF INTEREST SIGN, written as an escape so that this file is
/// not itself a hit if the scanned root is ever widened.
const COMMAND_GLYPH: char = '\u{2318}';

/// The other half of the ban, assembled rather than written literally, for the
/// same reason. `concat!` so it stays a `&'static str`.
const CONTROL_PREFIX: &str = concat!("Ctrl", "+");

fn repo_root() -> PathBuf {
    // <root>/crates/stratum-platform/Cargo.toml -> <root>
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("CARGO_MANIFEST_DIR is always <root>/crates/<crate>")
        .to_path_buf()
}

/// Every file under `dir`, depth-first. `std::fs` rather than `walkdir`: a
/// dev-dependency in the crate whose acceptance bullet is "zero deps" is a bad
/// trade for twenty lines.
fn source_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries =
            std::fs::read_dir(&d).unwrap_or_else(|e| panic!("reading {}: {e}", d.display()));
        for entry in entries {
            let entry =
                entry.unwrap_or_else(|e| panic!("reading an entry of {}: {e}", d.display()));
            let path = entry.path();
            // `file_type` does not follow symlinks, so a link loop cannot hang
            // the walk; a symlinked file is skipped because its target is
            // either inside the tree already or outside this unit's remit.
            let ty = entry
                .file_type()
                .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()));
            if ty.is_dir() {
                stack.push(path);
            } else if ty.is_file() {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn the_frontend_hardcodes_no_accelerator_glyph() {
    let root = repo_root();
    let scanned = root.join("apps").join("desktop").join("src");
    if !scanned.is_dir() {
        // Same contract as scripts/check-topology.sh: green on a tree where the
        // target has not landed yet, with teeth the moment it does.
        eprintln!("skipped: {} does not exist yet", scanned.display());
        return;
    }

    let mut hits = String::new();
    let mut count = 0usize;

    // No size cap and no extension filter. Every file under this root is
    // hand-written source, and a check that skips a class of file is a check
    // with a hole exactly where someone would put the literal to hide it.
    for file in source_files(&scanned) {
        // Lossy: a stray binary under src/ must fail this test loudly on its
        // contents, never panic on its encoding.
        let text = String::from_utf8_lossy(
            &std::fs::read(&file).unwrap_or_else(|e| panic!("reading {}: {e}", file.display())),
        )
        .into_owned();
        if !text.contains(COMMAND_GLYPH) && !text.contains(CONTROL_PREFIX) {
            continue;
        }
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .display()
            .to_string();
        for (n, line) in text.lines().enumerate() {
            let glyph = line.contains(COMMAND_GLYPH);
            let word = line.contains(CONTROL_PREFIX);
            if !glyph && !word {
                continue;
            }
            count += 1;
            let what = match (glyph, word) {
                (true, true) => "both",
                (true, false) => "the Command glyph",
                _ => concat!("a literal ", "Ctrl", "+ prefix"),
            };
            let _ = writeln!(hits, "  {rel}:{} — {what}: {}", n + 1, line.trim());
        }
    }

    assert!(
        hits.is_empty(),
        "{count} accelerator literal(s) under apps/desktop/src.\n{hits}\n\
         The host owns the string a menu item displays (08 §5.4, §12; CONTRACTS \
         §11 `menu_accelerator`). The frontend asks \
         `bridge().invoke(\"menu_accelerator\", {{ action, preset }})`, which is \
         `MenuHost::accelerator(ActionId, KeymapPreset)`, and renders whatever it \
         is handed. A local glyph table is a second answer to the same question, \
         and the user finds the disagreement between a tooltip and the native \
         menu beside it in the first minute.\n\
         apps/desktop/src/keys/** and apps/desktop/src/ui/** are W12's files; \
         W10 owns the trait and this assertion, not the fix. The three shapes \
         this test finds, and what each one is asking for:\n\
         (1) a glyph/word table plus a local renderer in keys/accelerator.ts — \
         delete it. `Accelerator::display(PlatformId)` in stratum-platform \
         already emits exactly these strings, and `menu_accelerator` returns \
         `string | null` where `null` means \"no accelerator\", which the UI \
         already renders as nothing. A detached bridge (`pnpm dev` in a browser \
         tab, every vitest run) has no host and therefore has no answer: the \
         honest stub result is `null`, not a second table that will drift.\n\
         (2) a chord spelled out in a doc comment — reword it; a comment that \
         names a glyph is the second source of truth arriving early.\n\
         (3) a modifier in *binding syntax* in a `.test.ts` — `parseKeystroke` \
         accepts \"Control\" as an exact synonym (keys/trie.ts, the `\"ctrl\" | \
         \"control\"` arm), so spelling the fixture that way parses to the same \
         `Keystroke` and is not the banned prefix. No carve-out is needed and \
         none should be added: a scan with exceptions grows exceptions, and \
         narrowing this one would still leave (1) and (2) red."
    );
}
