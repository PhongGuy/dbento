//! How staged edits look in the grid, and the edit tints in both themes.
use crate::support;
use crate::support::{Opened, label_of, launched, present, wait_real};
use dbento_core::RelationKind;
use gpui_kit::component::{ActiveTheme as _, ThemeMode};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext, px};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, note TEXT); \
                 INSERT INTO t VALUES (1, 'b', 'x'), (2, 'a', 'y');";

fn t(cx: &mut TestAppContext) -> Opened {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    o
}

#[gpui_kit::test]
fn the_edit_tints_derive_from_warning_success_and_danger(cx: &mut TestAppContext) {
    support::init(cx);
    for mode in [ThemeMode::Dark, ThemeMode::Light] {
        cx.update(|cx| {
            dbento_app::theme::set_mode(mode, None, cx);
            let (warning, success, danger) =
                (cx.theme().warning, cx.theme().success, cx.theme().danger);
            let p = dbento_app::theme::edit_pending(cx);
            if mode == ThemeMode::Dark {
                assert_eq!(p.fill, warning.opacity(0.2));
                assert_eq!((p.border, p.chip_text), (warning, warning));
            } else {
                let (fill, ink) = (
                    gpui_kit::Hsla::from(gpui_kit::rgb(0xFFF4E0)),
                    gpui_kit::Hsla::from(gpui_kit::rgb(0x8F5F00)),
                );
                assert_eq!(p.fill, fill);
                assert_eq!((p.border, p.chip_text), (ink, ink));
            }
            assert_eq!(dbento_app::theme::edit_inserted(cx), success.opacity(0.15));
            assert_eq!(dbento_app::theme::edit_deleted(cx), danger.opacity(0.15));
        });
    }
}

#[gpui_kit::test]
fn a_modified_cell_and_its_row_are_marked(cx: &mut TestAppContext) {
    let o = t(cx);
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    assert!(present(cx, o.win, "cell-modified-0-1"));
    assert!(present(cx, o.win, "row-modified-0"));
    assert!(!present(cx, o.win, "cell-modified-0-2"));
    assert!(!present(cx, o.win, "row-modified-1"));
    assert!(!present(cx, o.win, "row-deleted-0"));
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        assert_eq!(w.find("row-modified-0").bounds().size.width, px(3.));
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_deleted_row_is_marked_and_keeps_its_text(cx: &mut TestAppContext) {
    let o = t(cx);
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.delete_row(1, cx)));
    assert!(present(cx, o.win, "row-deleted-1"));
    assert!(!present(cx, o.win, "row-modified-1"));
    assert!(!present(cx, o.win, "row-deleted-0"));
    assert_eq!(label_of(cx, o.win, "cell-1-1").as_deref(), Some("a"));
}

#[gpui_kit::test]
fn a_rejected_row_carries_its_outcome(cx: &mut TestAppContext) {
    let o = t(cx);
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    rusqlite::Connection::open(o.fx.dir.path().join("data/app.db"))
        .unwrap()
        .execute_batch("DELETE FROM t WHERE id = 1")
        .unwrap();
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_real(cx, "the rejection", |cx| {
        m.read_with(cx, |m, _| m.edit_banner().is_some())
    });
    assert_eq!(
        label_of(cx, o.win, "row-failed-0").as_deref(),
        Some("modified or deleted by someone else")
    );
    assert!(!present(cx, o.win, "row-failed-1"));
    assert!(present(cx, o.win, "row-modified-0"), "the edit is kept");
}
