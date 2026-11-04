//! The bindings table is the one source for `bind`, key chips and the shortcut list.
mod support;

use dbento_app::keymap::{
    self, COMMANDS_KEYS, FORMAT_SQL_ALIASES, FORMAT_SQL_KEYS, NEW_TAB_KEYS, OPEN_QUERY_KEYS,
    PALETTE_KEYS, SAVE_QUERY_KEYS, Shortcut,
};
use dbento_app::theme::{APPEARANCE_KEYS, CycleAppearance};
use gpui_kit::{KeyBindingContextPredicate, Keystroke, TestAppContext};

fn strokes(keys: &str) -> Vec<Keystroke> {
    keys.split_whitespace()
        .map(|k| Keystroke::parse(k).unwrap())
        .collect()
}

#[gpui_kit::test]
fn bind_registers_exactly_the_bindings_table(cx: &mut TestAppContext) {
    support::init(cx);
    let table = keymap::bindings();
    cx.update(|cx| {
        let map = cx.key_bindings();
        let map = map.borrow();
        let ours: Vec<_> = map
            .bindings()
            .filter(|b| b.action().name().starts_with("dbento::"))
            .collect();
        assert_eq!(
            ours.len(),
            table.len(),
            "a dbento binding outside the table"
        );
        for (b, row) in ours.iter().zip(&table) {
            assert_eq!(b.action().name(), (row.action)().name(), "{}", row.keys);
            assert_eq!(
                b.match_keystrokes(&strokes(row.keys)),
                Some(false),
                "{}",
                row.keys
            );
            let want = row
                .context
                .map(|c| KeyBindingContextPredicate::parse(c).unwrap());
            assert_eq!(b.predicate().map(|p| (*p).clone()), want, "{}", row.keys);
        }
    });
}

#[gpui_kit::test]
fn a_second_install_binds_nothing_more(cx: &mut TestAppContext) {
    support::init(cx);
    cx.update(dbento_app::theme::install);
    let ours = cx.update(|cx| {
        let map = cx.key_bindings();
        let map = map.borrow();
        map.bindings()
            .filter(|b| b.action().name().starts_with("dbento::"))
            .count()
    });
    assert_eq!(ours, keymap::bindings().len());
}

#[gpui_kit::test]
fn keys_for_gives_the_last_binding_of_an_action(cx: &mut TestAppContext) {
    let keys = |cx: &mut TestAppContext, a: &dyn gpui_kit::Action| {
        cx.update(|cx| keymap::keys_for(a, cx))
            .map(|k| k.to_string())
    };
    assert_eq!(keys(cx, &keymap::NewTab).as_deref(), Some(NEW_TAB_KEYS));
    assert_eq!(
        keys(cx, &keymap::FormatSql).as_deref(),
        Some(FORMAT_SQL_KEYS)
    );
    assert_eq!(
        keys(cx, &keymap::OpenQuery).as_deref(),
        Some(OPEN_QUERY_KEYS)
    );
    assert_eq!(
        keys(cx, &keymap::OpenPalette).as_deref(),
        Some(PALETTE_KEYS)
    );
    assert_eq!(
        keys(cx, &keymap::OpenCommands).as_deref(),
        Some(COMMANDS_KEYS)
    );
    assert_eq!(keys(cx, &CycleAppearance).as_deref(), Some(APPEARANCE_KEYS));
    assert_eq!(
        keys(cx, &keymap::CancelQuery).as_deref(),
        Some(keymap::CANCEL_QUERY_KEYS)
    );
}

#[gpui_kit::test]
fn the_shortcut_list_names_each_chord_once(cx: &mut TestAppContext) {
    let list = cx.update(|cx| keymap::shortcuts(cx));
    let format: Vec<_> = list
        .iter()
        .filter(|s| s.label == "Format SQL")
        .map(|s| s.keys.clone())
        .collect();
    assert_eq!(
        format,
        [
            FORMAT_SQL_ALIASES[0],
            FORMAT_SQL_ALIASES[1],
            FORMAT_SQL_KEYS
        ]
    );
    for s in &list {
        assert_eq!(
            list.iter().filter(|o| *o == s).count(),
            1,
            "{s:?} listed twice"
        );
    }
    for want in [
        Shortcut {
            label: "Open palette",
            keys: PALETTE_KEYS.into(),
        },
        Shortcut {
            label: "Open commands",
            keys: COMMANDS_KEYS.into(),
        },
        Shortcut {
            label: "Open saved query",
            keys: OPEN_QUERY_KEYS.into(),
        },
        Shortcut {
            label: "Toggle appearance",
            keys: APPEARANCE_KEYS.into(),
        },
        Shortcut {
            label: "Cancel query",
            keys: keymap::CANCEL_QUERY_KEYS.into(),
        },
        Shortcut {
            label: "Run statement",
            keys: keymap::RUN_STATEMENT_KEYS.into(),
        },
    ] {
        assert!(list.contains(&want), "missing {want:?}");
    }
    for (label, keys) in [
        ("Switch to recent tab", "ctrl-tab"),
        ("Switch to recent tab, reversed", "ctrl-shift-tab"),
        ("Go to tab 1", "secondary-1"),
        ("Go to tab 8", "secondary-8"),
        ("Go to last tab", "secondary-9"),
        ("Move tab left", "secondary-shift-pageup"),
        ("Move tab right", "secondary-shift-pagedown"),
    ] {
        let want = Shortcut {
            label,
            keys: keys.into(),
        };
        assert!(list.contains(&want), "missing {want:?}");
    }
    assert!(list.iter().all(|s| !s.label.is_empty()));
    // Ctrl+S has three rows (SaveQuery, SaveConnection, SaveEdits); only SaveEdits is listed.
    let ctrl_s: Vec<_> = list.iter().filter(|s| s.keys == SAVE_QUERY_KEYS).collect();
    assert_eq!(ctrl_s.len(), 1, "{ctrl_s:?}");
    assert_eq!(ctrl_s[0].label, "Save staged edits, else the query");
}

#[cfg(not(target_os = "macos"))]
#[test]
fn chips_spell_tab_and_the_page_keys_as_keycaps_do() {
    assert_eq!(keymap::plus_keys("ctrl-shift-tab"), "Ctrl+Shift+Tab");
    assert_eq!(
        keymap::plus_keys("secondary-shift-pageup"),
        "Ctrl+Shift+PgUp"
    );
    assert_eq!(
        keymap::plus_keys("secondary-shift-pagedown"),
        "Ctrl+Shift+PgDn"
    );
}

#[gpui_kit::test]
fn the_shortcut_list_names_the_explorer_tree_keys(cx: &mut TestAppContext) {
    let list = cx.update(|cx| keymap::shortcuts(cx));
    for (label, keys) in [
        ("Previous tree row", "up"),
        ("Next tree row", "down"),
        ("First tree row", "home"),
        ("Last tree row", "end"),
        ("Expand tree row, or enter it", "right"),
        ("Collapse tree row, or go up", "left"),
    ] {
        let want = Shortcut {
            label,
            keys: keys.into(),
        };
        assert!(list.contains(&want), "missing {want:?}");
    }
}

#[test]
fn chips_spell_the_vertical_arrows_and_home_and_end() {
    assert_eq!(keymap::chip_keys("up"), "\u{2191}");
    assert_eq!(keymap::chip_keys("down"), "\u{2193}");
    assert_eq!(keymap::chip_keys("home"), "Home");
    assert_eq!(keymap::chip_keys("end"), "End");
}

const IDS: [&str; 52] = [
    "view.cycle_appearance",
    "app.open_settings",
    "grid.copy_cells",
    "query.run_statement",
    "query.run_all",
    "query.format",
    "query.explain",
    "query.explain_analyze",
    "tabs.new",
    "tabs.close",
    "query.save",
    "query.cancel",
    "query.open_saved",
    "view.toggle_history",
    "view.toggle_row_inspector",
    "workspace.reload",
    "connection.new",
    "explorer.open_selected",
    "explorer.select_prev",
    "explorer.select_next",
    "explorer.select_first",
    "explorer.select_last",
    "explorer.expand",
    "explorer.collapse",
    "connection.save",
    "grid.edit_cell",
    "grid.delete_row",
    "edits.undo",
    "edits.redo",
    "edits.save",
    "cell_editor.commit",
    "cell_editor.commit_right",
    "cell_editor.cancel",
    "record.commit_field",
    "record.cancel_field",
    "nav.back",
    "nav.forward",
    "palette.open",
    "palette.commands",
    "tabs.recent",
    "tabs.recent_reverse",
    "tabs.go_to_1",
    "tabs.go_to_2",
    "tabs.go_to_3",
    "tabs.go_to_4",
    "tabs.go_to_5",
    "tabs.go_to_6",
    "tabs.go_to_7",
    "tabs.go_to_8",
    "tabs.go_to_last",
    "tabs.move_left",
    "tabs.move_right",
];

#[test]
fn the_command_ids_are_pinned() {
    assert_eq!(keymap::command_ids(), IDS.to_vec());
}

#[test]
fn every_action_has_exactly_one_id_and_every_id_one_action() {
    use std::collections::HashMap;
    let (mut by_action, mut by_id) = (HashMap::new(), HashMap::new());
    for b in keymap::bindings() {
        let name = (b.action)().name();
        assert!(
            b.id.contains('.') && b.id == b.id.to_lowercase(),
            "{}",
            b.id
        );
        assert_eq!(*by_action.entry(name).or_insert(b.id), b.id, "{name}");
        assert_eq!(*by_id.entry(b.id).or_insert(name), name, "{}", b.id);
    }
}

#[test]
fn default_keys_follow_the_last_row_of_the_id() {
    assert_eq!(keymap::default_keys("tabs.new"), Some(NEW_TAB_KEYS));
    assert_eq!(keymap::default_keys("query.format"), Some(FORMAT_SQL_KEYS));
    assert_eq!(
        keymap::default_keys("view.cycle_appearance"),
        Some(APPEARANCE_KEYS)
    );
    assert_eq!(keymap::default_keys("nope.nothing"), None);
}
