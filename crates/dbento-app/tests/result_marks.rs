//! What a result tab shows beside its number, and the toolbar's button size.
mod support;
use dbento_app::results_pane::{ResultMark, result_mark};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext, px};
use support::{MANY, Opened, label_of, opened, present, wait_real};

const T: &str = "CREATE TABLE t(x); INSERT INTO t VALUES (1),(2),(3);";

fn marks(cx: &mut TestAppContext, o: &Opened) -> Vec<ResultMark> {
    o.model(cx).read_with(cx, |m, _| {
        let (asking, paused) = (m.confirm().map(|p| p.stmt), m.paused());
        m.results()
            .iter()
            .map(|r| result_mark(r, asking, paused))
            .collect()
    })
}

#[gpui_kit::test]
fn finished_statements_show_rows_or_a_failure(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "SELECT x FROM t; SELECT * FROM nope");
    o.idle(cx);
    assert_eq!(marks(cx, &o), [ResultMark::Rows(3), ResultMark::Failed]);
}

#[gpui_kit::test]
fn a_write_names_what_it_changed_on_its_tab_and_footer(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "DELETE FROM t WHERE x > 1");
    o.idle(cx);
    assert_eq!(
        marks(cx, &o),
        [ResultMark::Changed("2 rows deleted".into())]
    );
    assert!(
        !present(cx, o.win, "footer-rows"),
        "a write has no row count"
    );
    assert_eq!(
        label_of(cx, o.win, "footer-note").as_deref(),
        Some("2 rows affected")
    );
}

#[gpui_kit::test]
fn a_statement_waiting_on_its_question_shows_waiting(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "UPDATE t SET x = 0");
    wait_real(cx, "the dialog", |cx| present(cx, o.win, "confirm-dialog"));
    assert_eq!(marks(cx, &o), [ResultMark::Waiting]);
    assert_eq!(
        label_of(cx, o.win, ("statement-result", 0usize)).as_deref(),
        Some("waiting for confirmation")
    );
}

#[gpui_kit::test]
fn a_parked_statement_shows_its_rows_and_paused(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let m = o.model(cx);
    m.update(cx, |m, _| m.set_max_rows(Some(10_000)));
    o.run(cx, MANY);
    wait_real(cx, "the parked stream", |cx| {
        m.read_with(cx, |m, _| m.has_more())
    });
    assert_eq!(marks(cx, &o), [ResultMark::Paused(2048)]);
}

#[gpui_kit::test]
fn toolbar_icon_buttons_are_28_px_like_run(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        assert_eq!(w.find("run").bounds().size.height, px(28.));
        for id in ["format", "query-save"] {
            let b = w.find(id).bounds();
            assert_eq!((b.size.width, b.size.height), (px(28.), px(28.)), "{id}");
        }
        assert_eq!(w.find("run-all").bounds().size.height, px(28.));
    })
    .unwrap();
}
