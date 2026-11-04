mod support;
use dbento_app::explorer::connection_id;
use dbento_app::sidebar::engine_label;
use dbento_core::Engine;
use gpui_kit::TestAppContext;
use support::{label_of, opened, present};

#[test]
fn the_driver_hint_names_the_engine() {
    assert_eq!(engine_label(Engine::Sqlite), "sqlite");
    assert_eq!(engine_label(Engine::Postgres), "postgres");
}

#[gpui_kit::test]
fn the_restyled_chrome_keeps_every_id(cx: &mut TestAppContext) {
    let o = opened(cx, "", "read_only = true", 0);
    for id in [
        "shell",
        "sidebar",
        "connection-list",
        "tab-area",
        "tab-bar",
        "editor",
        "run",
        "run-split",
        "read-only-badge",
        "result-grid",
        "status-bar",
        "status-connection",
    ] {
        assert!(present(cx, o.win, id), "{id}");
    }
    assert_eq!(
        label_of(cx, o.win, connection_id("lite")).as_deref(),
        Some("lite")
    );
    assert_eq!(
        label_of(cx, o.win, connection_id("nopg")).as_deref(),
        Some("nopg")
    );
    assert_eq!(
        label_of(cx, o.win, ("tab", 0usize)).as_deref(),
        Some("Query 1")
    );
}
