//! The no-tab start column and the welcome column.
use crate::support;
use crate::support::{Opened, click_id, label_of, opened, present, wait_real};
use dbento_app::app::start;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, Bounds, Pixels, TestAppContext, px, size};

fn no_tab(cx: &mut TestAppContext) -> Opened {
    let o = opened(cx, "", "", 0);
    cx.update_window(o.win, |_, w, cx| w.press("ctrl-w", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(
        o.shell.read_with(cx, |s, _| s.tab().is_none()),
        "no tab is open at start"
    );
    o
}

fn centre(b: Bounds<Pixels>) -> f32 {
    f32::from(b.left() + b.right()) / 2.
}

// No box: the heading is the column's first thing (no icon or padding above it) and the rows
// span the column edge to edge (no border or padding beside them).
#[gpui_kit::test]
fn with_no_tab_the_start_actions_sit_unboxed_and_centred(cx: &mut TestAppContext) {
    let o = no_tab(cx);
    cx.simulate_window_resize(o.win, size(px(1600.), px(900.)));
    let (col, title, first, area) = cx
        .update_window(o.win, |_, w, cx| {
            w.render_frame(cx);
            (
                w.find("start-card").bounds(),
                w.find("start-title").bounds(),
                w.find(("start-action", 0usize)).bounds(),
                w.find("tab-area").bounds(),
            )
        })
        .unwrap();
    assert_eq!(col.size.width, px(360.));
    assert_eq!(title.top(), col.top(), "nothing sits above the heading");
    assert_eq!(
        (first.left(), first.right()),
        (col.left(), col.right()),
        "no border or padding beside the rows"
    );
    assert!(
        (centre(col) - centre(area)).abs() < 1.,
        "centred in the tab area"
    );
    assert_eq!(
        label_of(cx, o.win, "start-title").as_deref(),
        Some("No tab open")
    );
    let labels: Vec<_> = (0usize..)
        .map_while(|i| label_of(cx, o.win, ("start-action", i)))
        .collect();
    assert_eq!(
        labels,
        ["New tab", "Open query", "New connection", "History"]
    );
}
#[gpui_kit::test]
fn new_tab_opens_on_the_selected_connection_and_the_card_goes(cx: &mut TestAppContext) {
    let o = no_tab(cx);
    click_id(cx, o.win, ("start-action", 0usize));
    assert_eq!(o.shell.read_with(cx, |s, _| s.tabs().len()), 1);
    assert!(!present(cx, o.win, "start-card"));
}
#[gpui_kit::test]
fn open_query_opens_the_queries_mode(cx: &mut TestAppContext) {
    let o = no_tab(cx);
    click_id(cx, o.win, ("start-action", 1usize));
    wait_real(cx, "the palette", |cx| present(cx, o.win, "palette-empty"));
}
#[gpui_kit::test]
fn new_connection_opens_the_dialog(cx: &mut TestAppContext) {
    let o = no_tab(cx);
    click_id(cx, o.win, ("start-action", 2usize));
    wait_real(cx, "the dialog", |cx| present(cx, o.win, "conn-name"));
}
#[gpui_kit::test]
fn history_opens_the_panel(cx: &mut TestAppContext) {
    let o = no_tab(cx);
    click_id(cx, o.win, ("start-action", 3usize));
    assert!(o.shell.read_with(cx, |s, _| s.history_open()));
    assert!(present(cx, o.win, "history-panel"));
}
#[gpui_kit::test]
fn the_welcome_view_sits_unboxed_in_the_same_column(cx: &mut TestAppContext) {
    support::init(cx);
    let files = tempfile::tempdir().unwrap();
    let (win, _) = cx
        .update(|cx| start(support::services(files.path()), None, cx))
        .unwrap();
    cx.simulate_window_resize(win, size(px(1600.), px(900.)));
    let (col, title, open) = cx
        .update_window(win, |_, w, cx| {
            w.render_frame(cx);
            (
                w.find("welcome-card").bounds(),
                w.find("welcome-title").bounds(),
                w.find("welcome-open").bounds(),
            )
        })
        .unwrap();
    assert_eq!(col.size.width, px(360.));
    assert_eq!(title.top(), col.top(), "nothing sits above the heading");
    assert_eq!(
        (open.left(), open.right()),
        (col.left(), col.right()),
        "the button spans the column: no border or padding"
    );
    assert!((centre(open) - 800.).abs() < 1., "{}", centre(open));
    assert_eq!(
        label_of(cx, win, "welcome-title").as_deref(),
        Some("Open a workspace")
    );
}
