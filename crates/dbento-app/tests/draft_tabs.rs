use crate::support::{Opened, opened, present, wait_real};
use dbento_app::app::AppPaths;
use dbento_app::drafts::{DRAFT_DEBOUNCE, Draft, Drafts};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

fn drafts(o: &Opened) -> Vec<Draft> {
    Drafts::new(AppPaths::in_dir(o.files.path()).drafts).load(o.fx.ws.dir())
}
fn settle(cx: &mut TestAppContext) {
    cx.executor().advance_clock(DRAFT_DEBOUNCE);
    cx.run_until_parked();
}
fn set(cx: &mut TestAppContext, o: &Opened, sql: &str) {
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql(sql, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
}
fn draft_id(cx: &mut TestAppContext, o: &Opened) -> String {
    o.shell.read_with(cx, |s, cx| {
        s.tab().unwrap().read(cx).draft_id().unwrap().to_owned()
    })
}

#[gpui_kit::test]
fn edited_sql_becomes_a_draft_after_the_pause(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    set(cx, &o, "SELECT 42");
    assert!(drafts(&o).is_empty(), "nothing before the pause");
    settle(cx);
    wait_real(cx, "the draft", |_| drafts(&o).len() == 1);
    let d = &drafts(&o)[0];
    assert_eq!(
        (d.sql.as_str(), d.connection.as_str(), d.file.as_deref()),
        ("SELECT 42", "lite", None)
    );
    assert_eq!(d.id, draft_id(cx, &o));
}

#[gpui_kit::test]
fn typing_updates_the_draft(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let e = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone());
    cx.update_window(o.win, |_, w, cx| {
        e.update(cx, |e, cx| e.focus(w, cx));
        w.input("SELECT 7", cx);
    })
    .unwrap();
    settle(cx);
    wait_real(cx, "the draft", |_| {
        drafts(&o).first().is_some_and(|d| d.sql == "SELECT 7")
    });
}

#[gpui_kit::test]
fn returning_to_the_baseline_removes_the_draft(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    set(cx, &o, "SELECT 42");
    settle(cx);
    wait_real(cx, "the draft", |_| drafts(&o).len() == 1);
    set(cx, &o, "  ");
    settle(cx);
    wait_real(cx, "no draft", |_| drafts(&o).is_empty());
}

#[gpui_kit::test]
fn closing_an_edited_tab_with_close_deletes_its_draft(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    set(cx, &o, "SELECT 42");
    settle(cx);
    wait_real(cx, "the draft", |_| drafts(&o).len() == 1);
    cx.update_window(o.win, |_, w, cx| w.click(("tab-close", 0usize), cx))
        .unwrap();
    wait_real(cx, "the dialog", |cx| present(cx, o.win, "confirm-dialog"));
    cx.update_window(o.win, |_, w, cx| w.click("discard-close", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(drafts(&o).is_empty());
}

#[gpui_kit::test]
fn flush_draft_writes_and_removes_at_once(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    set(cx, &o, "SELECT 9");
    tab.update(cx, |t, cx| t.flush_draft(cx)).unwrap();
    assert_eq!(
        drafts(&o)
            .iter()
            .map(|d| d.sql.as_str())
            .collect::<Vec<_>>(),
        ["SELECT 9"]
    );
    set(cx, &o, "");
    tab.update(cx, |t, cx| t.flush_draft(cx)).unwrap();
    assert!(drafts(&o).is_empty());
    settle(cx);
    assert!(drafts(&o).is_empty(), "no pending write brings it back");
}
