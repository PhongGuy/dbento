//! Ctrl+S, Ctrl+Shift+S and Ctrl+. from the editor and the grid, the save icon, and the
//! toolbar hints.
use crate::support;
use crate::support::{
    LONG, Opened, click_id, label_of, launched, opened, present, wait_prompt, wait_real,
};
use dbento_app::keymap::{CANCEL_QUERY_KEYS, SAVE_QUERY_ALWAYS_KEYS, plus_keys};
use dbento_app::query_tab::{format_tooltip, save_button_keys, save_tooltip};
use dbento_app::tab_model::SaveState;
use dbento_core::{EditValue, RelationKind};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, Focusable as _, TestAppContext};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
                 INSERT INTO t VALUES (1, 'a');";

fn browse(cx: &mut TestAppContext) -> Opened {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    o
}
fn focus_editor(cx: &mut TestAppContext, o: &Opened) {
    let e = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone());
    cx.update_window(o.win, |_, w, cx| e.update(cx, |e, cx| e.focus(w, cx)))
        .unwrap();
    cx.run_until_parked();
}
fn focus_grid(cx: &mut TestAppContext, o: &Opened) {
    let table = o.shell.read_with(cx, |s, cx| {
        let pane = s.tab().unwrap().read(cx).results_pane().clone();
        pane.read(cx).table().clone()
    });
    cx.update_window(o.win, |_, w, cx| {
        table.read(cx).focus_handle(cx).focus(w, cx)
    })
    .unwrap();
    cx.run_until_parked();
}
fn press(cx: &mut TestAppContext, o: &Opened, key: &str) {
    cx.update_window(o.win, |_, w, cx| w.press(key, cx))
        .unwrap();
    cx.run_until_parked();
}
fn staged(cx: &mut TestAppContext, o: &Opened) {
    let m = o.model(cx);
    assert!(
        m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)),
        "the cell stages"
    );
}

#[gpui_kit::test]
fn the_save_tooltip_says_what_ctrl_s_does(cx: &mut TestAppContext) {
    cx.update(|cx| {
        assert_eq!(save_tooltip(0, cx), "Save query");
        assert_eq!(
            save_tooltip(1, cx),
            "Ctrl+S saves 1 staged edit; Ctrl+Shift+S saves the query"
        );
        assert_eq!(
            save_tooltip(3, cx),
            "Ctrl+S saves 3 staged edits; Ctrl+Shift+S saves the query"
        );
    });
}

#[gpui_kit::test]
fn hints_spell_keys_with_plus(cx: &mut TestAppContext) {
    assert_eq!(plus_keys(SAVE_QUERY_ALWAYS_KEYS), "Ctrl+Shift+S");
    assert_eq!(plus_keys(CANCEL_QUERY_KEYS), "Ctrl+.");
    cx.update(|cx| {
        assert_eq!(
            format_tooltip(cx),
            "Format SQL (also Alt+Shift+F, Ctrl+Alt+L)"
        );
    });
}

#[gpui_kit::test]
fn ctrl_s_in_the_editor_saves_staged_edits(cx: &mut TestAppContext) {
    let o = browse(cx);
    staged(cx, &o);
    focus_editor(cx, &o);
    press(cx, &o, "ctrl-s");
    let m = o.model(cx);
    wait_prompt(cx, &m);
    m.update(cx, |m, cx| m.rollback(cx));
    assert!(!present(cx, o.win, "save-name"));
}

/// Opens the cell editor on (row 0, name) from the grid and types `text` over its value.
fn type_in_cell(cx: &mut TestAppContext, o: &Opened, text: &str) {
    let table = o.shell.read_with(cx, |s, cx| {
        let pane = s.tab().unwrap().read(cx).results_pane().clone();
        pane.read(cx).table().clone()
    });
    cx.update_window(o.win, |_, w, cx| {
        table.update(cx, |t, cx| t.set_selected_cell(0, 2, cx));
        table.read(cx).focus_handle(cx).focus(w, cx);
    })
    .unwrap();
    cx.run_until_parked();
    press(cx, o, "enter");
    cx.update_window(o.win, |_, w, cx| w.input(text, cx))
        .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn ctrl_s_in_the_editor_stages_an_open_cell_editor_then_saves(cx: &mut TestAppContext) {
    let o = browse(cx);
    type_in_cell(cx, &o, "z");
    focus_editor(cx, &o);
    let pane = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).results_pane().clone());
    assert_eq!(pane.read_with(cx, |p, _| p.editing()), Some((0, 1)));
    press(cx, &o, "ctrl-s");
    let m = o.model(cx);
    wait_prompt(cx, &m);
    assert_eq!(
        m.read_with(cx, |m, _| m.edits().cell(0, 1).cloned()),
        Some(EditValue::Text("z".into()))
    );
    assert!(!present(cx, o.win, "save-name"));
    m.update(cx, |m, cx| m.rollback(cx));
}

#[gpui_kit::test]
fn the_save_icon_stages_an_open_cell_editor_then_saves(cx: &mut TestAppContext) {
    let o = browse(cx);
    type_in_cell(cx, &o, "z");
    let pane = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).results_pane().clone());
    assert_eq!(pane.read_with(cx, |p, _| p.editing()), Some((0, 1)));
    click_id(cx, o.win, "query-save");
    let m = o.model(cx);
    wait_prompt(cx, &m);
    assert_eq!(
        m.read_with(cx, |m, _| m.edits().cell(0, 1).cloned()),
        Some(EditValue::Text("z".into()))
    );
    assert!(!present(cx, o.win, "save-name"));
    m.update(cx, |m, cx| m.rollback(cx));
}

#[gpui_kit::test]
fn ctrl_s_in_the_editor_with_nothing_staged_or_open_saves_the_query(cx: &mut TestAppContext) {
    let o = browse(cx);
    focus_editor(cx, &o);
    press(cx, &o, "ctrl-s");
    wait_real(cx, "the name dialog", |cx| present(cx, o.win, "save-name"));
    let m = o.model(cx);
    m.read_with(cx, |m, _| {
        assert!(m.apply_prompt().is_none());
        assert!(m.edits().is_empty());
    });
}

#[gpui_kit::test]
fn ctrl_shift_s_in_the_editor_saves_the_query_not_the_edits(cx: &mut TestAppContext) {
    let o = browse(cx);
    staged(cx, &o);
    focus_editor(cx, &o);
    press(cx, &o, "ctrl-shift-s");
    wait_real(cx, "the name dialog", |cx| present(cx, o.win, "save-name"));
    let m = o.model(cx);
    m.read_with(cx, |m, _| {
        assert!(m.apply_prompt().is_none());
        assert_eq!(m.save_state(), SaveState::Idle);
        assert_eq!(m.edits().len(), 1);
    });
}

#[gpui_kit::test]
fn ctrl_shift_s_in_the_grid_saves_the_query_not_the_edits(cx: &mut TestAppContext) {
    let o = browse(cx);
    staged(cx, &o);
    focus_grid(cx, &o);
    press(cx, &o, "ctrl-shift-s");
    wait_real(cx, "the name dialog", |cx| present(cx, o.win, "save-name"));
    assert!(o.model(cx).read_with(cx, |m, _| m.apply_prompt().is_none()));
}

fn cancelled_by(cx: &mut TestAppContext, focus: fn(&mut TestAppContext, &Opened)) {
    let o = opened(cx, T, "", 0);
    o.run(cx, LONG);
    let m = o.model(cx);
    wait_real(cx, "the connection", |cx| {
        m.read_with(cx, |m, _| m.read_only().is_some())
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    focus(cx, &o);
    press(cx, &o, "ctrl-.");
    o.idle(cx);
    let text = label_of(cx, o.win, ("statement-result", 0usize)).unwrap();
    assert!(text.contains("cancelled"), "{text}");
}

#[gpui_kit::test]
fn ctrl_period_cancels_from_the_editor(cx: &mut TestAppContext) {
    cancelled_by(cx, focus_editor);
}

#[gpui_kit::test]
fn ctrl_period_cancels_from_the_grid(cx: &mut TestAppContext) {
    cancelled_by(cx, focus_grid);
}

// The kit's own action tooltip draws a light box with plain key text, so each toolbar
// button's keyed tooltip sits on a `<id>-tip` wrapper drawn by `theme::tooltip_keys`.
#[gpui_kit::test]
fn toolbar_buttons_carry_the_keyed_tooltip(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    for id in ["format-tip", "query-save-tip"] {
        assert!(present(cx, o.win, id), "{id}");
    }
    assert!(!present(cx, o.win, "cancel-tip"));
}

#[gpui_kit::test]
fn the_strip_save_is_enabled_over_an_open_editor_with_nothing_staged(cx: &mut TestAppContext) {
    let o = browse(cx);
    o.assert_inert(cx, "save-edits");
    type_in_cell(cx, &o, "z");
    assert!(o.model(cx).read_with(cx, |m, _| m.edits().is_empty()));
    // Like Ctrl+S, the click stages the open editor's text before it saves.
    assert!(o.strip_enabled(cx, "save-edits"));
    click_id(cx, o.win, "save-edits");
    let m = o.model(cx);
    wait_prompt(cx, &m);
    assert_eq!(
        m.read_with(cx, |m, _| m.edits().cell(0, 1).cloned()),
        Some(EditValue::Text("z".into()))
    );
    m.update(cx, |m, cx| m.rollback(cx));
}

// The Save button stages and saves edits first, so its tooltip boxes the save-edits keys.
#[gpui_kit::test]
fn the_save_button_tooltip_boxes_the_save_edits_keys(cx: &mut TestAppContext) {
    support::init(cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("keymap.json");
    std::fs::write(&path, r#"{"edits.save": ["f8"]}"#).unwrap();
    cx.update(|cx| dbento_app::keymap::install_user(Some(path.clone()), cx));
    cx.update(|cx| assert_eq!(save_button_keys(cx).as_deref(), Some("f8")));
    std::fs::write(&path, r#"{"query.save": ["f9"]}"#).unwrap();
    cx.update(dbento_app::keymap::reload);
    cx.update(|cx| assert_eq!(save_button_keys(cx).as_deref(), Some("secondary-s")));
}
