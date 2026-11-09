use crate::support;
use dbento_app::grid::{ResultsDelegate, cell_display, data_col};
use dbento_app::results_pane::ResultsPane;
use dbento_app::tab_model::TabModel;
use dbento_app::theme;
use dbento_core::CellRef;
use gpui_kit::component::table::{ColumnFixed, TableDelegate as _};
use gpui_kit::component::{ActiveTheme as _, Colorize as _, ThemeMode};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, Hsla, TestAppContext};

const T: &str = "CREATE TABLE t(a INTEGER, b TEXT); INSERT INTO t VALUES (7, 'x'), (8, NULL);";

fn shown(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<TabModel>) {
    support::init(cx);
    support::init_tokio(cx);
    let fx = support::fixture(T, "");
    let model = cx.new(|_| TabModel::new(support::spec(&fx, "lite")));
    let m = model.clone();
    let (win, _) = cx
        .update(|cx| {
            gpui_kit::open_window(Default::default(), cx, |window, cx| {
                cx.new(|cx| ResultsPane::new(m, window, cx))
            })
        })
        .unwrap();
    model.update(cx, |m, cx| {
        m.run("SELECT a, b FROM t ORDER BY a".into(), cx)
    });
    support::wait_real(cx, "the run", |cx| model.read_with(cx, |m, _| !m.running()));
    (win, model)
}
#[test]
fn the_gutter_is_no_data_column() {
    assert_eq!(data_col(0), None);
    assert_eq!(data_col(1), Some(0));
    assert_eq!(data_col(9), Some(8));
}
#[test]
fn null_is_dim_and_the_text_null_is_not() {
    assert!(cell_display(CellRef::Null).1);
    let (text, dim) = cell_display(CellRef::Text("NULL"));
    assert_eq!((text.as_ref(), dim), ("NULL", false));
}
#[gpui_kit::test]
fn the_gutter_is_column_zero_and_ids_keep_data_indexes(cx: &mut TestAppContext) {
    let (win, model) = shown(cx);
    cx.update(|cx| {
        let d = ResultsDelegate {
            model: model.clone(),
            stmt: 0,
            selection: Default::default(),
            editing: None,
            menu_target: Default::default(),
        };
        assert_eq!(d.columns_count(cx), 3);
        let gutter = d.column(0, cx);
        assert_eq!(gutter.fixed, Some(ColumnFixed::Left));
        assert!(!gutter.resizable && !gutter.movable);
        assert_eq!(d.column(1, cx).name.as_ref(), "a");
        assert_eq!(d.column(2, cx).name.as_ref(), "b");
        assert_eq!(d.cell_text(1, 1, cx), "8");
        assert_eq!(d.cell_text(1, 2, cx), "NULL");
        assert_eq!(d.cell_text(1, 0, cx), "");
    });
    assert_eq!(
        support::label_of(cx, win, ("result-col", 0usize)).as_deref(),
        Some("a")
    );
    assert_eq!(
        support::label_of(cx, win, ("result-col", 1usize)).as_deref(),
        Some("b")
    );
    assert!(!support::present(cx, win, ("result-col", 2usize)));
    assert_eq!(support::label_of(cx, win, "cell-0-0").as_deref(), Some("7"));
    assert_eq!(
        support::label_of(cx, win, "cell-1-1").as_deref(),
        Some("NULL")
    );
    assert!(!support::present(cx, win, "cell-0-2"));
}
// Dim (types, NULL) and faint (row and line numbers) are separate tokens; pin both per theme.
#[gpui_kit::test]
fn dim_is_the_hint_colour_and_faint_the_line_number_colour(cx: &mut TestAppContext) {
    support::init(cx);
    cx.update(|cx| {
        for (mode, dim, faint) in [
            (ThemeMode::Dark, "#9494A0", "#6C6C76"),
            (ThemeMode::Light, "#62626F", "#85858F"),
        ] {
            theme::set_mode(mode, None, cx);
            assert_eq!(
                theme::dim(cx),
                Hsla::parse_hex(dim).unwrap(),
                "{mode:?} dim"
            );
            assert_eq!(
                theme::faint(cx),
                Hsla::parse_hex(faint).unwrap(),
                "{mode:?} faint"
            );
        }
    });
}
// The kit's 4 px vertical padding left a 24 px row a 16 px box, which clipped a mono line's
// descenders; the 28 px row must still hold a whole line.
#[gpui_kit::test]
fn a_cell_box_holds_a_whole_mono_line(cx: &mut TestAppContext) {
    let (win, _) = shown(cx);
    let size = cx.update(|cx| cx.theme().mono_font_size);
    let height = cx
        .update_window(win, |_, w, cx| {
            w.render_frame(cx);
            w.find("cell-0-1").bounds().size.height
        })
        .unwrap();
    // gpui's default line height is phi times the font size.
    assert!(height >= size * 1.618, "{height:?} for a {size:?} font");
}

#[test]
fn a_selection_marks_its_cells_and_rings_only_a_single_cell() {
    use dbento_app::grid::selection_marks;
    use gpui_kit::component::table::TableSelection as S;
    assert_eq!(selection_marks(S::None, 0, 1), (false, false));
    assert_eq!(selection_marks(S::Row(2), 2, 0), (true, false));
    assert_eq!(selection_marks(S::Row(2), 2, 3), (true, false));
    assert_eq!(selection_marks(S::Row(2), 1, 3), (false, false));
    assert_eq!(selection_marks(S::Column(2), 7, 2), (true, false));
    assert_eq!(selection_marks(S::Column(2), 7, 3), (false, false));
    assert_eq!(selection_marks(S::Cell(1, 2), 1, 2), (true, true));
    assert_eq!(selection_marks(S::Cell(1, 2), 1, 3), (false, false));
    assert_eq!(selection_marks(S::Cell(1, 0), 1, 3), (true, false));
}
