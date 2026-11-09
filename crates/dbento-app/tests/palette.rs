//! The palette: Ctrl+P and Ctrl+Shift+P from the editor and the grid, ranked rows, Enter opens.
use crate::support;
use crate::support::{Opened, label_of, launched, opened, present, wait_real};
use dbento_app::catalog::CatalogQuery;
use dbento_app::keymap::{NEW_TAB_KEYS, chip_keys};
use dbento_app::palette::{Mode, short_message};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, Focusable as _, TestAppContext};

#[test]
fn a_catalog_error_shows_its_first_line_cut_to_80_characters() {
    assert_eq!(short_message("no such host\nat line 2"), "no such host");
    assert_eq!(short_message(""), "");
    let exact = "é".repeat(80);
    assert_eq!(short_message(&exact), exact);
    let long = "é".repeat(81);
    assert_eq!(short_message(&long), format!("{}\u{2026}", "é".repeat(79)));
}

const SETUP: &str = "CREATE TABLE orders(id INTEGER PRIMARY KEY); \
                     CREATE TABLE customers(id INTEGER PRIMARY KEY); \
                     CREATE VIEW recent_orders AS SELECT id FROM orders;";

/// Loads lite's `main` tables into the explorer's catalog, as an expanded tree would.
fn load_lite(cx: &mut TestAppContext, o: &Opened) {
    let sb = o.shell.read_with(cx, |s, _| s.sidebar().unwrap().clone());
    sb.update(cx, |s, cx| {
        s.ensure_loaded(
            "lite",
            &[
                CatalogQuery::Schemas,
                CatalogQuery::Relations {
                    schema: "main".into(),
                },
            ],
            cx,
        )
    });
    wait_real(cx, "lite's tables", |cx| {
        sb.read_with(cx, |s, _| s.tree().relations("lite", "main").is_some())
    });
}
fn rows(cx: &mut TestAppContext, o: &Opened) -> Vec<String> {
    (0usize..)
        .map_while(|n| label_of(cx, o.win, ("palette-row", n)))
        .collect()
}
fn press(cx: &mut TestAppContext, o: &Opened, key: &str) {
    cx.update_window(o.win, |_, w, cx| w.press(key, cx))
        .unwrap();
    cx.run_until_parked();
}
fn type_text(cx: &mut TestAppContext, o: &Opened, text: &str) {
    cx.update_window(o.win, |_, w, cx| w.input(text, cx))
        .unwrap();
    cx.run_until_parked();
}
fn in_editor(cx: &mut TestAppContext, o: &Opened, key: &str) {
    let e = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone());
    cx.update_window(o.win, |_, w, cx| e.update(cx, |e, cx| e.focus(w, cx)))
        .unwrap();
    press(cx, o, key);
}
fn in_grid(cx: &mut TestAppContext, o: &Opened, key: &str) {
    o.run(cx, "SELECT 1 AS a");
    o.idle(cx);
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
    press(cx, o, key);
}

#[gpui_kit::test]
fn ctrl_p_in_the_editor_finds_a_table_and_enter_browses_it(cx: &mut TestAppContext) {
    let o = opened(cx, SETUP, "", 0);
    load_lite(cx, &o);
    in_editor(cx, &o, "ctrl-p");
    wait_real(cx, "the palette", |cx| present(cx, o.win, "palette"));
    type_text(cx, &o, "ord");
    wait_real(cx, "orders first", |cx| {
        rows(cx, &o).first().map(String::as_str) == Some("main.orders \u{b7} lite")
    });
    press(cx, &o, "enter");
    assert!(!present(cx, o.win, "palette"));
    wait_real(cx, "the browse tab", |cx| {
        o.shell.read_with(cx, |s, _| s.tabs().len() == 2)
    });
    let recent = o.shell.read_with(cx, |s, _| s.recent_tables().to_vec());
    assert_eq!(recent.len(), 1);
    assert_eq!(
        (recent[0].connection.as_str(), recent[0].rel.name()),
        ("lite", "orders")
    );
}

#[gpui_kit::test]
fn ctrl_p_in_the_grid_opens_the_palette(cx: &mut TestAppContext) {
    let o = opened(cx, SETUP, "", 0);
    in_grid(cx, &o, "ctrl-p");
    wait_real(cx, "the palette", |cx| present(cx, o.win, "palette"));
}

#[gpui_kit::test]
fn ctrl_shift_p_in_the_editor_lists_commands_with_chips(cx: &mut TestAppContext) {
    let o = opened(cx, SETUP, "", 0);
    in_editor(cx, &o, "ctrl-shift-p");
    wait_real(cx, "the commands", |cx| {
        present(cx, o.win, ("palette-row", 2usize))
    });
    assert_eq!(rows(cx, &o)[..3], ["New tab", "Close tab", "Save query"]);
    assert_eq!(
        label_of(cx, o.win, ("palette-keys", 0usize)),
        Some(chip_keys(NEW_TAB_KEYS))
    );
}

#[gpui_kit::test]
fn ctrl_shift_p_in_the_grid_lists_commands(cx: &mut TestAppContext) {
    let o = opened(cx, SETUP, "", 0);
    in_grid(cx, &o, "ctrl-shift-p");
    wait_real(cx, "the commands", |cx| {
        rows(cx, &o).first().map(String::as_str) == Some("New tab")
    });
}

#[gpui_kit::test]
fn one_escape_closes_a_palette_holding_only_its_prefix(cx: &mut TestAppContext) {
    let o = opened(cx, SETUP, "", 0);
    for key in ["ctrl-o", "ctrl-shift-p"] {
        in_editor(cx, &o, key);
        wait_real(cx, "the palette", |cx| present(cx, o.win, "palette"));
        press(cx, &o, "escape");
        assert!(!present(cx, o.win, "palette"), "one Escape after {key}");
        let e = o
            .shell
            .read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone());
        cx.update_window(o.win, |_, w, cx| {
            assert!(
                e.read(cx).focus_handle(cx).is_focused(w),
                "focus back after {key}"
            )
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn escape_clears_a_typed_search_then_closes(cx: &mut TestAppContext) {
    let o = opened(cx, SETUP, "", 0);
    in_editor(cx, &o, "ctrl-shift-p");
    wait_real(cx, "the palette", |cx| present(cx, o.win, "palette"));
    type_text(cx, &o, "new");
    press(cx, &o, "escape");
    assert!(
        present(cx, o.win, "palette"),
        "the first Escape clears '> new'"
    );
    press(cx, &o, "escape");
    wait_real(cx, "the close", |cx| !present(cx, o.win, "palette"));
}

#[gpui_kit::test]
fn a_connection_row_opens_a_tab_on_it(cx: &mut TestAppContext) {
    let o = launched(cx, SETUP, "");
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.open_palette(Mode::All, w, cx))
    })
    .unwrap();
    type_text(cx, &o, "lite");
    wait_real(cx, "the connection row", |cx| {
        rows(cx, &o).iter().any(|r| r == "lite")
    });
    let n = rows(cx, &o).iter().position(|r| r == "lite").unwrap();
    support::click_id(cx, o.win, ("palette-row", n));
    assert!(!present(cx, o.win, "palette"));
    assert_eq!(o.shell.read_with(cx, |s, _| s.tabs().len()), 1);
}
