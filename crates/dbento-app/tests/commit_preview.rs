//! The apply prompt's SQL sits in a read-only, highlighted editor with typed literals.
use crate::support;
use crate::support::{label_of, launched, opened, present, wait_real};
use dbento_app::confirm::{sql_box, sql_preview};
use dbento_core::RelationKind;
use gpui_kit::component::input::EditorState;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    TestAppContext, Window, div, px,
};

#[gpui_kit::test]
fn the_preview_is_a_read_only_sql_editor_holding_the_text(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let sql = "UPDATE main.t SET name = 'z' WHERE id = 1;\nDELETE FROM main.t WHERE id = 2;";
    let state = cx
        .update_window(o.win, |_, w, cx| sql_preview(sql, w, cx))
        .unwrap();
    state.read_with(cx, |s, _| {
        assert_eq!(s.value().to_string(), sql);
        assert!(s.presentation().is_readonly());
        assert_eq!(s.language_name().to_string(), "sql");
    });
}

#[gpui_kit::test]
fn save_shows_typed_statements_in_the_preview_editor(cx: &mut TestAppContext) {
    let o = launched(
        cx,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); INSERT INTO t VALUES (1, 'b');",
        "",
    );
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    cx.run_until_parked();
    cx.update_window(o.win, |_, w, cx| w.click("save-edits", cx))
        .unwrap();
    wait_real(cx, "the prompt", |cx| present(cx, o.win, "confirm-dialog"));
    assert!(present(cx, o.win, "confirm-sql-editor"));
    assert_eq!(
        label_of(cx, o.win, "confirm-sql").as_deref(),
        Some("UPDATE main.t SET name = 'z' WHERE id = 1;")
    );
}

// The read-only editor is a tab stop; Roll back must still hold the first focus.
#[gpui_kit::test]
fn space_on_the_apply_prompt_still_rolls_back(cx: &mut TestAppContext) {
    let o = launched(
        cx,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); INSERT INTO t VALUES (1, 'b');",
        "",
    );
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    cx.run_until_parked();
    cx.update_window(o.win, |_, w, cx| w.click("save-edits", cx))
        .unwrap();
    wait_real(cx, "the prompt", |cx| {
        present(cx, o.win, "confirm-sql-editor")
    });
    cx.update_window(o.win, |_, w, cx| w.press("space", cx))
        .unwrap();
    wait_real(cx, "the prompt to close", |cx| {
        !present(cx, o.win, "confirm-dialog")
    });
    assert_eq!(o.fx.count("SELECT count(*) FROM t WHERE name = 'z'"), 0);
    assert!(present(cx, o.win, "pending-changes"));
}

struct Preview {
    sql: &'static str,
    state: Entity<EditorState>,
}
impl Render for Preview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(448.))
            .child(sql_box(self.sql.into(), &self.state, cx))
    }
}

// The box is sized from the line count, so its last statement must not be clipped by the
// editor's own padding, and the text starts where the approval dialog's SQL box starts its.
#[gpui_kit::test]
fn every_preview_line_fits_inside_the_box(cx: &mut TestAppContext) {
    support::init(cx);
    let sql = "UPDATE main.t SET name = 'a' WHERE id = 1;\n\
               UPDATE main.t SET name = 'b' WHERE id = 2;\n\
               DELETE FROM main.t WHERE id = 3;";
    let win = cx.add_window(|w, cx| Preview {
        sql,
        state: sql_preview(sql, w, cx),
    });
    let state = win.update(cx, |p, _, _| p.state.clone()).unwrap();
    for _ in 0..3 {
        cx.update_window(win.into(), |_, w, cx| w.render_frame(cx))
            .unwrap();
        cx.run_until_parked();
    }
    let (outer, boxed) = cx
        .update_window(win.into(), |_, w, _| {
            (
                w.find("confirm-sql").bounds(),
                w.find("confirm-sql-editor").bounds(),
            )
        })
        .unwrap();
    let last = sql.rfind('\n').unwrap() + 1;
    let (first, last) = state.read_with(cx, |s, _| {
        (
            s.range_to_bounds(&(0..1))
                .expect("the first line is laid out"),
            s.range_to_bounds(&(last..sql.len()))
                .expect("the last line is laid out"),
        )
    });
    assert!(
        last.bottom() <= boxed.bottom(),
        "last line {last:?} overflows the box {boxed:?}"
    );
    assert!(
        boxed.left() > outer.left() && boxed.right() < outer.right(),
        "the editor {boxed:?} sticks out of the box {outer:?}"
    );
    // The approval dialog's plain SQL box sets its text 9 px in: a 1 px border, 8 px padding.
    let inset = first.left() - outer.left();
    assert!(
        (inset - px(9.)).abs() <= px(1.),
        "text starts {inset:?} into the box"
    );
}
