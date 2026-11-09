//! Chips, palette hints and the shortcut list show the user's keys, not the compiled ones.
use crate::support;
use crate::support::{Opened, label_of, present};
use dbento_app::keymap::{self, NEW_TAB_KEYS, NewTab, Shortcut};
use dbento_app::palette::PaletteCommand;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

fn with_keymap(cx: &mut TestAppContext, text: &str) -> tempfile::TempDir {
    support::init(cx);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("keymap.json"), text).unwrap();
    cx.update(|cx| keymap::install_user(Some(dir.path().join("keymap.json")), cx));
    dir
}

// The app opened, then `text` written as its keymap file and reloaded, then the first tab's
// context menu opened.
fn tab_menu_with(cx: &mut TestAppContext, text: &str) -> Opened {
    let o = support::opened(cx, "", "", 0);
    std::fs::write(o.files.path().join("keymap.json"), text).unwrap();
    cx.update(keymap::reload);
    cx.update_window(o.win, |_, w, cx| w.right_click(("tab", 0usize), cx))
        .unwrap();
    cx.run_until_parked();
    o
}

#[gpui_kit::test]
fn the_palette_and_the_shortcut_list_show_an_override(cx: &mut TestAppContext) {
    let _d = with_keymap(cx, r#"{"tabs.new": ["ctrl-alt-n", "f9"]}"#);
    cx.update(|cx| {
        assert_eq!(
            PaletteCommand::NewTab.keys(cx).as_deref(),
            Some("ctrl-alt-n")
        );
        let list = keymap::shortcuts(cx);
        let new_tab: Vec<_> = list
            .iter()
            .filter(|s| s.label == "New tab")
            .map(|s| s.keys.to_string())
            .collect();
        assert_eq!(new_tab, vec!["ctrl-alt-n", "f9"]);
        assert!(!list.contains(&Shortcut {
            label: "New tab",
            keys: NEW_TAB_KEYS.into()
        }));
        assert_eq!(keymap::all_keys_for(&NewTab, cx).len(), 2);
    });
}

#[gpui_kit::test]
fn an_unbound_command_has_no_chip_and_no_shortcut_row(cx: &mut TestAppContext) {
    let _d = with_keymap(cx, r#"{"tabs.new": []}"#);
    cx.update(|cx| {
        assert_eq!(PaletteCommand::NewTab.keys(cx), None);
        assert!(keymap::shortcuts(cx).iter().all(|s| s.label != "New tab"));
    });
}

#[gpui_kit::test]
fn the_tab_menu_chip_shows_an_override(cx: &mut TestAppContext) {
    // One rendered chip end to end: the tab context menu's "Close" row, whose chip element
    // `tab-menu-close-keys` carries the chip text as its aria label (ui.rs menu_row).
    let o = tab_menu_with(cx, r#"{"tabs.close": ["ctrl-alt-w"]}"#);
    assert_eq!(
        label_of(cx, o.win, "tab-menu-close-keys"),
        Some(keymap::chip_keys("ctrl-alt-w"))
    );
}

#[gpui_kit::test]
fn an_unbound_tab_close_has_no_menu_chip(cx: &mut TestAppContext) {
    let o = tab_menu_with(cx, r#"{"tabs.close": []}"#);
    assert!(present(cx, o.win, "tab-menu-close"));
    assert!(!present(cx, o.win, "tab-menu-close-keys"));
}

#[test]
fn no_ui_file_reads_a_keys_constant() {
    fn scan(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                scan(&p, out)
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p)
            }
        }
    }
    // A keys constant is an upper-case identifier ending in _KEYS or _ALIASES.
    fn names_constant(line: &str) -> bool {
        line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .any(|w| {
                (w.ends_with("_KEYS") || w.ends_with("_ALIASES"))
                    && w.chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            })
    }
    let mut files = Vec::new();
    scan(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if name == "keymap.rs" {
            continue;
        }
        for (n, line) in std::fs::read_to_string(&path).unwrap().lines().enumerate() {
            let defining =
                name == "theme.rs" && line.trim_start().starts_with("pub const APPEARANCE_KEYS");
            assert!(
                defining || !names_constant(line),
                "{}:{}: {line}",
                path.display(),
                n + 1
            );
        }
    }
}

#[gpui_kit::test]
fn the_run_button_chip_follows_an_override(cx: &mut TestAppContext) {
    let o = support::opened(cx, "", "", 0);
    std::fs::write(
        o.files.path().join("keymap.json"),
        r#"{"query.run_statement": ["f5"]}"#,
    )
    .unwrap();
    cx.update(keymap::reload);
    cx.run_until_parked();
    let chip = label_of(cx, o.win, "run-keys");
    assert_eq!(chip, Some(keymap::chip_keys("f5")));
}
