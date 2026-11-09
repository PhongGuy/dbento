//! The toolbar's split Run holds Run all; Stop shows only while there is something to stop.
use crate::support::{LONG, label_of, opened, present, wait_real};
use dbento_app::results_pane::paused_label;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext, px};

#[test]
fn a_paused_result_says_how_many_rows_and_that_more_exist() {
    assert_eq!(paused_label(1000), "first 1,000 rows · more available");
}

#[gpui_kit::test]
fn run_all_is_the_second_half_of_the_run_button(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    assert!(present(cx, o.win, "run-split"));
    assert_eq!(label_of(cx, o.win, "run-all").as_deref(), Some("Run all"));
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        let run = w.find("run").bounds();
        let all = w.find("run-all").bounds();
        assert!(
            (f32::from(all.origin.x) - f32::from(run.right())).abs() <= 1.,
            "{run:?} {all:?}"
        );
        assert_eq!((all.origin.y, all.size.height), (run.origin.y, px(28.)));
    })
    .unwrap();
}

#[gpui_kit::test]
fn stop_shows_only_while_a_statement_runs(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    assert!(!present(cx, o.win, "cancel"));
    o.run(cx, LONG);
    let m = o.model(cx);
    wait_real(cx, "the run", |cx| m.read_with(cx, |m, _| m.running()));
    assert!(present(cx, o.win, "cancel"));
    assert!(present(cx, o.win, "cancel-tip"));
    cx.update_window(o.win, |_, w, cx| w.click("cancel", cx))
        .unwrap();
    o.idle(cx);
    assert!(!present(cx, o.win, "cancel"));
}
