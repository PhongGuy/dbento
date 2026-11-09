//! keymap.json at startup and on Reload changes which keys run an action.
use crate::support;
use crate::support::{click_id, present};
use dbento_app::app::start;
use dbento_app::keymap::{self, OpenSettings};
use dbento_app::user_keymap::KeymapProblem;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, TestAppContext};

fn press(cx: &mut TestAppContext, win: AnyWindowHandle, key: &str) {
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        w.press(key, cx)
    })
    .unwrap();
    cx.run_until_parked();
}
fn started(cx: &mut TestAppContext, keymap: Option<&str>) -> (tempfile::TempDir, AnyWindowHandle) {
    support::init(cx);
    let files = tempfile::tempdir().unwrap();
    if let Some(text) = keymap {
        std::fs::write(files.path().join("keymap.json"), text).unwrap();
    }
    let (win, _) = cx
        .update(|cx| start(support::services(files.path()), None, cx))
        .unwrap();
    cx.run_until_parked();
    (files, win)
}

#[gpui_kit::test]
fn an_override_moves_the_action_to_the_new_keys(cx: &mut TestAppContext) {
    let (_f, win) = started(cx, Some(r#"{"app.open_settings": ["ctrl-alt-s"]}"#));
    press(cx, win, "ctrl-,");
    assert!(
        !present(cx, win, "settings-dialog"),
        "the old keys still open settings"
    );
    press(cx, win, "ctrl-alt-s");
    assert!(present(cx, win, "settings-dialog"));
    assert_eq!(
        cx.update(|cx| keymap::keys_for(&OpenSettings, cx))
            .as_deref(),
        Some("ctrl-alt-s")
    );
}

#[gpui_kit::test]
fn unbinding_the_settings_key_leaves_the_rail(cx: &mut TestAppContext) {
    // The rail shows only with a workspace open, so this starts on a fixture one.
    support::init(cx);
    support::init_tokio(cx);
    let (fx, files) = (support::fixture("", ""), tempfile::tempdir().unwrap());
    std::fs::write(
        files.path().join("keymap.json"),
        r#"{"app.open_settings": []}"#,
    )
    .unwrap();
    let (win, _) = cx
        .update(|cx| {
            start(
                support::services(files.path()),
                Some(fx.dir.path().into()),
                cx,
            )
        })
        .unwrap();
    assert!(present(cx, win, "connection-list"));
    press(cx, win, "ctrl-,");
    assert!(!present(cx, win, "settings-dialog"));
    assert_eq!(cx.update(|cx| keymap::keys_for(&OpenSettings, cx)), None);
    click_id(cx, win, "rail-settings");
    assert!(present(cx, win, "settings-dialog"));
}

#[gpui_kit::test]
fn a_rebuild_keeps_the_kit_bindings_and_the_run_keys_win(cx: &mut TestAppContext) {
    support::init(cx);
    let count = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let map = cx.key_bindings();
            let map = map.borrow();
            map.bindings()
                .filter(|b| !b.action().name().starts_with("dbento::"))
                .count()
        })
    };
    let order = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let map = cx.key_bindings();
            let map = map.borrow();
            map.bindings()
                .map(|b| b.action().name().to_string())
                .collect::<Vec<_>>()
        })
    };
    let before = count(cx);
    let kit_before = cx.update(|cx| {
        let map = cx.key_bindings();
        let map = map.borrow();
        map.bindings()
            .filter(|b| !b.action().name().starts_with("dbento::"))
            .map(|b| b.action().name().to_string())
            .collect::<Vec<_>>()
    });
    let files = tempfile::tempdir().unwrap();
    std::fs::write(
        files.path().join("keymap.json"),
        r#"{"tabs.new": ["ctrl-alt-n"]}"#,
    )
    .unwrap();
    cx.update(|cx| keymap::install_user(Some(files.path().join("keymap.json")), cx));
    assert_eq!(count(cx), before, "a kit binding was lost");
    let kit_after: Vec<_> = order(cx)
        .into_iter()
        .filter(|n| !n.starts_with("dbento::"))
        .collect();
    assert_eq!(kit_after, kit_before, "the kit bindings changed order");
    // Kit bindings keep their own order and all come before ours, so the table's run keys
    // still win the tie over the editor's own secondary-enter.
    let names = order(cx);
    let first_ours = names
        .iter()
        .position(|n| n.starts_with("dbento::"))
        .unwrap();
    assert!(
        names[first_ours..]
            .iter()
            .all(|n| n.starts_with("dbento::")),
        "a kit binding after ours"
    );
    assert_eq!(
        names.len() - first_ours,
        keymap::bindings().len(),
        "tabs.new keeps its one row"
    );
}

#[gpui_kit::test]
fn reload_applies_an_edited_file(cx: &mut TestAppContext) {
    let (f, win) = started(cx, Some(r#"{"app.open_settings": ["ctrl-alt-s"]}"#));
    std::fs::write(f.path().join("keymap.json"), "{}").unwrap();
    let r = cx.update(keymap::reload);
    assert_eq!(
        r,
        keymap::Reloaded {
            problems: 0,
            kept_previous: false
        }
    );
    press(cx, win, "ctrl-,");
    assert!(present(cx, win, "settings-dialog"));
}

#[gpui_kit::test]
fn reload_of_a_broken_file_keeps_the_previous_keys_and_the_file(cx: &mut TestAppContext) {
    let (f, win) = started(cx, Some(r#"{"app.open_settings": ["ctrl-alt-s"]}"#));
    let path = f.path().join("keymap.json");
    std::fs::write(&path, "{ \"app.open_settings\": [").unwrap();
    let r = cx.update(keymap::reload);
    assert!(r.kept_previous);
    assert!(path.exists() && !f.path().join("keymap.json.bad").exists());
    assert!(matches!(
        cx.update(|cx| keymap::problems(cx)).as_slice(),
        [KeymapProblem::Unparseable { .. }]
    ));
    press(cx, win, "ctrl-alt-s");
    assert!(present(cx, win, "settings-dialog"));
}

#[gpui_kit::test]
fn a_broken_file_at_startup_moves_aside_and_defaults_apply(cx: &mut TestAppContext) {
    let (f, win) = started(cx, Some("not json"));
    assert!(f.path().join("keymap.json.bad").exists());
    press(cx, win, "ctrl-,");
    assert!(present(cx, win, "settings-dialog"));
}

#[gpui_kit::test]
fn an_unreadable_file_at_startup_warns_once(cx: &mut TestAppContext) {
    support::init(cx);
    let files = tempfile::tempdir().unwrap();
    std::fs::create_dir(files.path().join("keymap.json")).unwrap();
    let (win, _) = cx
        .update(|cx| start(support::services(files.path()), None, cx))
        .unwrap();
    assert_eq!(
        support::label_of(cx, win, "notice").as_deref(),
        Some("keymap.json could not be read; the default keys are in use")
    );
}

#[gpui_kit::test]
fn a_user_keymap_installed_before_bind_keeps_the_kit_bindings(cx: &mut TestAppContext) {
    let kit = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let map = cx.key_bindings();
            let map = map.borrow();
            map.bindings()
                .filter(|b| !b.action().name().starts_with("dbento::"))
                .count()
        })
    };
    support::init_kit_only(cx);
    let before = kit(cx);
    assert!(before > 0);
    let files = tempfile::tempdir().unwrap();
    cx.update(|cx| keymap::install_user(Some(files.path().join("keymap.json")), cx));
    assert_eq!(kit(cx), before, "the kit bindings were cleared");
}

#[test]
fn reload_notices_name_the_outcome() {
    assert_eq!(
        keymap::Reloaded {
            problems: 0,
            kept_previous: false
        }
        .notice()
        .text,
        "Keymap reloaded"
    );
    assert_eq!(
        keymap::Reloaded {
            problems: 2,
            kept_previous: false
        }
        .notice()
        .text,
        "Keymap reloaded, 2 problems"
    );
    assert!(
        keymap::Reloaded {
            problems: 1,
            kept_previous: true
        }
        .notice()
        .text
        .contains("previous keys are kept")
    );
}
