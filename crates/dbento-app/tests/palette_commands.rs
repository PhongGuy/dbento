//! Commands from the palette run what their keys run; `?` lists the keymap.
mod support;
use dbento_app::keymap::{CANCEL_QUERY_KEYS, chip_keys, shortcuts};
use dbento_app::palette::{Mode, PaletteCommand};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use support::{Opened, label_of, launched, opened, palette_rows, present, wait_real};

fn open(cx: &mut TestAppContext, o: &Opened, mode: Mode) {
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.open_palette(mode, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
}
fn type_text(cx: &mut TestAppContext, o: &Opened, text: &str) {
    cx.update_window(o.win, |_, w, cx| w.input(text, cx))
        .unwrap();
    cx.run_until_parked();
}
fn enter_on(cx: &mut TestAppContext, o: &Opened, first: &str) {
    wait_real(cx, first, |cx| {
        palette_rows(cx, o.win).first().map(String::as_str) == Some(first)
    });
    cx.update_window(o.win, |_, w, cx| w.press("enter", cx))
        .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn reload_runs_from_the_palette_with_no_tab_open(cx: &mut TestAppContext) {
    let o = launched(cx, "", "");
    let toml = o.fx.dir.path().join("dbento.toml");
    let mut text = std::fs::read_to_string(&toml).unwrap();
    text.push_str("\n[connections.extra]\nengine = \"sqlite\"\npath = \"data/x.db\"\n");
    std::fs::write(&toml, text).unwrap();
    open(cx, &o, Mode::Commands);
    type_text(cx, &o, "reload");
    enter_on(cx, &o, "Reload dbento.toml");
    assert!(!present(cx, o.win, "palette"));
    wait_real(cx, "the reload", |cx| {
        o.shell
            .read_with(cx, |s, _| s.workspace().unwrap().connections().len() == 3)
    });
}

#[gpui_kit::test]
fn tab_commands_from_the_palette_do_nothing_with_no_tab_open(cx: &mut TestAppContext) {
    let o = launched(cx, "", "");
    use PaletteCommand as C;
    for cmd in [
        C::CloseTab,
        C::SaveQuery,
        C::RunStatement,
        C::RunAll,
        C::FormatSql,
        C::CancelQuery,
    ] {
        let shell = o.shell.clone();
        cx.update_window(o.win, |_, w, cx| {
            shell.update(cx, |s, cx| s.run_palette_command(cmd, w, cx))
        })
        .unwrap();
        cx.run_until_parked();
        assert!(
            o.shell.read_with(cx, |s, _| s.tabs().is_empty()),
            "{cmd:?} opened a tab"
        );
        assert!(!present(cx, o.win, "palette"), "{cmd:?}");
    }
}

#[gpui_kit::test]
fn history_from_the_palette_opens_the_panel(cx: &mut TestAppContext) {
    let o = opened(cx, "CREATE TABLE history_log(id INTEGER);", "", 0);
    o.run(cx, "SELECT 1");
    o.idle(cx);
    open(cx, &o, Mode::All);
    type_text(cx, &o, "history");
    // The Tables group sits above Commands, so the clicked row is in a later section.
    wait_real(cx, "both groups", |cx| {
        let rows = palette_rows(cx, o.win);
        rows.iter().any(|r| r == "main.history_log \u{b7} lite")
            && rows.iter().any(|r| r == "Toggle history")
    });
    support::palette_pick(cx, o.win, "Toggle history");
    assert!(o.shell.read_with(cx, |s, _| s.history_open()));
    assert_eq!(
        o.shell.read_with(cx, |s, _| s.tabs().len()),
        1,
        "no table opened"
    );
}

#[gpui_kit::test]
fn format_from_the_palette_formats_the_active_editor(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let raw = "select a,b from t";
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql(raw, w, cx))
    })
    .unwrap();
    open(cx, &o, Mode::Commands);
    type_text(cx, &o, "format");
    enter_on(cx, &o, "Format SQL");
    let text = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).sql(cx));
    assert_ne!(text, raw);
}

#[gpui_kit::test]
fn keyboard_shortcuts_switches_to_the_list(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    open(cx, &o, Mode::Commands);
    type_text(cx, &o, "keyboard");
    enter_on(cx, &o, "Keyboard shortcuts");
    assert!(present(cx, o.win, "palette"));
    let first = cx.update(|cx| shortcuts(cx))[0].label;
    wait_real(cx, "the list", |cx| {
        palette_rows(cx, o.win).first().map(String::as_str) == Some(first)
    });
}

#[gpui_kit::test]
fn the_shortcut_list_shows_cancel_with_its_chip(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    open(cx, &o, Mode::Shortcuts);
    type_text(cx, &o, "cancel query");
    wait_real(cx, "cancel", |cx| {
        palette_rows(cx, o.win).first().map(String::as_str) == Some("Cancel query")
    });
    assert_eq!(
        label_of(cx, o.win, ("palette-keys", 0usize)),
        Some(chip_keys(CANCEL_QUERY_KEYS))
    );
}

#[gpui_kit::test]
fn confirming_a_shortcut_row_closes_the_palette(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    open(cx, &o, Mode::Shortcuts);
    type_text(cx, &o, "cancel query");
    enter_on(cx, &o, "Cancel query");
    assert!(!present(cx, o.win, "palette"));
}

#[gpui_kit::test]
fn a_shortcut_with_several_chords_is_one_row_with_every_chip(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    open(cx, &o, Mode::Shortcuts);
    type_text(cx, &o, "format sql");
    wait_real(cx, "format", |cx| {
        palette_rows(cx, o.win).first().map(String::as_str) == Some("Format SQL")
    });
    let rows = palette_rows(cx, o.win);
    assert_eq!(rows.iter().filter(|r| *r == "Format SQL").count(), 1);
    let chips: Vec<_> = cx
        .update(|cx| shortcuts(cx))
        .into_iter()
        .filter(|s| s.label == "Format SQL")
        .map(|s| chip_keys(&s.keys))
        .collect();
    assert_eq!(chips.len(), 3);
    assert_eq!(
        label_of(cx, o.win, ("palette-keys", 0usize)),
        Some(chips.join(", "))
    );
}

#[gpui_kit::test]
fn close_tab_from_the_palette_asks_on_edited_sql(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql("select 42", w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    assert!(
        tab.read_with(cx, |t, cx| t.edited(cx)),
        "precondition: edited"
    );
    open(cx, &o, Mode::Commands);
    type_text(cx, &o, "close tab");
    enter_on(cx, &o, "Close tab");
    wait_real(cx, "the question", |cx| {
        present(cx, o.win, "confirm-dialog")
    });
    assert_eq!(o.shell.read_with(cx, |s, _| s.tabs().len()), 1);
}

#[gpui_kit::test]
fn run_all_from_the_palette_asks_on_delete_without_where(cx: &mut TestAppContext) {
    let o = opened(
        cx,
        "CREATE TABLE t(id INTEGER); INSERT INTO t VALUES (1),(2);",
        "",
        0,
    );
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql("DELETE FROM t", w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    open(cx, &o, Mode::Commands);
    type_text(cx, &o, "run all");
    enter_on(cx, &o, "Run all");
    wait_real(cx, "the question", |cx| {
        present(cx, o.win, "confirm-dialog")
    });
    assert_eq!(o.fx.count("SELECT count(*) FROM t"), 2);
}
