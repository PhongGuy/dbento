//! The inspector's stacked fields: names on one edge with their boxes, sizes, box border.
use crate::support;
use crate::support::{click_id, launched};
use dbento_app::record_form::{STACKED_NAME_SIZE, STACKED_TYPE_SIZE, box_border};
use dbento_core::RelationKind;
use gpui_kit::component::{ActiveTheme as _, ThemeMode};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, Focusable as _, TestAppContext, px};

const T: &str = "CREATE TABLE b(id INTEGER PRIMARY KEY, name TEXT, note TEXT); \
                 INSERT INTO b VALUES (1, 'n', 'm');";

#[gpui_kit::test]
fn stacked_names_start_where_their_value_boxes_start(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "b", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.b\nORDER BY id ASC");
    click_id(cx, o.win, "inspector-toggle");
    let table = o.shell.read_with(cx, |s, cx| {
        s.tab()
            .unwrap()
            .read(cx)
            .results_pane()
            .read(cx)
            .table()
            .clone()
    });
    cx.update_window(o.win, |_, w, cx| {
        table.update(cx, |t, cx| t.set_selected_cell(0, 1, cx));
        table.read(cx).focus_handle(cx).focus(w, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        for col in 0..3usize {
            let name = w.find(("inspector-name", col)).bounds();
            let boxed = w.find(("inspector-box", col)).bounds();
            assert_eq!(name.origin.x, boxed.origin.x, "column {col}");
        }
        let key = w.find(("inspector-pk", 0usize)).bounds();
        let name = w.find(("inspector-name", 0usize)).bounds();
        assert!(key.right() <= name.origin.x, "{key:?} vs {name:?}");
    })
    .unwrap();
}

#[gpui_kit::test]
fn names_are_13_px_types_11_px_and_boxes_take_the_input_border(cx: &mut TestAppContext) {
    assert_eq!((STACKED_NAME_SIZE, STACKED_TYPE_SIZE), (px(13.), px(11.)));
    support::init(cx);
    for mode in [ThemeMode::Dark, ThemeMode::Light] {
        cx.update(|cx| {
            dbento_app::theme::set_mode(mode, None, cx);
            assert_eq!(box_border(cx), cx.theme().input, "{mode:?}");
        });
    }
}
