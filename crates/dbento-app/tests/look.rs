mod support;
use dbento_app::app::start;
use dbento_app::explorer::connection_id;
use gpui_kit::TestAppContext;
use gpui_kit::component::ThemeMode;
use support::{opened, present};

const IDS: &[&str] = &[
    "connection-list",
    "tab-bar",
    "editor",
    "run",
    "run-split",
    "production-band",
    "run-summary",
    "read-only-badge",
    "result-grid",
    "cell-0-0",
    "result-footer",
    "footer-rows",
    "footer-elapsed",
    "status-connection",
    "status-rows",
    "title-search",
    "rail-explorer",
    "rail-queries",
    "history-toggle",
    "rail-approvals",
    "rail-appearance",
];

#[gpui_kit::test]
fn every_m3a_id_renders_in_both_modes(cx: &mut TestAppContext) {
    let o = opened(
        cx,
        "CREATE TABLE t(x); INSERT INTO t VALUES (1);",
        "color = \"red\"\nread_only = true",
        0,
    );
    o.run(cx, "SELECT x FROM t");
    o.idle(cx);
    for mode in [ThemeMode::Dark, ThemeMode::Light] {
        cx.update(|cx| dbento_app::theme::set_mode(mode, None, cx));
        for &id in IDS {
            assert!(present(cx, o.win, id), "{mode:?}: {id}");
        }
        for id in [
            connection_id("lite"),
            ("tab", 0usize).into(),
            ("tab-connection", 0usize).into(),
            ("result-col", 0usize).into(),
            ("statement-result", 0usize).into(),
        ] {
            assert!(present(cx, o.win, id.clone()), "{mode:?}: {id:?}");
        }
    }
}

#[gpui_kit::test]
fn the_welcome_view_renders_in_both_modes(cx: &mut TestAppContext) {
    support::init(cx);
    let files = tempfile::tempdir().unwrap();
    let (win, _) = cx
        .update(|cx| start(support::services(files.path()), None, cx))
        .unwrap();
    for mode in [ThemeMode::Dark, ThemeMode::Light] {
        cx.update(|cx| dbento_app::theme::set_mode(mode, None, cx));
        assert!(present(cx, win, "welcome-open"), "{mode:?}");
    }
}
