//! Tab switching in recent order, jumping by number, and reordering by keys or drag.
use crate::support;
use crate::support::{Opened, opened, present};
use dbento_app::app::start;
use dbento_app::drafts::DRAFT_DEBOUNCE;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AppContext as _, ElementId, EntityId, Focusable as _, InputEvent as _, Modifiers,
    ModifiersChangedEvent, TestAppContext,
};

fn click(cx: &mut TestAppContext, o: &Opened, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(o.win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}
fn key(cx: &mut TestAppContext, o: &Opened, k: &str) {
    cx.update_window(o.win, |_, w, cx| w.press(k, cx)).unwrap();
    cx.run_until_parked();
}
/// The platform's report that every modifier is up, as when Ctrl is let go.
fn release(cx: &mut TestAppContext, o: &Opened) {
    cx.update_window(o.win, |_, w, cx| {
        let up = ModifiersChangedEvent {
            modifiers: Modifiers::none(),
            ..Default::default()
        };
        w.dispatch_event(up.to_platform_input(), cx);
    })
    .unwrap();
    cx.run_until_parked();
}
fn active(cx: &mut TestAppContext, o: &Opened) -> Option<usize> {
    o.shell.read_with(cx, |s, _| s.active())
}
fn ids(cx: &mut TestAppContext, o: &Opened) -> Vec<EntityId> {
    o.shell
        .read_with(cx, |s, _| s.tabs().iter().map(|t| t.entity_id()).collect())
}
fn set(cx: &mut TestAppContext, o: &Opened, ix: usize, sql: &str) {
    let tab = o.shell.read_with(cx, |s, _| s.tabs()[ix].clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql(sql, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
}
/// Three tabs on the fixture's first connection, opened left to right; the last is active.
fn three(cx: &mut TestAppContext) -> Opened {
    let o = opened(cx, "", "", 0);
    click(cx, &o, "tab-new");
    click(cx, &o, "tab-new");
    assert_eq!(active(cx, &o), Some(2), "the third tab is active");
    o
}
fn editor_focused(cx: &mut TestAppContext, o: &Opened) -> bool {
    let editor = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone());
    cx.update_window(o.win, |_, w, cx| {
        editor.read(cx).focus_handle(cx).is_focused(w)
    })
    .unwrap()
}

#[gpui_kit::test]
fn ctrl_tab_walks_back_in_recent_order_and_commits_on_release(cx: &mut TestAppContext) {
    let o = three(cx);
    // Recent order is now 0, 2, 1.
    click(cx, &o, ("tab", 0usize));
    key(cx, &o, "ctrl-tab");
    assert_eq!(
        active(cx, &o),
        Some(2),
        "the first press goes to the previous tab"
    );
    key(cx, &o, "ctrl-tab");
    assert_eq!(
        active(cx, &o),
        Some(1),
        "a second press with Ctrl held walks further"
    );
    release(cx, &o);
    // Committed: recent order is 1, 0, 2.
    key(cx, &o, "ctrl-tab");
    release(cx, &o);
    assert_eq!(active(cx, &o), Some(0));
    key(cx, &o, "ctrl-tab");
    release(cx, &o);
    assert_eq!(
        active(cx, &o),
        Some(1),
        "a single switch toggles the last two"
    );
}

#[gpui_kit::test]
fn ctrl_shift_tab_walks_forward_and_starts_at_the_oldest(cx: &mut TestAppContext) {
    let o = three(cx);
    // Recent order is 2, 1, 0.
    key(cx, &o, "ctrl-tab");
    key(cx, &o, "ctrl-tab");
    assert_eq!(active(cx, &o), Some(0));
    key(cx, &o, "ctrl-shift-tab");
    assert_eq!(
        active(cx, &o),
        Some(1),
        "Shift steps the walk back toward the newest"
    );
    release(cx, &o);
    // Committed: recent order is 1, 2, 0; a fresh reversed walk starts at the oldest.
    key(cx, &o, "ctrl-shift-tab");
    release(cx, &o);
    assert_eq!(active(cx, &o), Some(0));
}

#[gpui_kit::test]
fn another_key_ends_a_walk_as_releasing_ctrl_does(cx: &mut TestAppContext) {
    let o = three(cx);
    key(cx, &o, "ctrl-tab");
    key(cx, &o, "ctrl-tab");
    assert_eq!(active(cx, &o), Some(0));
    key(cx, &o, "escape");
    // Recent order is 0, 2, 1.
    key(cx, &o, "ctrl-tab");
    assert_eq!(active(cx, &o), Some(2));
}

#[gpui_kit::test]
fn ctrl_digits_jump_to_a_tab_and_ctrl_9_to_the_last(cx: &mut TestAppContext) {
    let o = three(cx);
    key(cx, &o, "ctrl-1");
    assert_eq!(active(cx, &o), Some(0));
    key(cx, &o, "ctrl-2");
    assert_eq!(active(cx, &o), Some(1));
    key(cx, &o, "ctrl-9");
    assert_eq!(active(cx, &o), Some(2));
    key(cx, &o, "ctrl-1");
    key(cx, &o, "ctrl-5");
    assert_eq!(
        active(cx, &o),
        Some(0),
        "a digit past the last tab does nothing"
    );
    // A jump counts as an activation, so Ctrl+Tab returns to the tab before it.
    key(cx, &o, "ctrl-tab");
    release(cx, &o);
    assert_eq!(active(cx, &o), Some(2));
}

#[gpui_kit::test]
fn ctrl_shift_page_keys_move_the_active_tab_and_stop_at_the_ends(cx: &mut TestAppContext) {
    let o = three(cx);
    let start = ids(cx, &o);
    key(cx, &o, "ctrl-shift-pagedown");
    assert_eq!(ids(cx, &o), start, "the last tab cannot move right");
    key(cx, &o, "ctrl-shift-pageup");
    assert_eq!(ids(cx, &o), [start[0], start[2], start[1]]);
    assert_eq!(active(cx, &o), Some(1), "the moved tab stays active");
    key(cx, &o, "ctrl-shift-pageup");
    key(cx, &o, "ctrl-shift-pageup");
    assert_eq!(ids(cx, &o), [start[2], start[0], start[1]]);
    assert_eq!(active(cx, &o), Some(0));
    key(cx, &o, "ctrl-shift-pagedown");
    assert_eq!(ids(cx, &o), [start[0], start[2], start[1]]);
    assert_eq!(active(cx, &o), Some(1));
}

#[gpui_kit::test]
fn dragging_a_tab_onto_another_moves_it_there(cx: &mut TestAppContext) {
    let o = three(cx);
    let start = ids(cx, &o);
    let from: ElementId = ("tab", 0usize).into();
    let to: ElementId = ("tab", 2usize).into();
    cx.update_window(o.win, |_, w, cx| w.drag_to(from, to, cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(ids(cx, &o), [start[1], start[2], start[0]]);
    assert_eq!(active(cx, &o), Some(1), "the active tab keeps its focus");
}

#[gpui_kit::test]
fn the_new_order_survives_a_restart(cx: &mut TestAppContext) {
    let o = three(cx);
    for (ix, sql) in ["SELECT 1", "SELECT 2", "SELECT 3"].iter().enumerate() {
        set(cx, &o, ix, sql);
    }
    key(cx, &o, "ctrl-1");
    key(cx, &o, "ctrl-shift-pagedown");
    cx.executor().advance_clock(DRAFT_DEBOUNCE);
    cx.run_until_parked();
    let (win, shell) = cx
        .update(|cx| {
            start(
                support::services(o.files.path()),
                Some(o.fx.dir.path().into()),
                cx,
            )
        })
        .unwrap();
    assert!(present(cx, win, "connection-list"));
    let sql: Vec<String> = shell.read_with(cx, |s, cx| {
        s.tabs().iter().map(|t| t.read(cx).sql(cx)).collect()
    });
    assert_eq!(sql, ["SELECT 2", "SELECT 1", "SELECT 3"]);
    assert_eq!(shell.read_with(cx, |s, _| s.active()), Some(1));
}

#[gpui_kit::test]
fn the_tab_keys_work_while_the_editor_has_focus(cx: &mut TestAppContext) {
    let o = three(cx);
    assert!(editor_focused(cx, &o));
    let typed = |cx: &mut TestAppContext, o: &Opened| -> String {
        o.shell
            .read_with(cx, |s, cx| s.tab().unwrap().read(cx).sql(cx))
    };
    key(cx, &o, "ctrl-1");
    assert_eq!(active(cx, &o), Some(0));
    assert!(editor_focused(cx, &o));
    key(cx, &o, "ctrl-tab");
    release(cx, &o);
    assert_eq!(active(cx, &o), Some(2));
    assert!(editor_focused(cx, &o));
    key(cx, &o, "ctrl-shift-pageup");
    assert_eq!(active(cx, &o), Some(1));
    assert!(editor_focused(cx, &o));
    assert_eq!(typed(cx, &o), "", "no key typed into the editor");
}
