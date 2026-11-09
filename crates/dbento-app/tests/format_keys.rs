use crate::support::{Opened, open_connection_row, opened, present, started};
use dbento_app::format::format_edit;
use dbento_core::Engine::{Postgres, Sqlite};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

const RAW: &str = "select 1 as a;\nselect b, c from t;";

/// `sql` in the focused editor with `from..to` selected.
fn place(cx: &mut TestAppContext, o: &Opened, sql: &str, from: usize, to: usize) {
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    let e = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql(sql, w, cx));
        e.update(cx, |e, cx| {
            e.focus(w, cx);
            e.set_selected_range(from..to, cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
}
fn key(cx: &mut TestAppContext, o: &Opened, k: &str) {
    cx.update_window(o.win, |_, w, cx| w.press(k, cx)).unwrap();
    cx.run_until_parked();
}
fn text(cx: &mut TestAppContext, o: &Opened) -> String {
    o.shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).sql(cx))
}

#[test]
fn the_edit_is_the_selection_else_the_statement_at_the_caret() {
    assert_eq!(
        format_edit(Sqlite, "select 1; select 2", 0..8),
        Some((0..8, "SELECT 1".to_owned()))
    );
    assert_eq!(
        format_edit(Postgres, "select 1;\nselect 2;", 12..12),
        Some((9..19, "\nSELECT 2;".to_owned()))
    );
}

#[test]
fn nothing_to_format_or_already_formatted_is_no_edit() {
    assert_eq!(format_edit(Sqlite, "", 0..0), None);
    assert_eq!(format_edit(Sqlite, "  -- c", 3..3), None);
    assert_eq!(format_edit(Sqlite, "SELECT 1;", 3..3), None);
}

#[gpui_kit::test]
fn ctrl_alt_l_formats_the_statement_under_the_caret_only(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    place(cx, &o, RAW, 20, 20);
    key(cx, &o, "ctrl-alt-l");
    assert_eq!(text(cx, &o), "select 1 as a;\nSELECT b,\n       c\nFROM t;");
}

#[gpui_kit::test]
fn ctrl_alt_l_formats_exactly_the_selection(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    place(cx, &o, RAW, 0, 13);
    key(cx, &o, "ctrl-alt-l");
    assert_eq!(text(cx, &o), "SELECT 1 AS a;\nselect b, c from t;");
}

#[gpui_kit::test]
fn one_ctrl_z_restores_the_original(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    place(cx, &o, RAW, 0, 0);
    // An edit before the format, so the history the format joins is not empty.
    let e = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone());
    cx.update_window(o.win, |_, w, cx| {
        e.update(cx, |e, cx| {
            e.insert("-- x\n", w, cx);
            e.set_selected_range(25..25, cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
    let edited = format!("-- x\n{RAW}");
    assert_eq!(text(cx, &o), edited);
    key(cx, &o, "ctrl-alt-l");
    assert_ne!(text(cx, &o), edited);
    key(cx, &o, "ctrl-z");
    assert_eq!(text(cx, &o), edited, "one undo takes back only the format");
    key(cx, &o, "ctrl-z");
    assert_eq!(text(cx, &o), RAW);
}

#[gpui_kit::test]
fn the_format_button_formats_at_the_caret(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    place(cx, &o, RAW, 3, 3);
    cx.update_window(o.win, |_, w, cx| w.click("format", cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(text(cx, &o), "SELECT 1 AS a;\nselect b, c from t;");
}

#[test]
fn a_selection_that_splits_a_literal_is_no_edit() {
    let sql = "select 'x select a from y' as s;";
    let at = sql.find("select a from").unwrap();
    assert_eq!(format_edit(Sqlite, sql, at..at + 13), None);
    assert_eq!(format_edit(Postgres, sql, at..at + 13), None);
}

#[test]
fn non_ascii_text_formats_at_the_caret_and_in_a_selection() {
    let first = "select '\u{e6}\u{f8}\u{e5} from', name from t;";
    let sql = format!("{first}\nselect 1;");
    let want = "SELECT '\u{e6}\u{f8}\u{e5} from',\n       name\nFROM t;".to_owned();
    assert_eq!(
        format_edit(Postgres, &sql, 3..3),
        Some((0..first.len(), want.clone()))
    );
    assert_eq!(
        format_edit(Sqlite, &sql, 0..first.len()),
        Some((0..first.len(), want))
    );
    let second = first.len() + 2;
    assert_eq!(
        format_edit(Sqlite, &sql, second..second),
        Some((first.len()..sql.len(), "\nSELECT 1;".to_owned()))
    );
    // Inside the string, from its second char through `from`.
    let cut = first.find('\u{f8}').unwrap();
    assert_eq!(format_edit(Sqlite, &sql, cut..cut + 9), None);
}

#[gpui_kit::test]
fn ctrl_shift_f_and_alt_shift_f_format_too(cx: &mut TestAppContext) {
    for k in ["ctrl-shift-f", "alt-shift-f"] {
        let o = opened(cx, "", "", 0);
        place(cx, &o, RAW, 20, 20);
        key(cx, &o, k);
        assert_eq!(
            text(cx, &o),
            "select 1 as a;\nSELECT b,\n       c\nFROM t;",
            "{k}"
        );
    }
}

#[gpui_kit::test]
fn the_shown_binding_is_ctrl_shift_f(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let shown = cx
        .update_window(o.win, |_, w, _| {
            w.highest_precedence_binding_for_action_in_context(
                &dbento_app::keymap::FormatSql,
                gpui_kit::KeyContext::parse("QueryTab").unwrap(),
            )
        })
        .unwrap()
        .expect("FormatSql is bound in QueryTab");
    let typed = [gpui_kit::Keystroke::parse("ctrl-shift-f").unwrap()];
    assert_eq!(shown.match_keystrokes(&typed), Some(false));
}

#[test]
fn a_half_typed_quote_elsewhere_does_not_block_a_selection() {
    let sql = "select a, b from t; select 'x";
    assert_eq!(
        format_edit(Sqlite, sql, 0..18),
        Some((0..18, "SELECT a,\n       b\nFROM t".to_owned()))
    );
}

fn selected(cx: &mut TestAppContext, o: &Opened) -> std::ops::Range<usize> {
    o.shell.read_with(cx, |s, cx| {
        s.tab().unwrap().read(cx).editor().read(cx).selected_range()
    })
}

#[gpui_kit::test]
fn undoing_a_format_puts_the_caret_back(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    place(cx, &o, RAW, 20, 20);
    key(cx, &o, "ctrl-alt-l");
    key(cx, &o, "ctrl-z");
    assert_eq!(text(cx, &o), RAW);
    assert_eq!(selected(cx, &o), 20..20);
}

#[gpui_kit::test]
fn undo_after_redo_puts_the_caret_back_again(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    place(cx, &o, RAW, 20, 20);
    key(cx, &o, "ctrl-alt-l");
    let formatted = text(cx, &o);
    key(cx, &o, "ctrl-z");
    key(cx, &o, "ctrl-y");
    assert_eq!(text(cx, &o), formatted);
    key(cx, &o, "ctrl-z");
    assert_eq!(text(cx, &o), RAW);
    assert_eq!(selected(cx, &o), 20..20);
}

#[gpui_kit::test]
fn undoing_a_formatted_selection_reselects_it(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    place(cx, &o, RAW, 0, 13);
    key(cx, &o, "ctrl-alt-l");
    key(cx, &o, "ctrl-z");
    assert_eq!(text(cx, &o), RAW);
    assert_eq!(selected(cx, &o), 0..13);
}

#[gpui_kit::test]
fn undoing_typing_then_the_format_puts_the_caret_back(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    place(cx, &o, RAW, 20, 20);
    key(cx, &o, "ctrl-alt-l");
    let formatted = text(cx, &o);
    let e = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone());
    cx.update_window(o.win, |_, w, cx| e.update(cx, |e, cx| e.insert("x", w, cx)))
        .unwrap();
    cx.run_until_parked();
    key(cx, &o, "ctrl-z");
    assert_eq!(
        text(cx, &o),
        formatted,
        "the first undo takes back the typing"
    );
    key(cx, &o, "ctrl-z");
    assert_eq!(text(cx, &o), RAW);
    assert_eq!(selected(cx, &o), 20..20);
}

#[gpui_kit::test]
fn formatting_formatted_text_leaves_undo_alone(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    place(cx, &o, "SELECT 1;", 3, 3);
    key(cx, &o, "ctrl-alt-l");
    assert_eq!(text(cx, &o), "SELECT 1;");
    assert_eq!(selected(cx, &o), 3..3);
    key(cx, &o, "ctrl-z");
    assert_eq!(text(cx, &o), "SELECT 1;");
}

/// A SurrealDB connection that never connects; the tab opens and its preconnect fails quietly.
const SR_CLOSED: &str = "dbento = 1\n[connections.sr]\nengine = \"surrealdb\"\nhost = \"127.0.0.1\"\nport = 9\nnamespace = \"app\"\ndatabase = \"main\"\n";

#[gpui_kit::test]
fn surrealdb_has_no_format_button_menu_item_or_working_keys(cx: &mut TestAppContext) {
    let s = started(cx, SR_CLOSED);
    open_connection_row(cx, s.win, "sr");
    assert!(present(cx, s.win, "editor"));
    assert!(!present(cx, s.win, "format"));
    assert!(!present(cx, s.win, "format-tip"));

    let tab = s.shell.read_with(cx, |sh, _| sh.tab().unwrap().clone());
    let e = s
        .shell
        .read_with(cx, |sh, cx| sh.tab().unwrap().read(cx).editor().clone());
    let raw = "select * from person where age > 1";
    cx.update_window(s.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql(raw, w, cx));
        e.update(cx, |e, cx| {
            e.focus(w, cx);
            e.set_selected_range(0..raw.len(), cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
    for k in ["ctrl-shift-f", "alt-shift-f", "ctrl-alt-l"] {
        cx.update_window(s.win, |_, w, cx| w.press(k, cx)).unwrap();
        cx.run_until_parked();
        assert_eq!(tab.read_with(cx, |t, cx| t.sql(cx)), raw, "{k}");
    }

    cx.update_window(s.win, |_, w, cx| w.right_click("editor", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(present(cx, s.win, "editor-menu-run"));
    assert!(!present(cx, s.win, "editor-menu-format"));
}
