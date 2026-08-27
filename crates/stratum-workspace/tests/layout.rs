//! Layout presets — spec §25, design 06 §8, plan W26's sixth acceptance bullet.
//!
//! "Layout presets (§25) and keymap presets (§33) persist and reload;
//! `layout_reset` deletes only the user overlay."
//!
//! The second clause is a claim about a *file*, not about a value, so the tests
//! below check the bytes of the shipped preset before and after a reset. A
//! `layout_reset` implemented as "reconstruct the defaults and write them back"
//! would pass a value-level test and quietly destroy a site's customised preset
//! bundle.

use stratum_proto::InlineResultsMode;
use stratum_workspace::layout::{
    Bounds, CommandBar, LayoutSpec, LayoutStore, Preset, TopBar, WindowRole, WindowSpec,
};

mod common;
use common::{project_at, tmp};

fn store(root: &camino::Utf8Path) -> LayoutStore {
    LayoutStore::new(root.join("resources/layouts"), root.join("config/layouts"))
}

#[test]
fn the_three_presets_of_spec_25_exist_and_differ_where_the_spec_says_they_do() {
    let modern = LayoutSpec::preset(Preset::Modern);
    let classic = LayoutSpec::preset(Preset::Classic);
    let focus = LayoutSpec::preset(Preset::Focus);

    // §25B / design 06 §8.3: Classic has inline results OFF. A traditional Stata
    // user who sees cards appear under their commands has not been given Classic.
    assert_eq!(classic.defaults.inline_results, InlineResultsMode::Off);
    assert_eq!(classic.defaults.command_bar, CommandBar::Pane);
    assert_eq!(classic.chrome.top_bar, TopBar::Compact);

    // §25C / §9: Focus is code plus output, chrome out of the way.
    assert_eq!(focus.defaults.inline_results, InlineResultsMode::Always);
    assert_eq!(focus.chrome.top_bar, TopBar::AutoHide);
    assert!(!focus.chrome.status_bar);

    // §25A / §8: Modern is the default.
    assert_eq!(modern.defaults.command_bar, CommandBar::DockedBottom);
    assert!(modern.chrome.status_bar);

    // Stata's Sidebar variant ships too (design 06 §8.3).
    assert_eq!(
        LayoutSpec::preset(Preset::ClassicSidebar).id,
        "classic-sidebar"
    );
}

#[test]
fn every_preset_persists_and_reloads() {
    let (_t, root) = tmp();
    let s = store(&root);

    for p in Preset::ALL {
        let mut spec = s.load(p.id()).unwrap();
        spec.name = format!("{} (mine)", spec.name);
        spec.chrome.status_bar = !spec.chrome.status_bar;
        spec.windows[0].bounds = Some(Bounds {
            x: 100.0,
            y: 50.0,
            w: 1440.0,
            h: 900.0,
            monitor: Some("DELL U2720Q".into()),
        });
        spec.windows.push(WindowSpec {
            role: WindowRole::Data,
            label: "data:proj".into(),
            bounds: None,
            dock: serde_json::json!({ "grid": { "root": "opaque to us" } }),
        });
        spec.panes
            .insert("variables".into(), serde_json::json!({ "sort": "name" }));
        s.save(&spec).unwrap();

        let back = s.load(p.id()).unwrap();
        assert_eq!(back, spec, "{} did not round-trip", p.id());
        // dockview's blob is opaque and must come back untouched.
        assert_eq!(back.windows[1].dock["grid"]["root"], "opaque to us");
    }
}

#[test]
fn layout_reset_deletes_only_the_user_overlay() {
    let (_t, root) = tmp();
    let s = store(&root);

    // A shipped preset bundle, as an installed app would have.
    std::fs::create_dir_all(&s.presets).unwrap();
    let shipped_path = s.presets.join("classic.json");
    let shipped = LayoutSpec {
        name: "Classic Stata (site build)".into(),
        ..LayoutSpec::preset(Preset::Classic)
    };
    std::fs::write(&shipped_path, shipped.to_canonical_bytes()).unwrap();
    let shipped_bytes = std::fs::read(&shipped_path).unwrap();

    // The user customises it…
    let mut mine = s.load("classic").unwrap();
    assert_eq!(mine.name, "Classic Stata (site build)");
    mine.name = "Classic, my way".into();
    mine.defaults.inline_results = InlineResultsMode::Compact;
    let overlay_path = s.save(&mine).unwrap();
    assert!(overlay_path.exists());
    assert_eq!(s.load("classic").unwrap().name, "Classic, my way");

    // …and resets.
    assert!(s.reset("classic").unwrap());
    assert!(!overlay_path.exists(), "the overlay must be gone");
    assert_eq!(
        std::fs::read(&shipped_path).unwrap(),
        shipped_bytes,
        "layout_reset must not touch the shipped preset"
    );
    assert_eq!(
        s.load("classic").unwrap().name,
        "Classic Stata (site build)"
    );

    // Resetting again is a no-op, not an error.
    assert!(!s.reset("classic").unwrap());
}

#[test]
fn a_user_layout_saves_loads_and_lists() {
    let (_t, root) = tmp();
    let s = store(&root);

    let mine = LayoutSpec {
        id: "user:9f2a".into(),
        name: "Two monitors".into(),
        based_on: Some("modern".into()),
        ..LayoutSpec::preset(Preset::Modern)
    };
    s.save(&mine).unwrap();

    assert!(s.load("user:9f2a").unwrap().is_user());
    assert_eq!(
        s.list(),
        vec!["modern", "classic", "classic-sidebar", "focus", "user:9f2a"]
    );
    assert!(s.reset("user:9f2a").unwrap());
    assert!(s.load("user:9f2a").is_err());
}

#[test]
fn a_malformed_user_layout_falls_back_and_is_not_deleted() {
    // Design 06 §8.5: "a malformed user layout falls back to its `basedOn`
    // preset with a status-bar notice". Repairing it by deleting it would throw
    // away a layout the user can still fix in a text editor.
    let (_t, root) = tmp();
    let s = store(&root);
    std::fs::create_dir_all(&s.user).unwrap();
    let path = s.user.join("user_broken.json");
    std::fs::write(
        &path,
        br#"{ "schema": 3, "id": "user:broken", "basedOn": "focus", "windows": 12 }"#,
    )
    .unwrap();

    let spec = s.load("user:broken").unwrap();
    assert_eq!(spec.id, "focus");
    assert_eq!(spec.defaults.inline_results, InlineResultsMode::Always);
    assert!(path.exists());
}

#[test]
fn the_command_surface_reaches_the_same_store() {
    let (_t, root) = tmp();
    let ws = project_at(&root);

    let mut spec = ws.layout_load("focus").unwrap();
    spec.name = "Focus, dimmed".into();
    ws.layout_save(&spec).unwrap();
    assert_eq!(ws.layout_load("focus").unwrap().name, "Focus, dimmed");

    assert!(ws.layout_reset("focus").unwrap());
    assert_eq!(ws.layout_load("focus").unwrap().name, "Focus");
}

#[test]
fn a_saved_layout_is_human_editable_json() {
    // Design 06 §8.5 promises the layout JSON is "documented and editable", so a
    // user who wants a 3-monitor arrangement is not forced through the UI.
    let (_t, root) = tmp();
    let s = store(&root);
    let path = s.save(&LayoutSpec::preset(Preset::Modern)).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();

    assert!(text.contains("\n  \"id\": \"modern\""), "{text}");
    assert!(!text.contains('\r'));
    assert!(text.ends_with("}\n"));
}
