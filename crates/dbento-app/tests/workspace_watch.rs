//! The app with its real watcher: an outside edit reaches the explorer and an open saved
//! query, and the reload's own reads do not feed back into more reloads.
use crate::support::{fixture, init_then, init_tokio, palette_pick, present, services, wait_real};
use dbento_app::workspace_reload::enable_watching;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use std::fs;

#[gpui_kit::test]
fn outside_edits_arrive_through_the_watcher(cx: &mut TestAppContext) {
    init_then(cx, enable_watching);
    init_tokio(cx);
    let (fx, files) = (fixture("", ""), tempfile::tempdir().unwrap());
    let file = fx.dir.path().join("queries/lite/a.sql");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "SELECT 1").unwrap();
    let (win, shell) = cx
        .update(|cx| dbento_app::app::start(services(files.path()), Some(fx.dir.path().into()), cx))
        .unwrap();
    present(cx, win, "connection-list");
    crate::support::open_connection_row(cx, win, "lite");
    let e = shell.read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone());
    cx.update_window(win, |_, w, cx| {
        e.update(cx, |e, cx| e.focus(w, cx));
        w.press("ctrl-o", cx);
    })
    .unwrap();
    cx.run_until_parked();
    palette_pick(cx, win, "lite/a.sql");

    let toml = fs::read_to_string(fx.dir.path().join("dbento.toml")).unwrap();
    fs::write(
        fx.dir.path().join("dbento.toml"),
        format!("{toml}[connections.aaa]\nengine = \"sqlite\"\npath = \"data/app.db\"\n"),
    )
    .unwrap();
    fs::write(&file, "SELECT 2").unwrap();
    wait_real(cx, "the new connection", |cx| {
        shell.read_with(cx, |s, _| s.workspace().unwrap().declared("aaa").is_some())
    });
    wait_real(cx, "the tab's new text", |cx| {
        shell.read_with(cx, |s, cx| s.tab().unwrap().read(cx).sql(cx) == "SELECT 2")
    });

    // Let the reads' own access events pass, then nothing may start another reload.
    let quiet = std::time::Instant::now() + std::time::Duration::from_millis(1500);
    while std::time::Instant::now() < quiet {
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let settled = shell.read_with(cx, |s, _| s.reloads_run());
    let quiet = std::time::Instant::now() + std::time::Duration::from_millis(1000);
    while std::time::Instant::now() < quiet {
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        shell.read_with(cx, |s, _| s.reloads_run()),
        settled,
        "reloads feed no reloads"
    );
}
