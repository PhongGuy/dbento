//! Follows open in one preview tab per origin; staging or a title double-click pins it; a
//! follow that lands on one row selects it.
use crate::support::{Opened, launched, present, wait_real};
use dbento_core::{RelationKind, RelationRef};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

const T: &str = "CREATE TABLE a(id INTEGER PRIMARY KEY, name TEXT); \
                 CREATE TABLE b(id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a(id), note TEXT); \
                 INSERT INTO a VALUES (1, 'x'), (2, 'y'); \
                 INSERT INTO b VALUES (1, 1, 'n'), (2, NULL, 'm'), (3, 1, 'o');";

fn b(cx: &mut TestAppContext) -> Opened {
    let o = launched(cx, T, "");
    o.open_relation(cx, "b", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.b\nORDER BY id ASC");
    o
}
fn follow(cx: &mut TestAppContext, o: &Opened, rel: &str, filter: &str, rows: u64) {
    let (shell, rel, filter) = (
        o.shell.clone(),
        RelationRef::new("main", rel),
        filter.to_owned(),
    );
    let opened = cx
        .update_window(o.win, |_, w, cx| {
            shell.update(cx, |s, cx| s.open_filtered("lite", &rel, filter, w, cx))
        })
        .unwrap();
    assert!(opened);
    let m = o.model(cx);
    wait_real(cx, "the followed rows", |cx| {
        m.read_with(cx, |m, _| {
            !m.running()
                && m.browse().is_some()
                && m.results().first().is_some_and(|r| r.store.rows() == rows)
        })
    });
}
fn activate(cx: &mut TestAppContext, o: &Opened, ix: usize) {
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.activate(ix, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
}
fn tabs(cx: &mut TestAppContext, o: &Opened) -> usize {
    o.shell.read_with(cx, |s, _| s.tabs().len())
}
fn preview(cx: &mut TestAppContext, o: &Opened, ix: usize) -> bool {
    o.shell
        .read_with(cx, |s, cx| s.tabs()[ix].read(cx).is_preview())
}
fn current_row(cx: &mut TestAppContext, o: &Opened) -> Option<usize> {
    o.shell.read_with(cx, |s, cx| {
        s.tab()
            .unwrap()
            .read(cx)
            .results_pane()
            .read(cx)
            .current_row(cx)
    })
}

#[gpui_kit::test]
fn hops_from_one_origin_replace_one_preview_tab(cx: &mut TestAppContext) {
    let o = b(cx);
    follow(cx, &o, "a", "id = '1'", 1);
    assert_eq!(tabs(cx, &o), 2);
    assert!(preview(cx, &o, 1) && !preview(cx, &o, 0));
    assert!(present(cx, o.win, ("tab-preview", 1usize)));
    activate(cx, &o, 0);
    follow(cx, &o, "b", "a_id = '1'", 2);
    assert_eq!(tabs(cx, &o), 2);
    assert_eq!(o.shell.read_with(cx, |s, _| s.active()), Some(1));
    o.wait_browse(
        cx,
        "SELECT *\nFROM main.b\nWHERE (\na_id = '1'\n)\nORDER BY id ASC",
    );
    follow(cx, &o, "a", "id = '2'", 1);
    assert_eq!(tabs(cx, &o), 2, "a follow from the preview hops inside it");
    o.wait_browse(
        cx,
        "SELECT *\nFROM main.a\nWHERE (\nid = '2'\n)\nORDER BY id ASC",
    );
}

#[gpui_kit::test]
fn a_follow_that_lands_on_one_row_selects_it(cx: &mut TestAppContext) {
    let o = b(cx);
    follow(cx, &o, "a", "id = '1'", 1);
    wait_real(cx, "row 0 selected", |cx| current_row(cx, &o) == Some(0));
    follow(cx, &o, "b", "a_id = '1'", 2);
    assert_eq!(current_row(cx, &o), None, "two rows select nothing");
}

#[gpui_kit::test]
fn staging_or_a_title_double_click_pins_the_preview(cx: &mut TestAppContext) {
    let o = b(cx);
    follow(cx, &o, "a", "id = '1'", 1);
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    cx.run_until_parked();
    assert!(!preview(cx, &o, 1));
    assert!(!present(cx, o.win, ("tab-preview", 1usize)));
    activate(cx, &o, 0);
    follow(cx, &o, "a", "id = '2'", 1);
    assert_eq!(tabs(cx, &o), 3);
    assert!(preview(cx, &o, 2));
    cx.update_window(o.win, |_, w, cx| w.double_click(("tab", 2usize), cx))
        .unwrap();
    cx.run_until_parked();
    assert!(!preview(cx, &o, 2));
    activate(cx, &o, 0);
    follow(cx, &o, "a", "id = '1'", 1);
    assert_eq!(tabs(cx, &o), 4);
}

// SQL the user typed into a preview is work of theirs, so the next follow leaves it and opens
// a new preview.
#[gpui_kit::test]
fn a_preview_with_typed_sql_is_kept(cx: &mut TestAppContext) {
    let o = b(cx);
    follow(cx, &o, "a", "id = '1'", 1);
    let tab = o.shell.read_with(cx, |s, _| s.tabs()[1].clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql("SELECT 42 AS mine", w, cx))
    })
    .unwrap();
    activate(cx, &o, 0);
    follow(cx, &o, "a", "id = '2'", 1);
    assert_eq!(tabs(cx, &o), 3);
    assert!(!preview(cx, &o, 1) && preview(cx, &o, 2));
    assert_eq!(tab.read_with(cx, |t, cx| t.sql(cx)), "SELECT 42 AS mine");
}
