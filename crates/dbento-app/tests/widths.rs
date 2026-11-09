use crate::support;
use dbento_app::grid::{
    MONO_ADVANCE, ResultsDelegate, column_width, column_width_at, fit_width_at,
};
use dbento_app::results::ResultStore;
use dbento_app::results_pane::ResultsPane;
use dbento_app::tab_model::TabModel;
use gpui_kit::component::table::TableDelegate as _;
use gpui_kit::{AppContext as _, TestAppContext, px};

#[test]
fn widths_follow_the_longer_of_header_and_cells_within_bounds() {
    assert_eq!(column_width("a", "", 1), px(48.));
    assert_eq!(column_width("id", "", 2), px(49.));
    assert_eq!(column_width("name", "text", 5), px(98.));
    assert_eq!(column_width("a", "", 10), px(88.));
    assert_eq!(column_width("a", "", 100), px(400.));
    assert_eq!(column_width("name", "text", 0), px(98.));
}

#[test]
fn a_header_keeps_room_for_its_sort_arrow() {
    // The 12 px arrow and its 6 px gap sit beside the name, the type and their gap.
    let header = (5 + 7) as f32 * MONO_ADVANCE + 6.;
    assert!(column_width("rowid", "INTEGER", 1) >= px(header + 18. + 16.));
}
#[test]
fn sample_chars_come_from_the_first_batch_only() {
    // SQLite closes batches at 1,024 rows, so only rows past the first batch hold the wider text.
    let (columns, batches) = support::batches(
        "SELECT CASE WHEN x <= 1024 THEN 'ab' ELSE 'abcdefgh' END AS v FROM (WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 2000) SELECT x FROM r)",
    );
    assert!(batches.len() >= 2);
    let mut s = ResultStore::new(columns);
    assert_eq!(s.sample_chars(0), None);
    for b in batches {
        s.push(b);
    }
    assert_eq!(
        s.sample_chars(0),
        Some(2),
        "later, wider batches never rescan"
    );
    assert_eq!(s.sample_chars(1), None);
}
#[gpui_kit::test]
fn the_grid_sizes_data_columns_from_the_first_batch(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let fx = support::fixture(
        "CREATE TABLE t(a INTEGER, b TEXT); INSERT INTO t VALUES (7, 'abcdefghijklmnopqrstuvwxyz');",
        "",
    );
    let model = cx.new(|_| TabModel::new(support::spec(&fx, "lite")));
    let m = model.clone();
    cx.update(|cx| {
        gpui_kit::open_window(Default::default(), cx, |window, cx| {
            cx.new(|cx| ResultsPane::new(m, window, cx))
        })
    })
    .unwrap();
    model.update(cx, |m, cx| m.run("SELECT a, b FROM t".into(), cx));
    support::wait_real(cx, "the run", |cx| model.read_with(cx, |m, _| !m.running()));
    cx.update(|cx| {
        let d = ResultsDelegate {
            model: model.clone(),
            stmt: 0,
            selection: Default::default(),
            editing: None,
            menu_target: Default::default(),
        };
        assert_eq!(d.column(1, cx).width, column_width("a", "INTEGER", 1));
        let b = d.column(2, cx);
        assert_eq!(b.width, column_width("b", "TEXT", 26));
        assert!(b.resizable);
    });
}

#[test]
fn widths_scale_with_the_grid_font_size() {
    for (name, ty, cells) in [
        ("a", "", 1),
        ("id", "", 2),
        ("name", "text", 5),
        ("a", "", 10),
        ("a", "", 100),
    ] {
        assert_eq!(
            column_width_at(name, ty, cells, px(12.)),
            column_width(name, ty, cells),
            "{name}"
        );
    }
    // Ten cells at 24 px: 10 x 14.4 + 16 padding; float rounding may land a pixel either way.
    let wide = column_width_at("a", "", 10, px(24.));
    assert!(wide >= px(159.) && wide <= px(161.), "{wide:?}");
    let narrow = column_width_at("a", "", 10, px(10.));
    assert!(narrow < column_width("a", "", 10), "{narrow:?}");
    assert_eq!(
        column_width_at("a", "", 100, px(24.)),
        px(400.),
        "still capped"
    );
    assert_eq!(
        fit_width_at("a", "", 100, px(24.)),
        px(800.),
        "fit caps at FIT_MAX_WIDTH"
    );
}
