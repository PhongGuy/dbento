//! Back and Forward walk a preview tab's hops: the pure history, the inspector's buttons and
//! Alt+Left/Right from the grid and the inspector, never from an input.
use crate::support::{Opened, click_id, launched, present, wait_real};
use dbento_app::nav::{Hop, NavHistory};
use dbento_core::{RelationKind, RelationRef};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, Focusable as _, TestAppContext};

const T: &str = "CREATE TABLE a(id INTEGER PRIMARY KEY, name TEXT); \
                 CREATE TABLE b(id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a(id), note TEXT); \
                 INSERT INTO a VALUES (1, 'x'), (2, 'y'); \
                 INSERT INTO b VALUES (1, 1, 'n'), (2, NULL, 'm'), (3, 1, 'o');";
const A1: &str = "SELECT *\nFROM main.a\nWHERE (\nid = '1'\n)\nORDER BY id ASC";
const B_A1: &str = "SELECT *\nFROM main.b\nWHERE (\na_id = '1'\n)\nORDER BY id ASC";

fn hop(f: &str) -> Hop {
    Hop::new(RelationRef::new("main", "a"), f)
}
fn follow(cx: &mut TestAppContext, o: &Opened, rel: &str, filter: &str, sql: &str) {
    let (shell, rel, filter) = (
        o.shell.clone(),
        RelationRef::new("main", rel),
        filter.to_owned(),
    );
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.open_filtered("lite", &rel, filter, w, cx))
    })
    .unwrap();
    o.wait_browse(cx, sql);
}
// b, then a follow to a (one row) and one from that preview to b (two rows).
fn walked(cx: &mut TestAppContext) -> Opened {
    let o = launched(cx, T, "");
    o.open_relation(cx, "b", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.b\nORDER BY id ASC");
    follow(cx, &o, "a", "id = '1'", A1);
    follow(cx, &o, "b", "a_id = '1'", B_A1);
    o
}
// gpui-pre 0.3.8 writes no accesskit disabled flag for the kit's Button, so the button is
// found shown and its state read from the inspector that draws it.
fn disabled(cx: &mut TestAppContext, o: &Opened, id: &'static str) -> bool {
    cx.run_until_parked();
    assert!(present(cx, o.win, id), "{id} is shown");
    let inspector = o.shell.read_with(cx, |s, _| s.inspector().clone());
    let (back, forward) = inspector.read_with(cx, |i, cx| i.nav_live(cx));
    !if id == "inspector-back" {
        back
    } else {
        forward
    }
}
fn press(cx: &mut TestAppContext, o: &Opened, k: &str) {
    cx.update_window(o.win, |_, w, cx| w.press(k, cx)).unwrap();
    cx.run_until_parked();
}
fn browse_sql(cx: &mut TestAppContext, o: &Opened) -> String {
    let m = o.model(cx);
    wait_real(cx, "an idle tab", |cx| m.read_with(cx, |m, _| !m.running()));
    m.read_with(cx, |m, _| {
        m.browse().map(|b| b.sql().to_owned()).unwrap_or_default()
    })
}

#[test]
fn back_and_forward_walk_the_hops_and_a_new_hop_drops_the_forward_ones() {
    let mut h = NavHistory::default();
    assert!(!h.can_back() && !h.can_forward());
    h.push(hop("id = '1'"));
    assert!(
        !h.can_back(),
        "the first hop has nothing behind it in this tab"
    );
    h.push(hop("id = '2'"));
    h.push(hop("id = '3'"));
    assert_eq!(h.back(), Some(hop("id = '2'")));
    assert_eq!(h.back(), Some(hop("id = '1'")));
    assert_eq!(h.back(), None);
    assert_eq!(h.current(), Some(&hop("id = '1'")));
    assert_eq!(h.forward(), Some(hop("id = '2'")));
    assert!(h.can_forward());
    h.push(hop("id = '9'"));
    assert!(!h.can_forward());
    assert_eq!(h.back(), Some(hop("id = '2'")));
    h.clear();
    assert!(h.current().is_none() && !h.can_back() && !h.can_forward());
}

#[gpui_kit::test]
fn the_inspector_buttons_walk_the_preview_tabs_history(cx: &mut TestAppContext) {
    let o = walked(cx);
    click_id(cx, o.win, "inspector-toggle");
    assert!(!disabled(cx, &o, "inspector-back"));
    assert!(disabled(cx, &o, "inspector-forward"));
    click_id(cx, o.win, "inspector-back");
    o.wait_browse(cx, A1);
    assert!(disabled(cx, &o, "inspector-back"));
    assert!(!disabled(cx, &o, "inspector-forward"));
    click_id(cx, o.win, "inspector-forward");
    o.wait_browse(cx, B_A1);
    assert_eq!(o.shell.read_with(cx, |s, _| s.tabs().len()), 2);
}

#[gpui_kit::test]
fn a_disabled_inspector_button_takes_no_step(cx: &mut TestAppContext) {
    let o = walked(cx);
    click_id(cx, o.win, "inspector-toggle");
    assert!(disabled(cx, &o, "inspector-forward"));
    click_id(cx, o.win, "inspector-forward");
    assert_eq!(browse_sql(cx, &o), B_A1, "no forward hop to take");
    click_id(cx, o.win, "inspector-back");
    o.wait_browse(cx, A1);
    assert!(disabled(cx, &o, "inspector-back"));
    click_id(cx, o.win, "inspector-back");
    assert_eq!(browse_sql(cx, &o), A1, "no back hop to take");
    assert!(
        !disabled(cx, &o, "inspector-forward"),
        "the forward hop stays"
    );
}

#[gpui_kit::test]
fn alt_arrows_walk_from_the_grid_and_the_inspector_but_not_from_inputs(cx: &mut TestAppContext) {
    let o = walked(cx);
    click_id(cx, o.win, "inspector-toggle");
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    let editor = tab.read_with(cx, |t, _| t.editor().clone());
    cx.update_window(o.win, |_, w, cx| editor.update(cx, |e, cx| e.focus(w, cx)))
        .unwrap();
    press(cx, &o, "alt-left");
    assert_eq!(browse_sql(cx, &o), B_A1, "the editor keeps Alt+Left");
    let filter = tab.read_with(cx, |t, cx| t.results_pane().read(cx).filter_input().clone());
    cx.update_window(o.win, |_, w, cx| filter.update(cx, |f, cx| f.focus(w, cx)))
        .unwrap();
    press(cx, &o, "alt-left");
    assert_eq!(browse_sql(cx, &o), B_A1, "the WHERE bar keeps Alt+Left");
    let table = tab.read_with(cx, |t, cx| t.results_pane().read(cx).table().clone());
    cx.update_window(o.win, |_, w, cx| {
        table.read(cx).focus_handle(cx).focus(w, cx)
    })
    .unwrap();
    press(cx, &o, "alt-left");
    o.wait_browse(cx, A1);
    let inspector = o.shell.read_with(cx, |s, _| s.inspector().clone());
    cx.update_window(o.win, |_, w, cx| {
        inspector.read(cx).focus_handle(cx).focus(w, cx)
    })
    .unwrap();
    press(cx, &o, "alt-right");
    o.wait_browse(cx, B_A1);
}

#[gpui_kit::test]
fn back_never_replaces_sql_the_user_typed(cx: &mut TestAppContext) {
    let o = walked(cx);
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql("SELECT 42 AS mine", w, cx))
    })
    .unwrap();
    let shell = o.shell.clone();
    let stepped = cx
        .update_window(o.win, |_, w, cx| {
            shell.update(cx, |s, cx| s.nav_back(w, cx))
        })
        .unwrap();
    cx.run_until_parked();
    assert!(!stepped, "typed SQL stops the walk");
    assert_eq!(tab.read_with(cx, |t, cx| t.sql(cx)), "SELECT 42 AS mine");
    click_id(cx, o.win, "inspector-toggle");
    assert!(
        disabled(cx, &o, "inspector-back"),
        "and the inspector's Back"
    );
}

#[gpui_kit::test]
fn staging_an_edit_clears_the_history(cx: &mut TestAppContext) {
    let o = walked(cx);
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    assert!(tab.read_with(cx, |t, _| t.nav().can_back()));
    let m = o.model(cx);
    m.update(cx, |m, cx| m.add_row(cx));
    cx.run_until_parked();
    assert!(tab.read_with(cx, |t, _| t.nav().current().is_none() && !t.is_preview()));
}
