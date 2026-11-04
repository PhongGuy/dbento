mod support;
use dbento_app::keymap::{PALETTE_KEYS, chip_keys};
use dbento_app::theme::Appearance;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Focusable as _, Pixels, TestAppContext, px, size,
};
use support::{click_id, label_of, opened, present, wait_real};

fn at(cx: &mut TestAppContext, win: AnyWindowHandle, id: &'static str) -> Bounds<Pixels> {
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        w.find(id).bounds()
    })
    .unwrap()
}

fn near(a: Pixels, b: f32, what: &str) {
    assert!((f32::from(a) - b).abs() < 0.5, "{what}: {a:?} vs {b}");
}

#[gpui_kit::test]
fn panels_float_on_the_frame_six_px_apart(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    cx.simulate_window_resize(o.win, size(px(1600.), px(900.)));
    let (rail, side, main, foot) = (
        at(cx, o.win, "rail-explorer"),
        at(cx, o.win, "sidebar"),
        at(cx, o.win, "tab-area"),
        at(cx, o.win, "status-bar"),
    );
    near(rail.left(), 6., "rail button centred in the 44 px rail");
    near(rail.size.width, 32., "rail button width");
    near(side.left(), 50., "sidebar 6 px right of the rail");
    near(side.top(), 46., "sidebar 6 px under the 40 px title bar");
    near(
        main.left() - side.right(),
        6.,
        "gutter between sidebar and main",
    );
    near(main.top(), 46., "main panel top");
    near(main.right(), 1594., "main panel 6 px from the right edge");
    near(
        foot.top() - main.bottom(),
        6.,
        "gutter above the status bar",
    );
    near(
        side.bottom(),
        f32::from(main.bottom()),
        "panels share a bottom",
    );
}

#[gpui_kit::test]
fn the_title_pill_is_centred_and_opens_the_palette(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    cx.simulate_window_resize(o.win, size(px(1600.), px(900.)));
    let pill = at(cx, o.win, "title-search");
    near(pill.size.width, 420., "pill width");
    near(pill.size.height, 28., "pill height");
    near((pill.left() + pill.right()) / 2., 800., "pill centre");
    assert_eq!(
        label_of(cx, o.win, "title-search").as_deref(),
        Some("Search tables, queries and commands")
    );
    assert_eq!(
        label_of(cx, o.win, "title-search-keys"),
        Some(chip_keys(PALETTE_KEYS))
    );
    click_id(cx, o.win, "title-search");
    wait_real(cx, "the palette", |cx| present(cx, o.win, "palette"));
    assert!(
        !present(cx, o.win, "query-open"),
        "the tab-bar folder button is gone"
    );
}

#[gpui_kit::test]
fn the_queries_rail_button_opens_the_queries_mode(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click_id(cx, o.win, "rail-queries");
    wait_real(cx, "the palette", |cx| present(cx, o.win, "palette-empty"));
}

#[gpui_kit::test]
fn the_explorer_rail_button_focuses_the_filter(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click_id(cx, o.win, "rail-explorer");
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        let input = shell
            .read(cx)
            .sidebar()
            .unwrap()
            .read(cx)
            .filter_state()
            .clone();
        assert!(input.read(cx).focus_handle(cx).is_focused(w));
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_appearance_rail_button_cycles_the_choice(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let before: Appearance = o.shell.read_with(cx, |s, _| s.appearance());
    click_id(cx, o.win, "rail-appearance");
    assert_eq!(o.shell.read_with(cx, |s, _| s.appearance()), before.next());
}

#[gpui_kit::test]
fn history_toggle_lives_on_the_rail_and_still_toggles(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let (toggle, rail) = (
        at(cx, o.win, "history-toggle"),
        at(cx, o.win, "rail-explorer"),
    );
    near(toggle.left(), f32::from(rail.left()), "same rail column");
    click_id(cx, o.win, "history-toggle");
    assert!(o.shell.read_with(cx, |s, _| s.history_open()));
    assert!(present(cx, o.win, "history-panel"));
}
