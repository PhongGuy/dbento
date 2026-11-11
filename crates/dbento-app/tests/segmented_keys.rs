//! Segmented options driven by keys: Tab reaches the row, the arrows step the choice
//! without wrapping, and Home and End jump to its ends.
use crate::support;

use crate::support::{Opened, click_id, form, opened};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, ElementId, TestAppContext};

fn press(cx: &mut TestAppContext, win: AnyWindowHandle, key: &str) {
    cx.update_window(win, |_, w, cx| w.press(key, cx)).unwrap();
    cx.run_until_parked();
}

fn focused(cx: &mut TestAppContext, win: AnyWindowHandle, id: &'static str) -> Option<bool> {
    let id: ElementId = id.into();
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        w.find(id).focused()
    })
    .unwrap()
}

/// The index of the option `group` shows selected.
fn chosen(
    cx: &mut TestAppContext,
    win: AnyWindowHandle,
    group: &'static str,
    options: usize,
) -> usize {
    let group: ElementId = group.into();
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        let within = w.within(group.clone());
        let on: Vec<usize> = (0..options)
            .filter(|&ix| within.find(ix).selected() == Some(true))
            .collect();
        assert_eq!(on.len(), 1, "exactly one option is selected: {on:?}");
        on[0]
    })
    .unwrap()
}

fn new_form(cx: &mut TestAppContext) -> Opened {
    let o = opened(cx, "", "", 0);
    click_id(cx, o.win, "connection-new");
    form(cx, &o);
    o
}

#[gpui_kit::test]
fn tab_from_the_name_lands_on_the_engine_row_and_the_arrows_pick(cx: &mut TestAppContext) {
    let o = new_form(cx);
    click_id(cx, o.win, "conn-name");
    assert_eq!(focused(cx, o.win, "conn-engine"), Some(false));
    press(cx, o.win, "tab");
    assert_eq!(
        focused(cx, o.win, "conn-engine"),
        Some(true),
        "Tab skips the engine row"
    );
    assert_eq!(chosen(cx, o.win, "conn-engine", 9), 0);
    press(cx, o.win, "right");
    assert_eq!(chosen(cx, o.win, "conn-engine", 9), 1, "Right picks SQLite");
    assert!(
        support::present(cx, o.win, "conn-path"),
        "the SQLite form shows"
    );
    press(cx, o.win, "right");
    assert_eq!(chosen(cx, o.win, "conn-engine", 9), 2, "Right picks MySQL");
    press(cx, o.win, "right");
    assert_eq!(chosen(cx, o.win, "conn-engine", 9), 3, "Right picks DuckDB");
    assert!(
        support::present(cx, o.win, "conn-path"),
        "the DuckDB form shows"
    );
    press(cx, o.win, "right");
    assert_eq!(
        chosen(cx, o.win, "conn-engine", 9),
        4,
        "Right picks SQL Server"
    );
    assert!(
        support::present(cx, o.win, "conn-read-only-user"),
        "the SQL Server form shows"
    );
    press(cx, o.win, "right");
    assert_eq!(
        chosen(cx, o.win, "conn-engine", 9),
        5,
        "Right picks SurrealDB"
    );
    assert!(
        support::present(cx, o.win, "conn-namespace"),
        "the SurrealDB form shows"
    );
    press(cx, o.win, "right");
    assert_eq!(
        chosen(cx, o.win, "conn-engine", 9),
        6,
        "Right picks MongoDB"
    );
    assert!(
        support::present(cx, o.win, "conn-uri"),
        "the MongoDB form shows"
    );
    press(cx, o.win, "right");
    assert_eq!(chosen(cx, o.win, "conn-engine", 9), 7, "Right picks Redis");
    assert!(
        support::present(cx, o.win, "conn-database") && !support::present(cx, o.win, "conn-uri"),
        "the Redis form shows"
    );
    press(cx, o.win, "right");
    assert_eq!(
        chosen(cx, o.win, "conn-engine", 9),
        8,
        "Right picks DynamoDB"
    );
    assert!(
        support::present(cx, o.win, "conn-region"),
        "the DynamoDB form shows"
    );
    press(cx, o.win, "right");
    assert_eq!(
        chosen(cx, o.win, "conn-engine", 9),
        8,
        "Right stops at the end"
    );
    for want in [7, 6, 5, 4, 3, 2, 1, 0] {
        press(cx, o.win, "left");
        assert_eq!(chosen(cx, o.win, "conn-engine", 9), want, "Left steps back");
    }
    press(cx, o.win, "left");
    assert_eq!(
        chosen(cx, o.win, "conn-engine", 9),
        0,
        "Left stops at the start"
    );
    assert_eq!(
        focused(cx, o.win, "conn-engine"),
        Some(true),
        "the row keeps focus as the form changes under it"
    );
}

#[gpui_kit::test]
fn home_end_up_and_down_move_a_three_option_row(cx: &mut TestAppContext) {
    let o = new_form(cx);
    cx.update_window(o.win, |_, w, cx| w.within("conn-tls").click(0usize, cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(
        focused(cx, o.win, "conn-tls"),
        Some(true),
        "a click focuses the row"
    );
    press(cx, o.win, "end");
    assert_eq!(chosen(cx, o.win, "conn-tls", 3), 2, "End picks Off");
    press(cx, o.win, "up");
    assert_eq!(chosen(cx, o.win, "conn-tls", 3), 1, "Up steps back");
    press(cx, o.win, "home");
    assert_eq!(chosen(cx, o.win, "conn-tls", 3), 0, "Home picks Default");
    press(cx, o.win, "down");
    assert_eq!(chosen(cx, o.win, "conn-tls", 3), 1, "Down steps on");
}
