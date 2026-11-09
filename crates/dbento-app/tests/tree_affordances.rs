//! Small affordances: icon contrast, theme glyphs, the folder dot slot, tree tooltips.
use crate::support;
use crate::support::{click_id, label_of, launched, present, started, wait_real};
use dbento_app::explorer::{connection_id, part_id};
use dbento_app::schema_tree::NodeKey;
use dbento_app::sidebar::TREE_TOOLTIP_DELAY;
use dbento_app::theme::{Appearance, affordance, appearance_icon, editor_fill};
use dbento_app::ui::Icons;
use dbento_core::{RelationKind, RelationRef};
use gpui_kit::component::{ActiveTheme as _, ThemeMode};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, Hsla, TestAppContext, px};
use std::time::Duration;

const FOLDERS: &str = "dbento = 1\n[folders.red]\ncolor = \"red\"\n\n[folders.plain]\n\n\
[connections.one]\nengine = \"sqlite\"\npath = \"1.db\"\nfolder = \"red\"\n\n\
[connections.two]\nengine = \"sqlite\"\npath = \"2.db\"\nfolder = \"plain\"\n";

fn contrast(a: Hsla, b: Hsla) -> f32 {
    let lum = |c: Hsla| {
        let c = c.to_rgb();
        let ch = |v: f32| {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(c.r) + 0.7152 * ch(c.g) + 0.0722 * ch(c.b)
    };
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

#[gpui_kit::test]
fn affordance_icons_reach_four_and_a_half_to_one_where_they_sit(cx: &mut TestAppContext) {
    support::init(cx);
    for mode in [ThemeMode::Dark, ThemeMode::Light] {
        cx.update(|cx| {
            dbento_app::theme::set_mode(mode, None, cx);
            let t = cx.theme().clone();
            let c = affordance(cx);
            assert_eq!(c.a, 1.0, "{mode:?}");
            for (name, bg) in [
                ("sidebar", t.sidebar),
                ("background", t.background),
                ("editor", editor_fill(cx)),
            ] {
                assert!(contrast(c, bg) >= 4.5, "{mode:?} on {name}");
            }
        });
    }
}

#[test]
fn each_appearance_has_its_own_glyph_and_tree_tooltips_wait() {
    assert_eq!(appearance_icon(Appearance::System), Icons::SunMoon);
    assert_eq!(appearance_icon(Appearance::Dark), Icons::Moon);
    assert_eq!(appearance_icon(Appearance::Light), Icons::Sun);
    assert_eq!(TREE_TOOLTIP_DELAY, Duration::from_millis(600));
}

#[gpui_kit::test]
fn untagged_folders_keep_the_dot_slot(cx: &mut TestAppContext) {
    let s = started(cx, FOLDERS);
    cx.update_window(s.win, |_, w, cx| {
        w.render_frame(cx);
        let red = w.find(part_id("folder-dot", "red")).bounds();
        let plain = w.find(part_id("folder-dot", "plain")).bounds();
        assert_eq!(
            (red.origin.x, red.size.width),
            (plain.origin.x, plain.size.width)
        );
    })
    .unwrap();
    assert_eq!(
        label_of(cx, s.win, part_id("folder-dot", "plain")).as_deref(),
        Some("none")
    );
    assert_eq!(
        label_of(cx, s.win, part_id("folder-dot", "red")).as_deref(),
        Some("red")
    );
}

#[gpui_kit::test]
fn a_click_hides_the_rows_tooltip_until_the_pointer_leaves(cx: &mut TestAppContext) {
    let s = started(cx, FOLDERS);
    let sidebar = s.shell.read_with(cx, |sh, _| sh.sidebar().unwrap().clone());
    assert!(!sidebar.read_with(cx, |sb, _| sb.tooltip_suppressed("one")));
    click_id(cx, s.win, connection_id("one"));
    assert!(sidebar.read_with(cx, |sb, _| sb.tooltip_suppressed("one")));
    cx.update_window(s.win, |_, w, cx| w.hover(connection_id("two"), cx))
        .unwrap();
    cx.run_until_parked();
    assert!(!sidebar.read_with(cx, |sb, _| sb.tooltip_suppressed("one")));
}

#[gpui_kit::test]
fn the_gear_is_14_px_and_the_rail_shows_the_chosen_theme(cx: &mut TestAppContext) {
    let s = started(cx, FOLDERS);
    // The gear shows only on a hovered or selected row (from A2), so select the row first.
    click_id(cx, s.win, connection_id("one"));
    cx.update_window(s.win, |_, w, cx| {
        w.render_frame(cx);
        let gear = w.find(part_id("edit-glyph", "one")).bounds().size;
        assert_eq!((gear.width, gear.height), (px(14.), px(14.)));
    })
    .unwrap();
    assert_eq!(
        label_of(cx, s.win, "rail-appearance-icon").as_deref(),
        Some("SunMoon")
    );
    click_id(cx, s.win, "rail-appearance");
    assert_eq!(
        label_of(cx, s.win, "rail-appearance-icon").as_deref(),
        Some("Moon")
    );
    click_id(cx, s.win, "rail-appearance");
    assert_eq!(
        label_of(cx, s.win, "rail-appearance-icon").as_deref(),
        Some("Sun")
    );
}

#[gpui_kit::test]
fn undo_and_redo_icons_are_14_px(cx: &mut TestAppContext) {
    let o = launched(
        cx,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); INSERT INTO t VALUES (1, 'b');",
        "",
    );
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    cx.run_until_parked();
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        for id in ["undo-edits-icon", "redo-edits-icon"] {
            let size = w.find(id).bounds().size;
            assert_eq!((size.width, size.height), (px(14.), px(14.)), "{id}");
        }
    })
    .unwrap();
}

const ALBUM: &str =
    "CREATE TABLE Album(AlbumId INTEGER PRIMARY KEY); CREATE VIEW Recent AS SELECT 1;";

#[gpui_kit::test]
fn a_click_quiets_a_relation_row_until_another_row_is_entered(cx: &mut TestAppContext) {
    let o = launched(cx, ALBUM, "");
    click_id(cx, o.win, part_id("connection-toggle", "lite"));
    wait_real(cx, "Album listed", |cx| {
        label_of(cx, o.win, ("tree-row", 2usize)).as_deref() == Some("lite.main.Album")
    });
    let sidebar = o.shell.read_with(cx, |sh, _| sh.sidebar().unwrap().clone());
    let album = NodeKey::Relation {
        conn: "lite".into(),
        rel: RelationRef::new("main", "Album"),
    };
    click_id(cx, o.win, ("tree-row", 2usize));
    assert!(sidebar.read_with(cx, |sb, _| sb.tooltip_suppressed_at(&album, false)));
    cx.update_window(o.win, |_, w, cx| w.hover(("tree-row", 4usize), cx))
        .unwrap();
    cx.run_until_parked();
    assert!(!sidebar.read_with(cx, |sb, _| sb.tooltip_suppressed_at(&album, false)));
}

#[gpui_kit::test]
fn a_click_quiets_a_failed_row_apart_from_its_connection(cx: &mut TestAppContext) {
    let o = launched(cx, "", "");
    click_id(cx, o.win, part_id("connection-toggle", "nopg"));
    wait_real(cx, "nopg failed", |cx| {
        present(cx, o.win, ("tree-retry", 0usize))
    });
    let sidebar = o.shell.read_with(cx, |sh, _| sh.sidebar().unwrap().clone());
    let nopg = NodeKey::Connection("nopg".into());
    click_id(cx, o.win, ("tree-row", 0usize));
    sidebar.read_with(cx, |sb, _| {
        assert!(sb.tooltip_suppressed_at(&nopg, true));
        assert!(
            !sb.tooltip_suppressed("nopg"),
            "the connection row keeps its tooltip"
        );
    });
    cx.update_window(o.win, |_, w, cx| w.hover(connection_id("lite"), cx))
        .unwrap();
    cx.run_until_parked();
    assert!(!sidebar.read_with(cx, |sb, _| sb.tooltip_suppressed_at(&nopg, true)));
}
