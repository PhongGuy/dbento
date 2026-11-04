//! Several query tabs: open-or-activate, new and close, and no edited SQL lost on the way.
mod support;
use dbento_core::{RelationKind, RelationRef};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, ElementId, TestAppContext, VisualTestContext, px, size};
use support::{Opened, label_of, opened, present, wait_real};

fn click(cx: &mut TestAppContext, o: &Opened, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(o.win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}
fn key(cx: &mut TestAppContext, o: &Opened, k: &str) {
    cx.update_window(o.win, |_, w, cx| w.press(k, cx)).unwrap();
    cx.run_until_parked();
}
fn count(cx: &mut TestAppContext, o: &Opened) -> usize {
    o.shell.read_with(cx, |s, _| s.tabs().len())
}
fn active(cx: &mut TestAppContext, o: &Opened) -> Option<usize> {
    o.shell.read_with(cx, |s, _| s.active())
}
fn set(cx: &mut TestAppContext, o: &Opened, ix: usize, sql: &str) {
    let tab = o.shell.read_with(cx, |s, _| s.tabs()[ix].clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql(sql, w, cx))
    })
    .unwrap();
}
fn sql_of(cx: &mut TestAppContext, o: &Opened, ix: usize) -> String {
    o.shell.read_with(cx, |s, cx| s.tabs()[ix].read(cx).sql(cx))
}
fn selected(cx: &mut TestAppContext, o: &Opened) -> Option<usize> {
    let sidebar = o.shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    sidebar.read_with(cx, |s, _| s.selected())
}

#[gpui_kit::test]
fn another_connection_opens_a_second_tab_and_keeps_the_first(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    set(cx, &o, 0, "SELECT 42");
    support::open_connection_row(cx, o.win, "nopg");
    assert!(!present(cx, o.win, "confirm-dialog"));
    assert_eq!(count(cx, &o), 2);
    assert_eq!(
        label_of(cx, o.win, ("tab-connection", 0usize)).as_deref(),
        Some("lite")
    );
    assert_eq!(
        label_of(cx, o.win, ("tab-connection", 1usize)).as_deref(),
        Some("nopg")
    );
    assert_eq!((active(cx, &o), selected(cx, &o)), (Some(1), Some(1)));
    assert_eq!(sql_of(cx, &o, 0), "SELECT 42");
}

#[gpui_kit::test]
fn a_connection_or_tab_click_activates_its_tab(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    support::open_connection_row(cx, o.win, "nopg");
    support::open_connection_row(cx, o.win, "lite");
    assert_eq!(
        (count(cx, &o), active(cx, &o), selected(cx, &o)),
        (2, Some(0), Some(0))
    );
    click(cx, &o, ("tab", 1usize));
    assert_eq!((active(cx, &o), selected(cx, &o)), (Some(1), Some(1)));
}

#[gpui_kit::test]
fn tab_new_and_ctrl_t_open_on_the_active_connection(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click(cx, &o, "tab-new");
    assert_eq!((count(cx, &o), active(cx, &o)), (2, Some(1)));
    assert_eq!(
        label_of(cx, o.win, ("tab-connection", 1usize)).as_deref(),
        Some("lite")
    );
    key(cx, &o, "ctrl-t");
    assert_eq!((count(cx, &o), active(cx, &o)), (3, Some(2)));
    assert_eq!(
        label_of(cx, o.win, ("tab-connection", 2usize)).as_deref(),
        Some("lite")
    );
}

#[gpui_kit::test]
fn ctrl_w_closes_an_unedited_tab_at_once_and_ends_its_session(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let events = o.model(cx).read_with(cx, |m, _| m.events().unwrap());
    key(cx, &o, "ctrl-w");
    assert!(!present(cx, o.win, "confirm-dialog"));
    assert_eq!((count(cx, &o), active(cx, &o)), (0, None));
    wait_real(cx, "the runner to end", |_| events.is_closed());
}

#[gpui_kit::test]
fn closing_an_edited_tab_asks_and_keep_keeps_it(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click(cx, &o, "tab-new");
    set(cx, &o, 0, "SELECT 42");
    click(cx, &o, ("tab-close", 0usize));
    wait_real(cx, "the dialog", |cx| present(cx, o.win, "confirm-dialog"));
    click(cx, &o, "discard-keep");
    assert_eq!(
        (count(cx, &o), sql_of(cx, &o, 0)),
        (2, "SELECT 42".to_owned())
    );
    click(cx, &o, ("tab-close", 0usize));
    wait_real(cx, "the dialog", |cx| present(cx, o.win, "confirm-dialog"));
    click(cx, &o, "discard-close");
    assert_eq!((count(cx, &o), active(cx, &o)), (1, Some(0)));
    assert_eq!(sql_of(cx, &o, 0), "");
}

#[gpui_kit::test]
fn a_second_close_request_while_the_question_is_open_asks_nothing_more(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click(cx, &o, "tab-new");
    set(cx, &o, 0, "SELECT 42");
    let shell = o.shell.clone();
    for _ in 0..2 {
        cx.update_window(o.win, |_, w, cx| {
            shell.update(cx, |s, cx| s.close_tab_at(0, w, cx))
        })
        .unwrap();
        cx.run_until_parked();
    }
    wait_real(cx, "the dialog", |cx| present(cx, o.win, "confirm-dialog"));
    click(cx, &o, "discard-keep");
    assert!(
        !present(cx, o.win, "confirm-dialog"),
        "one answer clears the only question"
    );
    assert_eq!(count(cx, &o), 2);
    // The question answered, the next request asks again.
    click(cx, &o, ("tab-close", 0usize));
    wait_real(cx, "the dialog", |cx| present(cx, o.win, "confirm-dialog"));
    click(cx, &o, "discard-close");
    assert_eq!(count(cx, &o), 1);
}

#[gpui_kit::test]
fn a_window_close_while_a_tab_question_is_open_waits_for_its_answer(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click(cx, &o, "tab-new");
    set(cx, &o, 0, "SELECT 42");
    click(cx, &o, ("tab-close", 0usize));
    wait_real(cx, "the dialog", |cx| present(cx, o.win, "confirm-dialog"));
    let mut v = VisualTestContext::from_window(o.win, cx);
    assert!(!v.simulate_close(), "the open question holds the window");
    click(cx, &o, "discard-keep");
    assert!(!present(cx, o.win, "confirm-dialog"));
    assert_eq!(count(cx, &o), 2);
}

#[gpui_kit::test]
fn a_middle_click_closes_an_unedited_tab_active_or_not(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click(cx, &o, "tab-new");
    click(cx, &o, "tab-new");
    assert_eq!((count(cx, &o), active(cx, &o)), (3, Some(2)));
    support::middle_click(cx, o.win, ("tab", 0usize));
    assert!(!present(cx, o.win, "confirm-dialog"));
    assert_eq!((count(cx, &o), active(cx, &o)), (2, Some(1)));
    support::middle_click(cx, o.win, ("tab", 1usize));
    assert!(!present(cx, o.win, "confirm-dialog"));
    assert_eq!((count(cx, &o), active(cx, &o)), (1, Some(0)));
}

#[gpui_kit::test]
fn a_middle_click_on_an_edited_tab_asks_and_keep_keeps_it(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click(cx, &o, "tab-new");
    set(cx, &o, 0, "SELECT 42");
    support::middle_click(cx, o.win, ("tab", 0usize));
    wait_real(cx, "the dialog", |cx| present(cx, o.win, "confirm-dialog"));
    click(cx, &o, "discard-keep");
    assert_eq!(
        (count(cx, &o), active(cx, &o), sql_of(cx, &o, 0)),
        (2, Some(1), "SELECT 42".to_owned())
    );
}

#[gpui_kit::test]
fn closing_the_window_closes_every_tab(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click(cx, &o, "tab-new");
    let events: Vec<_> = o.shell.read_with(cx, |s, cx| {
        s.tabs()
            .iter()
            .map(|t| t.read(cx).model().read(cx).events().unwrap())
            .collect()
    });
    let mut v = VisualTestContext::from_window(o.win, cx);
    assert!(
        !v.simulate_close(),
        "the first close waits for the sessions"
    );
    wait_real(cx, "every runner to end", |_| {
        events.iter().all(|e| e.is_closed())
    });
}

#[gpui_kit::test]
fn untitled_tabs_count_up_and_name_their_connection_on_the_kind_icon(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click(cx, &o, "tab-new");
    key(cx, &o, "ctrl-w"); // drops the unedited "Query 2"; numbers are never reused
    support::open_connection_row(cx, o.win, "nopg");
    assert_eq!(count(cx, &o), 2);
    let labels: Vec<_> = (0..2usize)
        .map(|i| {
            (
                label_of(cx, o.win, ("tab", i)),
                label_of(cx, o.win, ("tab-connection", i)),
            )
        })
        .collect();
    assert_eq!(
        labels,
        [
            (Some("Query 1".to_owned()), Some("lite".to_owned())),
            (Some("Query 3".to_owned()), Some("nopg".to_owned())),
        ]
    );
}

#[gpui_kit::test]
fn the_tab_strip_is_36_px(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let h = cx
        .update_window(o.win, |_, w, cx| {
            w.render_frame(cx);
            w.find("tab-bar").bounds().size.height
        })
        .unwrap();
    assert_eq!(h, px(36.));
}

#[gpui_kit::test]
fn a_table_tab_is_named_after_its_relation_and_opens_mostly_results(cx: &mut TestAppContext) {
    let o = opened(
        cx,
        "CREATE TABLE Album(AlbumId INTEGER PRIMARY KEY, Title TEXT);",
        "",
        0,
    );
    cx.simulate_window_resize(o.win, size(px(1600.), px(900.)));
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| {
            s.open_relation(
                0,
                &RelationRef::new("main", "Album"),
                RelationKind::Table,
                w,
                cx,
            )
        })
    })
    .unwrap();
    cx.run_until_parked();
    o.idle(cx);
    assert_eq!(count(cx, &o), 2);
    assert_eq!(
        label_of(cx, o.win, ("tab", 1usize)).as_deref(),
        Some("main.Album")
    );
    assert_eq!(
        label_of(cx, o.win, ("tab-connection", 1usize)).as_deref(),
        Some("lite")
    );
    let (editor, footer) = cx
        .update_window(o.win, |_, w, cx| {
            w.render_frame(cx);
            (w.find("editor").bounds(), w.find("result-footer").bounds())
        })
        .unwrap();
    let share = f32::from(editor.size.height) / f32::from(footer.bottom() - editor.top());
    assert!((0.17..=0.23).contains(&share), "editor share {share}");
}
