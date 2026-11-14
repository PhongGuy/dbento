//! The editor's right-click menu holds the app's actions, not the kit's LSP items.
use crate::support::{label_of, opened, present, wait_real};
use dbento_app::keymap::{
    EXPLAIN_ANALYZE_KEYS, EXPLAIN_KEYS, FORMAT_SQL_KEYS, RUN_STATEMENT_KEYS, SAVE_QUERY_KEYS,
    TOGGLE_COMMENT_KEYS, chip_keys,
};
use dbento_app::query_tab::{editor_menu_entries, formats};
use dbento_core::Engine;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

#[gpui_kit::test]
fn the_menu_holds_run_format_comment_save_and_the_clipboard(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let e = editor_menu_entries(true, true, true, cx);
        let ids: Vec<_> = e.iter().map(|e| e.id).collect();
        assert_eq!(
            ids,
            [
                "editor-menu-run",
                "editor-menu-format",
                "editor-menu-explain",
                "editor-menu-explain-analyze",
                "editor-menu-toggle-comment",
                "editor-menu-save",
                "editor-menu-cut",
                "editor-menu-copy",
                "editor-menu-paste",
                "editor-menu-select-all",
            ]
        );
        let keys: Vec<_> = e.iter().map(|e| e.keys.as_deref()).collect();
        assert_eq!(
            keys,
            [
                Some(RUN_STATEMENT_KEYS),
                Some(FORMAT_SQL_KEYS),
                Some(EXPLAIN_KEYS),
                Some(EXPLAIN_ANALYZE_KEYS),
                Some(TOGGLE_COMMENT_KEYS),
                Some(SAVE_QUERY_KEYS),
                None,
                None,
                None,
                None,
            ]
        );
        assert!(e.iter().all(|e| e.enabled));
    });
}

#[gpui_kit::test]
fn cut_and_copy_wait_for_a_selection(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let off: Vec<_> = editor_menu_entries(false, true, true, cx)
            .into_iter()
            .filter(|e| !e.enabled)
            .map(|e| e.id)
            .collect();
        assert_eq!(off, ["editor-menu-cut", "editor-menu-copy"]);
    });
}

#[gpui_kit::test]
fn right_click_in_the_editor_opens_the_menu_and_run_runs(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql("SELECT 7 AS a", w, cx))
    })
    .unwrap();
    cx.update_window(o.win, |_, w, cx| w.right_click("editor", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(present(cx, o.win, "editor-menu-run"));
    assert_eq!(
        label_of(cx, o.win, "editor-menu-run-keys"),
        Some(chip_keys(RUN_STATEMENT_KEYS))
    );
    cx.update_window(o.win, |_, w, cx| w.click("editor-menu-run", cx))
        .unwrap();
    cx.run_until_parked();
    wait_real(cx, "the run", |cx| {
        label_of(cx, o.win, "cell-0-0").as_deref() == Some("7")
    });
}

#[gpui_kit::test]
fn right_click_moves_the_caret_so_run_runs_the_statement_clicked(cx: &mut TestAppContext) {
    // The second statement spans enough lines that the editor's centre lands inside it,
    // while the caret starts in the first.
    let o = opened(cx, "", "", 0);
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    let sql = format!("SELECT 1 AS a;\nSELECT{}2 AS b", "\n".repeat(80));
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| {
            t.set_sql(&sql, w, cx);
            t.editor()
                .update(cx, |e, cx| e.set_selected_range(0..0, cx));
        })
    })
    .unwrap();
    cx.update_window(o.win, |_, w, cx| w.right_click("editor", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(o.win, |_, w, cx| w.click("editor-menu-run", cx))
        .unwrap();
    cx.run_until_parked();
    wait_real(cx, "the run", |cx| {
        label_of(cx, o.win, "cell-0-0").as_deref() == Some("2")
    });
}

#[gpui_kit::test]
fn cut_from_the_menu_cuts_a_selection_and_waits_without_one(cx: &mut TestAppContext) {
    // Enough lines that the right-click at the editor's centre lands inside the text.
    let sql = "SELECT 7 AS a\n".repeat(80);
    let o = opened(cx, "", "", 0);
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    let editor = tab.read_with(cx, |t, _| t.editor().clone());
    let cut = |cx: &mut TestAppContext, range: std::ops::Range<usize>| {
        cx.update_window(o.win, |_, w, cx| {
            tab.update(cx, |t, cx| t.set_sql(&sql, w, cx));
            editor.update(cx, |e, cx| e.set_selected_range(range, cx));
        })
        .unwrap();
        cx.update_window(o.win, |_, w, cx| w.right_click("editor", cx))
            .unwrap();
        cx.run_until_parked();
        cx.update_window(o.win, |_, w, cx| w.click("editor-menu-cut", cx))
            .unwrap();
        cx.run_until_parked();
        editor.read_with(cx, |e, _| e.value().to_string())
    };
    assert_eq!(cut(cx, 0..0), sql, "a disabled Cut does nothing");
    assert!(
        present(cx, o.win, "editor-menu-cut"),
        "the disabled item keeps the menu open"
    );
    cx.update_window(o.win, |_, w, cx| w.press("escape", cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(
        cut(cx, 0..sql.len()),
        "",
        "a right-click inside the selection keeps it"
    );
}

#[gpui_kit::test]
fn without_explain_the_menu_leaves_both_explain_items_out(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let ids: Vec<_> = editor_menu_entries(true, false, true, cx)
            .iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(
            ids,
            [
                "editor-menu-run",
                "editor-menu-format",
                "editor-menu-toggle-comment",
                "editor-menu-save",
                "editor-menu-cut",
                "editor-menu-copy",
                "editor-menu-paste",
                "editor-menu-select-all",
            ]
        );
    });
}

#[gpui_kit::test]
fn a_redis_editor_offers_neither_format_nor_explain(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let e = Engine::Redis;
        let ids: Vec<_> = editor_menu_entries(true, e.capabilities().explain, formats(e), cx)
            .iter()
            .map(|m| m.id)
            .collect();
        assert!(ids.contains(&"editor-menu-run"), "{ids:?}");
        assert!(
            !ids.contains(&"editor-menu-format") && !ids.contains(&"editor-menu-explain"),
            "{ids:?}"
        );
        assert!(formats(Engine::Postgres));
    });
}

// The Save chip names the key the keymap binds first, so a rebinding shows in the menu.
#[gpui_kit::test]
fn the_save_chip_follows_a_rebound_save_query(cx: &mut TestAppContext) {
    let files = tempfile::tempdir().unwrap();
    let path = files.path().join("keymap.json");
    std::fs::write(&path, r#"{"query.save": ["ctrl-alt-s"]}"#).unwrap();
    cx.update(|cx| {
        dbento_app::keymap::install_user(Some(path), cx);
        let save = editor_menu_entries(true, true, true, cx)
            .into_iter()
            .find(|e| e.id == "editor-menu-save")
            .unwrap();
        assert_eq!(save.keys.as_deref(), Some("ctrl-alt-s"));
    });
}

#[gpui_kit::test]
fn toggle_comment_from_the_menu_comments_the_caret_line(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    let editor = tab.read_with(cx, |t, _| t.editor().clone());
    let sql = "SELECT 7 AS a\n".repeat(80);
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql(&sql, w, cx));
        editor.update(cx, |e, cx| e.set_selected_range(0..0, cx));
    })
    .unwrap();
    cx.update_window(o.win, |_, w, cx| w.right_click("editor", cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(
        label_of(cx, o.win, "editor-menu-toggle-comment-keys"),
        Some(chip_keys(TOGGLE_COMMENT_KEYS))
    );
    cx.update_window(o.win, |_, w, cx| w.click("editor-menu-toggle-comment", cx))
        .unwrap();
    cx.run_until_parked();
    let text = editor.read_with(cx, |e, _| e.value().to_string());
    assert_eq!(text.matches("-- ").count(), 1, "one line commented");
}

// A disabled row reads as unavailable: dimmer than an enabled row's secondary text.
#[gpui_kit::test]
fn a_disabled_menu_row_is_drawn_faded(cx: &mut TestAppContext) {
    crate::support::init(cx);
    cx.update(|cx| {
        use gpui_kit::component::ActiveTheme as _;
        let faded = dbento_app::ui::menu_row_text(false, cx);
        assert_eq!(dbento_app::ui::menu_row_text(true, cx), None);
        assert_eq!(
            faded,
            Some(
                cx.theme()
                    .muted_foreground
                    .opacity(dbento_app::ui::DISABLED_OPACITY)
            )
        );
        let muted = cx.theme().muted_foreground;
        assert!(faded.is_some_and(|f| f.a <= muted.a * 0.5), "{faded:?}");
    });
}
