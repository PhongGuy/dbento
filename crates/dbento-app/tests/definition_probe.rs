//! The 1 MiB probe of the read-only SQL editor the definition dock is built on. Run it in
//! the timing profile; its line goes to the cap probe note.
mod support;
use std::time::Instant;

use dbento_app::confirm::sql_preview;
use dbento_core::DEFINITION_CAP;
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement as _, Render,
    Styled as _, TestAppContext, Window, div, px,
};

/// About `bytes` of one CASE view, a line per branch, under `bytes`.
fn big_sql(bytes: usize) -> String {
    let mut sql = String::from("CREATE VIEW big AS SELECT CASE 0\n");
    let mut i = 0;
    while sql.len() < bytes - 64 {
        sql.push_str(&format!("  WHEN {i} THEN 'value {i:08}'\n"));
        i += 1;
    }
    sql.push_str("END AS c;");
    sql
}
struct Probe {
    state: Entity<EditorState>,
}
impl Render for Probe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(Editor::new(&self.state).readonly(true).h(px(600.)))
    }
}

#[gpui_kit::test]
fn a_one_mib_preview_opens_and_jumps_to_its_end(cx: &mut TestAppContext) {
    support::init(cx);
    let sql = big_sql(DEFINITION_CAP);
    let started = Instant::now();
    let win = cx.add_window(|w, cx| Probe {
        state: sql_preview(&sql, w, cx),
    });
    cx.update_window(win.into(), |_, w, cx| w.render_frame(cx))
        .unwrap();
    cx.run_until_parked();
    let opened = started.elapsed();
    let state = win.update(cx, |p, _, _| p.state.clone()).unwrap();
    let moved = Instant::now();
    cx.update_window(win.into(), |_, w, cx| {
        state.read(cx).focus_handle(cx).focus(w, cx);
        w.press("ctrl-end", cx);
        w.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    let jumped = moved.elapsed();
    eprintln!(
        "sql_preview probe: {} bytes, {} lines; open and first frame {opened:?}; ctrl-end and frame {jumped:?}",
        sql.len(),
        sql.lines().count()
    );
    assert_eq!(state.read_with(cx, |s, _| s.value().len()), sql.len());
    // The jump time means something only if ctrl-end really reached the end.
    assert_eq!(state.read_with(cx, |s, _| s.cursor()), sql.len());
    // Time bounds hold only for the cap decision's timing-profile run under
    // DBENTO_CAP_PROBE=strict; any other build checks correctness and prints the times.
    if std::env::var("DBENTO_CAP_PROBE").as_deref() == Ok("strict") {
        assert!(
            opened.as_millis() < 500,
            "open and first frame took {opened:?}"
        );
        assert!(
            jumped.as_millis() < 100,
            "ctrl-end and frame took {jumped:?}"
        );
    }
}
