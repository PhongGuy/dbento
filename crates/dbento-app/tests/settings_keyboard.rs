//! The Keyboard section: the file path, Open, Reload, the problem list and the command list.
use crate::support;
use crate::support::{click_id, label_of, present};
use dbento_app::app::start;
use dbento_app::keymap::{self, FileOpener};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, TestAppContext};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

fn opened(
    cx: &mut TestAppContext,
    keymap: Option<&str>,
) -> (
    tempfile::TempDir,
    AnyWindowHandle,
    Rc<RefCell<Vec<PathBuf>>>,
) {
    support::init(cx);
    let files = tempfile::tempdir().unwrap();
    if let Some(text) = keymap {
        std::fs::write(files.path().join("keymap.json"), text).unwrap();
    }
    let seen = Rc::new(RefCell::new(Vec::new()));
    let rec = Rc::clone(&seen);
    cx.update(|cx| {
        cx.set_global(FileOpener(Rc::new(move |p, _| {
            rec.borrow_mut().push(p.to_path_buf())
        })))
    });
    let (win, _) = cx
        .update(|cx| start(support::services(files.path()), None, cx))
        .unwrap();
    cx.run_until_parked();
    // The rail shows only with a workspace open, so the key opens the dialog here.
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        w.press("ctrl-,", cx)
    })
    .unwrap();
    cx.run_until_parked();
    assert!(
        present(cx, win, "settings-dialog"),
        "the settings dialog opens"
    );
    (files, win, seen)
}

#[gpui_kit::test]
fn a_clean_keymap_shows_its_path_and_no_problems(cx: &mut TestAppContext) {
    let (f, win, _) = opened(cx, None);
    let path = f.path().join("keymap.json");
    assert!(
        label_of(cx, win, "settings-keymap-path")
            .unwrap()
            .contains(&*path.to_string_lossy())
    );
    assert!(present(cx, win, "settings-keymap-ok"));
    assert!(!present(cx, win, "settings-keymap-problem-0"));
}

#[gpui_kit::test]
fn open_creates_an_empty_object_and_opens_it(cx: &mut TestAppContext) {
    let (f, win, seen) = opened(cx, None);
    let path = f.path().join("keymap.json");
    click_id(cx, win, "settings-keymap-open");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}\n");
    assert_eq!(*seen.borrow(), vec![path.clone()]);
    std::fs::write(&path, r#"{"tabs.new": ["f9"]}"#).unwrap();
    click_id(cx, win, "settings-keymap-open");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        r#"{"tabs.new": ["f9"]}"#,
        "an existing file is never rewritten"
    );
}

#[gpui_kit::test]
fn problems_are_listed_and_reload_clears_them(cx: &mut TestAppContext) {
    let (f, win, _) = opened(cx, Some(r#"{"nope.x": ["f1"], "tabs.close": "ctrl-w"}"#));
    assert!(present(cx, win, "settings-keymap-problem-0"));
    assert!(present(cx, win, "settings-keymap-problem-1"));
    assert!(!present(cx, win, "settings-keymap-ok"));
    let all = [0, 1]
        .map(|n| label_of(cx, win, format!("settings-keymap-problem-{n}")).unwrap())
        .join("\n");
    assert!(
        all.contains("nope.x") && all.contains("tabs.close"),
        "{all}"
    );
    std::fs::write(f.path().join("keymap.json"), r#"{"tabs.new": ["f9"]}"#).unwrap();
    click_id(cx, win, "settings-keymap-reload");
    assert!(present(cx, win, "settings-keymap-ok"));
    assert!(!present(cx, win, "settings-keymap-problem-0"));
}

#[gpui_kit::test]
fn the_command_list_names_every_id_with_its_effective_keys(cx: &mut TestAppContext) {
    let (_f, win, _) = opened(
        cx,
        Some(r#"{"tabs.new": ["f9"], "view.toggle_history": []}"#),
    );
    let n = keymap::command_ids().len();
    assert!(present(
        cx,
        win,
        format!("settings-keymap-command-{}", n - 1)
    ));
    assert!(!present(cx, win, format!("settings-keymap-command-{n}")));
    let ix = |id: &str| keymap::command_ids().iter().position(|c| *c == id).unwrap();
    assert_eq!(
        label_of(
            cx,
            win,
            format!("settings-keymap-command-{}", ix("tabs.new"))
        )
        .as_deref(),
        Some("tabs.new: f9")
    );
    assert_eq!(
        label_of(
            cx,
            win,
            format!("settings-keymap-command-{}", ix("view.toggle_history"))
        )
        .as_deref(),
        Some("view.toggle_history: unbound")
    );
}

#[gpui_kit::test]
fn the_footer_stays_on_screen_below_the_command_list(cx: &mut TestAppContext) {
    let (_f, win, _) = opened(cx, None);
    click_id(cx, win, "settings-close");
    assert!(!present(cx, win, "settings-dialog"));
}

// An id with several chords (query.format has three) must not squeeze its label: every label
// is at least as wide as its text set in the dialog's font.
#[gpui_kit::test]
fn command_rows_keep_their_label_beside_several_chords(cx: &mut TestAppContext) {
    let (_f, win, _) = opened(cx, None);
    let commands = cx.update(|cx| keymap::commands(cx));
    assert!(
        commands
            .iter()
            .any(|(id, _, k)| *id == "query.format" && k.len() >= 3),
        "query.format no longer has several chords"
    );
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        let size = w.rem_size() * 0.875;
        for (n, (id, label, _)) in commands.iter().enumerate() {
            let shown = w
                .find(format!("settings-keymap-command-{n}-label"))
                .bounds()
                .size
                .width;
            let run = gpui_kit::TextRun {
                len: label.len(),
                font: w.text_style().font(),
                color: gpui_kit::black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let text = w
                .text_system()
                .shape_line((*label).into(), size, &[run], None)
                .width;
            assert!(
                shown + gpui_kit::px(0.5) >= text,
                "{id}: label {label:?} gets {shown:?} of {text:?}"
            );
        }
    })
    .unwrap();
}
