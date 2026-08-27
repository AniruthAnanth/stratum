//! Both sidecars — ARCHITECTURE C19, plan W26's fifth acceptance bullet.
//!
//! The bullet: "The durable sidecar round-trips with sorted keys, LF, stable
//! field order, no timestamps and no output; deleting it loses nothing but
//! section names and collapse intent, and the app opens the `.do` file fine
//! without it."
//!
//! The last clause is the one that carries the product promise. If a `.do` file
//! needs its sidecar in order to be useful, the sidecar is a proprietary
//! notebook format wearing a dot-file's clothing (§5, ADR-010).

use stratum_proto::{CodeHash, InlineResultsMode, SectionId};
use stratum_workspace::sidecar_cache::{CachePaths, CacheSidecar, MeasuredHeight};
use stratum_workspace::sidecar_durable::{
    sidecar_path, AiConversationRef, AutoCommentAnchor, DurableSidecar, DurableSidecarPatch,
    PinnedComparison,
};

mod common;
use common::{project_at, tmp};

const SRC: &str = "\
// %% Load
sysuse auto, clear

// %% Model
regress price mpg
";

fn h(n: u8) -> CodeHash {
    CodeHash([n; 16])
}

fn populated() -> DurableSidecar {
    DurableSidecar {
        collapsed: vec![h(9), h(2), h(5)],
        inline_results: Some(InlineResultsMode::Compact),
        doc_view: Some(true),
        pinned_comparisons: vec![
            PinnedComparison {
                name: "wage models".into(),
                results: vec!["m2".into(), "m1".into()],
            },
            PinnedComparison {
                name: "alt".into(),
                results: vec!["m3".into()],
            },
        ],
        auto_comment_anchors: vec![
            AutoCommentAnchor {
                block_hash: h(4),
                comment_hash: h(7),
            },
            AutoCommentAnchor {
                block_hash: h(1),
                comment_hash: h(3),
            },
        ],
        ai_conversations: vec![AiConversationRef {
            block_hash: h(6),
            conversation_id: "conv-a1b2".into(),
        }],
        ..Default::default()
    }
}

#[test]
fn the_durable_sidecar_round_trips_deterministically() {
    let (_t, root) = tmp();
    let doc = root.join("analysis.do");
    std::fs::write(&doc, SRC).unwrap();

    let mut s = populated();
    s.reconcile(SRC);
    let written = s.save(&doc).unwrap();
    assert_eq!(written, sidecar_path(&doc));
    assert_eq!(written.file_name(), Some(".analysis.do.workspace"));

    let raw = std::fs::read(&written).unwrap();
    // LF, one trailing newline, no CR anywhere — the file is committed, so it
    // must be byte-identical on Windows without a `.gitattributes` rule.
    assert!(!raw.contains(&b'\r'));
    assert_eq!(raw.last(), Some(&b'\n'));

    let back = DurableSidecar::load(&doc).unwrap();
    let mut expected = s.clone();
    expected.canonicalise();
    assert_eq!(back, expected);

    // Writing it again is byte-identical: no churn in version control.
    back.save(&doc).unwrap();
    assert_eq!(std::fs::read(&written).unwrap(), raw);
}

#[test]
fn collection_order_does_not_reach_the_bytes() {
    let (_t, root) = tmp();
    let doc = root.join("a.do");

    let a = populated();
    let mut b = populated();
    b.collapsed.reverse();
    b.auto_comment_anchors.reverse();
    b.pinned_comparisons.reverse();

    a.save(&doc).unwrap();
    let first = std::fs::read(sidecar_path(&doc)).unwrap();
    b.save(&doc).unwrap();
    assert_eq!(std::fs::read(sidecar_path(&doc)).unwrap(), first);

    // …but the order *inside* one pinned comparison is the user's choice and is
    // preserved.
    let back = DurableSidecar::load(&doc).unwrap();
    let wage = back
        .pinned_comparisons
        .iter()
        .find(|c| c.name == "wage models")
        .unwrap();
    assert_eq!(wage.results, vec!["m2", "m1"]);
}

#[test]
fn the_durable_sidecar_carries_no_timestamp_and_no_output() {
    let (_t, root) = tmp();
    let doc = root.join("a.do");
    let mut s = populated();
    s.reconcile(SRC);
    s.save(&doc).unwrap();

    let text = std::fs::read_to_string(sidecar_path(&doc)).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();

    // No key may name a clock, a duration, an execution, or output.
    let mut keys = Vec::new();
    collect_keys(&v, &mut keys);
    for k in &keys {
        let lower = k.to_lowercase();
        for banned in [
            "_ms",
            "time",
            "date",
            "duration",
            "elapsed",
            "stamp",
            "execution",
            "output",
            "log",
        ] {
            assert!(
                !lower.contains(banned),
                "sidecar key {k:?} leaks {banned:?}"
            );
        }
    }
    // And no value may be a number that looks like a Unix millisecond stamp —
    // the way a timestamp sneaks back in is under an innocent key name.
    let mut nums = Vec::new();
    collect_numbers(&v, &mut nums);
    for n in nums {
        assert!(
            n < 1_000_000_000_000.0,
            "sidecar value {n} looks like a millisecond timestamp"
        );
    }
    // §6, stated as a test: none of the document's output is in here. The only
    // thing from the source that appears at all is section titles.
    assert!(!text.contains("regress"));
    assert!(text.contains("Load") && text.contains("Model"));
}

#[test]
fn deleting_the_sidecar_loses_only_section_names_and_collapse_intent() {
    let (_t, root) = tmp();
    let doc = root.join("analysis.do");
    std::fs::write(&doc, SRC).unwrap();

    let mut ws = project_at(&root);
    let opened = ws.doc_open(&doc).unwrap();
    ws.sidecar_patch(
        opened.doc,
        DurableSidecarPatch {
            collapsed: Some(vec![h(3)]),
            doc_view: Some(Some(true)),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(sidecar_path(&doc).exists());
    ws.doc_close(opened.doc).unwrap();

    // Now the collaborator's clone: the `.do` without the sidecar.
    std::fs::remove_file(sidecar_path(&doc)).unwrap();
    let mut ws2 = project_at(&root);
    let reopened = ws2.doc_open(&doc).unwrap();

    // The document opens fine, byte for byte…
    assert_eq!(reopened.text, SRC);
    assert!(reopened.diagnostics.is_empty());
    // …the sections are still there, because the `// %%` markers are in the
    // SOURCE, which is the whole point of §5…
    assert_eq!(reopened.sections.len(), 2);
    assert_eq!(reopened.sections[0].title, "Load");
    assert_eq!(reopened.sections[1].title, "Model");
    // …and it is still editable and saveable.
    let before = std::fs::read(&doc).unwrap();
    ws2.doc_save(reopened.doc).unwrap();
    assert_eq!(std::fs::read(&doc).unwrap(), before);

    // What was lost: the collapse intent and the doc-view flag. Nothing else.
    let s = ws2.sidecar_get(reopened.doc).unwrap();
    assert!(s.collapsed.is_empty());
    assert_eq!(s.doc_view, None);
}

#[test]
fn a_stale_sidecar_is_reconciled_against_the_source_not_trusted() {
    let (_t, root) = tmp();
    let doc = root.join("analysis.do");
    std::fs::write(&doc, SRC).unwrap();

    // A sidecar committed before somebody renamed a heading by hand in vim.
    let stale = DurableSidecar {
        sections: vec![stratum_workspace::sidecar_durable::SidecarSection {
            id: 0,
            title: "A name that is no longer in the file".into(),
            span: (0, 4),
        }],
        ..Default::default()
    };
    stale.save(&doc).unwrap();

    let mut ws = project_at(&root);
    let opened = ws.doc_open(&doc).unwrap();
    let s = ws.sidecar_get(opened.doc).unwrap();
    assert_eq!(s.sections.len(), 2);
    assert_eq!(s.sections[0].title, "Load");
    assert_eq!(s.section_ids(), vec![SectionId(0), SectionId(1)]);
}

#[test]
fn a_corrupt_sidecar_does_not_stop_the_document_opening() {
    let (_t, root) = tmp();
    let doc = root.join("analysis.do");
    std::fs::write(&doc, SRC).unwrap();
    std::fs::write(sidecar_path(&doc), b"{ this is not the file we wrote").unwrap();

    let mut ws = project_at(&root);
    let opened = ws.doc_open(&doc).unwrap();
    assert_eq!(opened.text, SRC);
    assert_eq!(opened.sections.len(), 2);
}

#[test]
fn the_sidecar_records_the_byte_policy_so_a_clone_inherits_it() {
    let (_t, root) = tmp();
    let doc = root.join("windows.do");
    std::fs::write(&doc, b"\xef\xbb\xbf// %% A\r\nlist\r\n").unwrap();

    let mut ws = project_at(&root);
    let opened = ws.doc_open(&doc).unwrap();
    ws.sidecar_patch(opened.doc, DurableSidecarPatch::default())
        .unwrap();

    let s = DurableSidecar::load(&doc).unwrap();
    assert_eq!(s.eol, stratum_workspace::Eol::Crlf);
    assert!(s.bom);
    assert_eq!(s.doc_bytes(), opened.bytes);
}

#[test]
fn the_volatile_cache_ignores_itself_and_holds_nothing_irreplaceable() {
    let (_t, root) = tmp();
    let doc = root.join("analysis.do");
    std::fs::write(&doc, SRC).unwrap();

    let mut ws = project_at(&root);
    let opened = ws.doc_open(&doc).unwrap();
    ws.cache_update(opened.doc, |c| {
        c.record_height(MeasuredHeight {
            block_hash: h(1),
            pane_width: 340,
            height: 128,
        });
        c.scroll_line = 12;
    })
    .unwrap();

    let paths = CachePaths::for_document(&root, &doc);
    assert_eq!(
        std::fs::read_to_string(paths.stratum.join(".gitignore")).unwrap(),
        "*\n"
    );
    assert_eq!(CacheSidecar::load(&paths).height_for(h(1), 340), Some(128));

    // Delete the whole tree: the document is unaffected.
    std::fs::remove_dir_all(&paths.stratum).unwrap();
    let mut ws2 = project_at(&root);
    let reopened = ws2.doc_open(&doc).unwrap();
    assert_eq!(reopened.text, SRC);
    assert_eq!(ws2.cache(reopened.doc).unwrap().scroll_line, 0);
}

fn collect_keys(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Object(m) => {
            for (k, v) in m {
                out.push(k.clone());
                collect_keys(v, out);
            }
        }
        serde_json::Value::Array(a) => a.iter().for_each(|v| collect_keys(v, out)),
        _ => {}
    }
}

fn collect_numbers(v: &serde_json::Value, out: &mut Vec<f64>) {
    match v {
        serde_json::Value::Object(m) => m.values().for_each(|v| collect_numbers(v, out)),
        serde_json::Value::Array(a) => a.iter().for_each(|v| collect_numbers(v, out)),
        serde_json::Value::Number(n) => out.push(n.as_f64().unwrap_or(0.0)),
        _ => {}
    }
}
