//! Byte fidelity — ARCHITECTURE A24, plan W26's second and third acceptance
//! bullets.
//!
//! The claim under test is spec §5's: a `.do` written by Stratum is "portable,
//! version-control friendly". That claim is false the moment saving a file
//! rewrites lines the user did not touch, and it is false in a way nobody
//! notices until a collaborator opens a 400-line diff for a one-word change.

use camino::Utf8PathBuf;
use stratum_proto::{Edit, Span};
use stratum_workspace::bytes::{decode, encode, DocBytes, Eol};
use stratum_workspace::keymap::KeymapStore;
use stratum_workspace::layout::LayoutStore;
use stratum_workspace::project::Project;
use stratum_workspace::Workspace;

mod common;
use common::{lines_differing, project_at, tmp};

/// A do-file as Stata for Windows writes one: UTF-8 BOM, CRLF throughout.
const WINDOWS_DO: &[u8] = b"\xef\xbb\xbf// %% Setup\r\n\
sysuse auto, clear\r\n\
\r\n\
// %% Model\r\n\
regress price mpg weight\r\n\
summarize price\r\n";

#[test]
fn a_crlf_with_bom_file_edited_on_one_line_produces_a_one_line_diff() {
    let (_t, root) = tmp();
    let path = root.join("analysis.do");
    std::fs::write(&path, WINDOWS_DO).unwrap();
    let before = std::fs::read(&path).unwrap();

    let mut ws = project_at(&root);
    let opened = ws.doc_open(&path).unwrap();
    assert_eq!(
        opened.bytes,
        DocBytes {
            eol: Eol::Crlf,
            bom: true
        }
    );
    assert!(opened.diagnostics.is_empty(), "{:?}", opened.diagnostics);
    // The editor sees LF; the file's CRLF is a property of the file, not of the
    // buffer.
    assert!(!opened.text.contains('\r'));

    // Edit exactly one line: `regress price mpg weight` → `regress price mpg`.
    let at = opened.text.find("regress price mpg weight").unwrap() as u32;
    ws.doc_change(
        opened.doc,
        opened.version,
        &[Edit {
            span: Span {
                start: at,
                end: at + "regress price mpg weight".len() as u32,
            },
            text: "regress price mpg".to_owned(),
        }],
    )
    .unwrap();

    let ack = ws.doc_save(opened.doc).unwrap();
    assert_eq!(ack.eol, Eol::Crlf);
    assert!(ack.bom);

    let after = std::fs::read(&path).unwrap();
    assert_eq!(
        lines_differing(&before, &after),
        1,
        "a one-line edit must produce a one-line diff"
    );
    // The BOM and every CRLF survived.
    assert!(after.starts_with(b"\xef\xbb\xbf"));
    assert_eq!(
        after.iter().filter(|&&c| c == b'\n').count(),
        before.iter().filter(|&&c| c == b'\n').count()
    );
    assert!(!after.windows(2).any(|w| w == b"\r\r"));
}

#[test]
fn saving_an_unedited_file_rewrites_the_same_bytes() {
    for raw in [
        WINDOWS_DO,
        b"sysuse auto\nlist\n",
        b"\xef\xbb\xbfsysuse auto\nlist\n",
        b"sysuse auto\r\nlist\r\n",
        // No trailing newline: a real and easily broken case.
        b"sysuse auto\r\nlist",
    ] {
        let (_t, root) = tmp();
        let path = root.join("a.do");
        std::fs::write(&path, raw).unwrap();

        let mut ws = project_at(&root);
        let opened = ws.doc_open(&path).unwrap();
        ws.doc_save(opened.doc).unwrap();

        assert_eq!(
            std::fs::read(&path).unwrap(),
            raw,
            "save must be a no-op for an unedited {:?}",
            String::from_utf8_lossy(raw)
        );
    }
}

#[test]
fn a_mixed_eol_file_raises_l013_and_says_which_way_it_will_normalise() {
    let (_t, root) = tmp();
    let path = root.join("mixed.do");
    std::fs::write(&path, b"a\r\nb\r\nc\nd\r\n").unwrap();

    let mut ws = project_at(&root);
    let opened = ws.doc_open(&path).unwrap();
    assert_eq!(opened.bytes.eol, Eol::Crlf);
    let l013: Vec<_> = opened
        .diagnostics
        .iter()
        .filter(|d| d.code == "L013")
        .collect();
    assert_eq!(l013.len(), 1);
    assert!(l013[0].message.contains("CRLF"));
    assert_eq!(l013[0].file.as_deref(), Some(path.as_path()));
}

#[test]
fn a_non_utf8_file_is_refused_and_its_bytes_are_untouched() {
    let (_t, root) = tmp();
    let path = root.join("latin1.do");
    // `£` in latin-1 — a real thing to find in a UK wage-data do-file.
    let raw: &[u8] = b"sysuse auto\nlabel var price \"Price (\xa3)\"\n";
    std::fs::write(&path, raw).unwrap();

    let mut ws = project_at(&root);
    let failure = ws.doc_open(&path).unwrap_err();
    let d = failure.diagnostic().expect("an encoding refusal");
    assert_eq!(d.code, "STRATUM0601");
    assert_eq!(d.severity, stratum_proto::Severity::Error);
    // No suggestion means no edit means nothing can transcode it "helpfully".
    assert!(d.suggestions.is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), raw, "the file was modified");

    // The offer: open it read-only.
    let ro = ws.doc_open_read_only(&path).unwrap();
    assert!(ro.read_only);
    assert!(ro.diagnostics.iter().any(|d| d.code == "STRATUM0601"));

    // And a read-only document cannot be written, by any of the four writers.
    assert!(ws.doc_save(ro.doc).is_err());
    assert!(ws
        .section_rename(ro.doc, stratum_proto::SectionId(0), "x")
        .is_err());
    assert_eq!(std::fs::read(&path).unwrap(), raw, "the file was modified");
}

#[test]
fn an_untitled_buffer_defaults_to_lf_without_a_bom() {
    let (_t, root) = tmp();
    let mut ws = project_at(&root);
    let opened = ws.doc_new("list\n");
    assert_eq!(
        opened.bytes,
        DocBytes {
            eol: Eol::Lf,
            bom: false
        }
    );
    // It has no path, so there is nothing to be faithful to and nothing to save.
    assert!(ws.doc_save(opened.doc).is_err());
}

#[test]
fn a_stale_doc_change_is_refused_rather_than_applied_out_of_order() {
    let (_t, root) = tmp();
    let path = root.join("a.do");
    std::fs::write(&path, b"list\n").unwrap();
    let mut ws = project_at(&root);
    let opened = ws.doc_open(&path).unwrap();

    let e = vec![Edit {
        span: Span { start: 0, end: 0 },
        text: "// x\n".to_owned(),
    }];
    assert_eq!(ws.doc_change(opened.doc, 0, &e).unwrap(), 1);
    assert!(ws.doc_change(opened.doc, 0, &e).is_err());
}

#[test]
fn the_saved_ack_hashes_the_bytes_on_disk() {
    let (_t, root) = tmp();
    let path = root.join("a.do");
    std::fs::write(&path, WINDOWS_DO).unwrap();
    let mut ws = project_at(&root);
    let opened = ws.doc_open(&path).unwrap();
    let ack = ws.doc_save(opened.doc).unwrap();
    assert_eq!(ack.text_hash, ws.document(opened.doc).unwrap().text_hash());
    assert_eq!(ack.path, path);
}

#[test]
fn a_workspace_can_be_built_over_a_project_with_no_files_at_all() {
    // Nothing here should require a project file, a sidecar, or a resources
    // directory: opening a folder of do-files must Just Work.
    let (_t, root) = tmp();
    let project = Project::load(&root).unwrap();
    let ws = Workspace::new(
        project,
        LayoutStore::new(root.join("nope"), root.join("cfg/layouts")),
        KeymapStore::new(root.join("nope"), root.join("cfg/keymaps")),
    );
    assert!(ws.layout_load("modern").is_ok());
    assert!(ws
        .keymap_load(stratum_workspace::keymap::KeymapPreset::Modern)
        .is_ok());
}

// ---------------------------------------------------------------------------
// The property the two functions above are supposed to have.
// ---------------------------------------------------------------------------

proptest::proptest! {
    /// For any file with uniform line endings, `encode(decode(b)) == b`.
    ///
    /// This is the whole of A24 stated once. The generator builds text out of
    /// fragments that have historically broken naive implementations: a lone
    /// `\r`, a `\r` at the end of a line, non-ASCII, and an empty line.
    #[test]
    fn encode_inverts_decode_for_any_uniform_file(
        parts in proptest::collection::vec(
            proptest::sample::select(vec![
                "sysuse auto", "", "  list", "di \"x\ry\"", "gen z = a + ///",
                "label var y \"Wage (£)\"", "\t*/ trailing", "di 1\r",
            ]),
            0..12),
        crlf in proptest::bool::ANY,
        bom in proptest::bool::ANY,
        trailing in proptest::bool::ANY,
    ) {
        let eol = if crlf { "\r\n" } else { "\n" };
        let mut text = parts.join(eol);
        if trailing && !parts.is_empty() {
            text.push_str(eol);
        }
        let mut raw = Vec::new();
        if bom {
            raw.extend_from_slice(b"\xef\xbb\xbf");
        }
        raw.extend_from_slice(text.as_bytes());

        // Uniformity is decided from the assembled BYTES, not from `crlf`: the
        // `di 1\r` fragment ends in a carriage return, so joining with `\n`
        // manufactures a genuinely mixed file. That is the interesting case, not
        // a flaw in the generator.
        let body = &raw[if bom { 3 } else { 0 }..];
        let lf = body.iter().enumerate()
            .filter(|&(i, &c)| c == b'\n' && (i == 0 || body[i - 1] != b'\r'))
            .count();
        let crlf = body.windows(2).filter(|w| w == b"\r\n").count();

        let d = decode(&raw).unwrap();
        if lf > 0 && crlf > 0 {
            // Mixed: exactly one L013, and the round trip is deliberately NOT an
            // identity — majority wins, which is what the lint warns about.
            proptest::prop_assert_eq!(d.diagnostics.len(), 1);
            proptest::prop_assert_eq!(d.diagnostics[0].code.as_str(), "L013");
        } else {
            proptest::prop_assert!(d.diagnostics.is_empty());
            proptest::prop_assert_eq!(encode(&d.text, d.bytes), raw);
        }
    }
}

/// A regression guard for the temp-file strategy in `write_document`: the
/// sibling `.tmp` must never be left behind on a successful save.
#[test]
fn saving_leaves_no_temporary_file_behind() {
    let (_t, root) = tmp();
    let path = root.join("a.do");
    std::fs::write(&path, b"list\n").unwrap();
    let mut ws = project_at(&root);
    let opened = ws.doc_open(&path).unwrap();
    ws.doc_save(opened.doc).unwrap();

    let strays: Vec<Utf8PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .flatten()
        .filter_map(|e| Utf8PathBuf::from_path_buf(e.path()).ok())
        .filter(|p| p.as_str().ends_with(".tmp"))
        .collect();
    assert!(strays.is_empty(), "{strays:?}");
}
