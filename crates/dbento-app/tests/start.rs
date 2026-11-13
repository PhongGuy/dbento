use crate::support;
use dbento_app::app::{MIN_WINDOW, start, window_options, window_options_for};
use gpui_kit::TestAppContext;
use gpui_kit::component::Theme;
#[gpui_kit::test]
fn start_opens_a_themed_window_with_the_shell(cx: &mut TestAppContext) {
    support::init(cx);
    let files = tempfile::tempdir().unwrap();
    let (win, shell) = cx
        .update(|cx| start(support::services(files.path()), None, cx))
        .unwrap();
    assert!(support::present(cx, win, "shell"));
    assert_eq!(
        shell.read_with(cx, |s, _| s.requested().map(|p| p.to_path_buf())),
        None
    );
    cx.update(|cx| {
        let t = Theme::global(cx);
        assert_eq!(t.dark_theme.name.as_ref(), dbento_app::theme::DARK);
        assert_eq!(t.light_theme.name.as_ref(), dbento_app::theme::LIGHT);
        assert_eq!(t.font_family.as_ref(), "Inter");
        assert_eq!(t.mono_font_family.as_ref(), "JetBrains Mono");
    });
}
#[test]
fn the_window_is_titled_dbento() {
    let o = window_options();
    assert_eq!(
        o.titlebar.as_ref().and_then(|t| t.title.clone()),
        Some("Dbento".into())
    );
    assert_eq!(o.app_id.as_deref(), Some("dbento-app"));
}
#[test]
fn linux_windows_draw_their_own_title_bar() {
    // The kit title bar is the only one: a compositor bar above it would double it.
    let expected = cfg!(target_os = "linux").then_some(gpui_kit::WindowDecorations::Client);
    assert_eq!(window_options().window_decorations, expected);
}
#[test]
fn the_window_cannot_be_resized_below_its_minimum() {
    let min = Some(gpui_kit::size(
        gpui_kit::px(MIN_WINDOW.0),
        gpui_kit::px(MIN_WINDOW.1),
    ));
    assert_eq!(window_options().window_min_size, min);
    let saved = dbento_app::app::WindowState {
        x: 0.,
        y: 0.,
        width: 900.,
        height: 600.,
        maximized: false,
    };
    assert_eq!(window_options_for(Some(&saved)).window_min_size, min);
}
