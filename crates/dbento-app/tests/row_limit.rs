//! The row limit cycles through capped values and the gutter grows with the cap.
use crate::support::{label_of, opened};
use dbento_app::grid::gutter_width;
use dbento_app::query_tab::{next_row_limit, row_limit_label};
use gpui_kit::component::table::TableDelegate as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext, px};

#[test]
fn labels_steps_and_widths() {
    assert_eq!(row_limit_label(Some(1_000)), "1k rows");
    assert_eq!(row_limit_label(Some(100_000)), "100k rows");
    assert_eq!(row_limit_label(Some(1_000_000)), "1M rows");
    assert_eq!(row_limit_label(None), "no limit");
    assert_eq!(next_row_limit(Some(100_000)), Some(1_000_000));
    assert_eq!(next_row_limit(Some(1_000_000)), None);
    assert_eq!(next_row_limit(None), Some(1_000));
    assert_eq!(next_row_limit(Some(1_500)), Some(1_000));
    assert_eq!(gutter_width(Some(1_000)), px(48.));
    assert_eq!(gutter_width(Some(9_999)), px(48.));
    assert_eq!(gutter_width(Some(100_000)), px(64.));
    assert_eq!(gutter_width(Some(1_000_000)), px(72.));
    assert_eq!(gutter_width(None), px(80.));
}

#[gpui_kit::test]
fn the_control_cycles_and_sets_the_next_runs_cap(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let m = o.model(cx);
    assert_eq!(m.read_with(cx, |m, _| m.max_rows()), Some(1_000));
    assert_eq!(label_of(cx, o.win, "row-limit").as_deref(), Some("1k rows"));
    for (want, label) in [
        (Some(10_000), "10k rows"),
        (Some(100_000), "100k rows"),
        (Some(1_000_000), "1M rows"),
        (None, "no limit"),
        (Some(1_000), "1k rows"),
    ] {
        cx.update_window(o.win, |_, w, cx| w.click("row-limit", cx))
            .unwrap();
        assert_eq!(m.read_with(cx, |m, _| m.max_rows()), want);
        assert_eq!(label_of(cx, o.win, "row-limit").as_deref(), Some(label));
    }
}

#[gpui_kit::test]
fn the_gutter_fits_the_runs_cap(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    o.model(cx)
        .update(cx, |m, _| m.set_max_rows(Some(1_000_000)));
    o.run(cx, "SELECT 1");
    o.idle(cx);
    let table = o.shell.read_with(cx, |s, cx| {
        s.tab()
            .unwrap()
            .read(cx)
            .results_pane()
            .read(cx)
            .table()
            .clone()
    });
    let width = cx.update(|cx| table.read(cx).delegate().column(0, cx).width);
    assert_eq!(width, px(72.));
}
