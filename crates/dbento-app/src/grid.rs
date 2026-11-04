//! The result grid: a kit `DataTable` delegate over one statement's `ResultStore`, and the
//! text each cell shows.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::rc::Rc;

use dbento_core::{CellRef, ColumnInfo, EditValue, FkDirection};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::table::{Column, TableDelegate, TableSelection, TableState};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Div, Edges, Entity, FontWeight, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Pixels, SharedString, StatefulInteractiveElement, Styled,
    TestSupportExt, Window, div, px,
};

use crate::grid_menu::GridTarget;
use crate::results::ResultStore;
use crate::runner::RunnerTuning;
use crate::staged::{GridRow, RowEdit, was_fresh_text};
use crate::tab_model::TabModel;
use crate::{settings, theme, ui};

/// The kit's 4 px top and bottom cell padding would crowd a mono line in a grid row;
/// every column drops the vertical padding and centres its single line instead.
const CELL_PADDING: Edges<Pixels> = Edges {
    top: px(0.),
    right: px(8.),
    bottom: px(0.),
    left: px(8.),
};

const MAX_TEXT_CHARS: usize = 256;
const MAX_HEX_BYTES: usize = 32;

/// The text a cell shows and whether it is dimmed. NULL is a dimmed "NULL"; text shows its
/// first line, at most 256 characters; bytes show as `\x` hex of the first 32 bytes, with
/// "…" when longer, followed by the byte count.
pub fn cell_display(cell: CellRef<'_>) -> (SharedString, bool) {
    match cell {
        CellRef::Null => ("NULL".into(), true),
        CellRef::Text(s) => {
            let line = s.lines().next().unwrap_or("");
            (
                line.chars().take(MAX_TEXT_CHARS).collect::<String>().into(),
                false,
            )
        }
        CellRef::Bytes(b) => {
            let mut out = String::from("\\x");
            for byte in b.iter().take(MAX_HEX_BYTES) {
                let _ = write!(out, "{byte:02x}");
            }
            if b.len() > MAX_HEX_BYTES {
                out.push('…');
            }
            let unit = if b.len() == 1 { "byte" } else { "bytes" };
            let _ = write!(out, " ({} {unit})", b.len());
            (out.into(), false)
        }
        // A kind core adds later is flagged, never passed off as NULL.
        _ => ("(unsupported value)".into(), true),
    }
}

/// What a staged value shows: text as [`cell_display`] shows it, "NULL" and "DEFAULT" dimmed.
pub fn staged_display(value: &EditValue) -> (SharedString, bool) {
    match value {
        EditValue::Null => ("NULL".into(), true),
        EditValue::Default => ("DEFAULT".into(), true),
        EditValue::Text(s) => cell_display(CellRef::Text(s)),
        EditValue::Bytes(b) => cell_display(CellRef::Bytes(b)),
        _ => ("(unsupported value)".into(), true),
    }
}

/// Whether a column of `type_name` (and Postgres type `pg_oid`, which wins when known)
/// holds numbers, so its cells align right.
pub fn is_numeric(type_name: &str, pg_oid: Option<u32>) -> bool {
    // int2, int4, int8, float4, float8, numeric.
    const PG_NUMERIC: [u32; 6] = [21, 23, 20, 700, 701, 1700];
    match pg_oid {
        Some(oid) => PG_NUMERIC.contains(&oid),
        None => {
            let t = type_name.to_ascii_uppercase();
            ["INT", "REAL", "FLOA", "DOUB", "NUM", "DEC"]
                .iter()
                .any(|k| t.contains(k))
        }
    }
}

fn numeric_column(col: &ColumnInfo) -> bool {
    is_numeric(col.type_name(), col.pg_type_oid())
}

/// Feeds the kit's table from statement `stmt` of the tab's current run. It holds no rows
/// itself; every call reads the model, so rows arriving later show on the next render.
pub struct ResultsDelegate {
    /// The tab whose results are shown.
    pub model: Entity<TabModel>,
    /// The index into `TabModel::results` of the statement shown.
    pub stmt: usize,
    /// The table's selection, mirrored by the pane because `render_td` cannot read its own state.
    pub selection: TableSelection,
    /// The open in-cell editor: table row, full table column (data + 1) and its input.
    pub editing: Option<(usize, usize, Entity<InputState>)>,
    /// Where the last right-click landed, shared with the pane, whose context menu reads it.
    pub menu_target: Rc<Cell<GridTarget>>,
}

/// The advance of one monospace character at the grid's font size.
pub const MONO_ADVANCE: f32 = 7.2;
/// The narrowest a data column starts out.
pub const MIN_COL_WIDTH: Pixels = px(48.);
/// The widest a data column starts out.
pub const MAX_COL_WIDTH: Pixels = px(400.);

/// Room the header keeps for the sort arrow: the 12 px icon and its 6 px gap.
const SORT_ROOM: f32 = 18.;

/// A data column's starting width: the wider of the header (name, type at `MONO_ADVANCE` per
/// char, a 6 px gap when a type shows, and `SORT_ROOM`) and the sampled cell text, plus 16 px
/// of padding, clamped to `MIN_COL_WIDTH..=MAX_COL_WIDTH`.
pub fn column_width(name: &str, type_name: &str, cells: usize) -> Pixels {
    column_width_at(name, type_name, cells, px(12.))
}

/// `column_width` with the per-character advance scaled to `font_size` (MONO_ADVANCE is the
/// 12 px advance).
pub fn column_width_at(name: &str, type_name: &str, cells: usize, font_size: Pixels) -> Pixels {
    content_width(name, type_name, cells, advance(font_size)).clamp(MIN_COL_WIDTH, MAX_COL_WIDTH)
}

/// The widest a fitted column grows.
pub const FIT_MAX_WIDTH: Pixels = px(800.);

/// `column_width`'s rule with `FIT_MAX_WIDTH` as the upper bound.
pub fn fit_width(name: &str, type_name: &str, cells: usize) -> Pixels {
    fit_width_at(name, type_name, cells, px(12.))
}

/// `fit_width` with the same scaling.
pub fn fit_width_at(name: &str, type_name: &str, cells: usize, font_size: Pixels) -> Pixels {
    content_width(name, type_name, cells, advance(font_size)).clamp(MIN_COL_WIDTH, FIT_MAX_WIDTH)
}

// Only the character advance follows the font size; the gap, sort room and padding stay fixed.
fn advance(font_size: Pixels) -> f32 {
    MONO_ADVANCE * f32::from(font_size) / 12.
}

/// The unclamped width of a column's header or its widest cell text, padding included.
fn content_width(name: &str, type_name: &str, cells: usize, advance: f32) -> Pixels {
    let chars = name.chars().count() + type_name.chars().count();
    let gap = if type_name.is_empty() { 0. } else { 6. };
    let header = chars as f32 * advance + gap + SORT_ROOM;
    px(header.max(cells as f32 * advance).ceil() + 16.)
}

/// Which data columns of one result the grid shows, and widths the user set for them. The
/// kit knows no hidden column, so the grid's table indexes count only visible columns
/// ("visible"), while selections handed to the rest of the app count every column ("full",
/// data + 1); this maps between the two.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ColumnView {
    names: Vec<String>,
    /// Data columns shown, in order.
    visible: Vec<usize>,
    widths: BTreeMap<usize, Pixels>,
}

impl ColumnView {
    /// Every column of a result with `names` shown at its starting width.
    pub fn new(names: Vec<String>) -> Self {
        let visible = (0..names.len()).collect();
        ColumnView {
            names,
            visible,
            widths: BTreeMap::new(),
        }
    }

    /// The result's column names the view was made for.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Hides data column `data`. False when out of range, already hidden, or the last
    /// visible column.
    pub fn hide(&mut self, data: usize) -> bool {
        match self.visible.iter().position(|&d| d == data) {
            Some(at) if self.visible.len() > 1 => {
                self.visible.remove(at);
                true
            }
            _ => false,
        }
    }

    /// Shows every column again; false when nothing was hidden.
    pub fn show_all(&mut self) -> bool {
        let hidden = self.any_hidden();
        self.visible = (0..self.names.len()).collect();
        hidden
    }

    /// Whether any column is hidden.
    pub fn any_hidden(&self) -> bool {
        self.visible.len() < self.names.len()
    }

    /// The data columns shown, in order.
    pub fn visible(&self) -> &[usize] {
        &self.visible
    }

    /// How many data columns show.
    pub fn visible_count(&self) -> usize {
        self.visible.len()
    }

    /// Visible table index -> data column; None for the gutter (0) and past the end.
    pub fn data_col(&self, ix: usize) -> Option<usize> {
        self.visible.get(ix.checked_sub(1)?).copied()
    }

    /// Data column -> visible table index; None when hidden.
    pub fn table_col(&self, data: usize) -> Option<usize> {
        self.visible
            .iter()
            .position(|&d| d == data)
            .map(|at| at + 1)
    }

    /// Visible-index selection -> full-index selection (data + 1); rows and the gutter unchanged.
    pub fn full_selection(&self, sel: TableSelection) -> TableSelection {
        let full = |ix: usize| match ix {
            0 => 0,
            ix => self.data_col(ix).map_or(ix, |d| d + 1),
        };
        match sel {
            TableSelection::Cell(r, c) => TableSelection::Cell(r, full(c)),
            TableSelection::Column(c) => TableSelection::Column(full(c)),
            other => other,
        }
    }

    /// Full-index selection -> visible; None when it names a hidden column.
    pub fn visible_selection(&self, sel: TableSelection) -> Option<TableSelection> {
        let visible = |ix: usize| match ix {
            0 => Some(0),
            ix => self.table_col(ix - 1),
        };
        Some(match sel {
            TableSelection::Cell(r, c) => TableSelection::Cell(r, visible(c)?),
            TableSelection::Column(c) => TableSelection::Column(visible(c)?),
            other => other,
        })
    }

    /// Sets data column `data`'s width, overriding its starting width.
    pub fn set_width(&mut self, data: usize, width: Pixels) {
        self.widths.insert(data, width);
    }

    /// The width set for data column `data`, if any.
    pub fn width(&self, data: usize) -> Option<Pixels> {
        self.widths.get(&data).copied()
    }
}

/// The row-number gutter's width: 48 px fits four digits, and each further digit of the
/// run's row cap adds 8 px. No cap is sized as eight digits, since the memory budget, not a
/// row count, bounds it.
pub fn gutter_width(cap: Option<u64>) -> Pixels {
    let digits = cap.unwrap_or(10_000_000).max(1).ilog10() + 1;
    px(48. + 8. * digits.saturating_sub(4) as f32)
}

/// The cell between its staged tint below and its selection mark on top; the cell truncates
/// (clips), so neither layer can be its child. `target` lies beneath them all.
fn layered(
    target: Div,
    under: Option<AnyElement>,
    cell: impl IntoElement,
    overlay: Option<impl IntoElement>,
    edge: Option<AnyElement>,
) -> impl IntoElement {
    div()
        .relative()
        .size_full()
        .child(target)
        .children(under)
        .child(cell)
        .children(overlay)
        .children(edge)
}

/// An absolute layer covering the whole cell, padding included, like the selection mark.
fn cell_layer() -> Div {
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .left(-CELL_PADDING.left)
        .right(-CELL_PADDING.right)
}

/// Whether the cell at table position (`row_ix`, `col_ix`) is inside `sel`, and whether it is
/// the one selected cell, which also gets a ring. The gutter cell stands for its whole row.
pub fn selection_marks(sel: TableSelection, row_ix: usize, col_ix: usize) -> (bool, bool) {
    match sel {
        TableSelection::None => (false, false),
        TableSelection::Row(r) | TableSelection::Cell(r, 0) => (r == row_ix, false),
        TableSelection::Column(c) => (c == col_ix, false),
        TableSelection::Cell(r, c) => (r == row_ix && c == col_ix, r == row_ix && c == col_ix),
    }
}

/// The data column shown at table column `ix`; `None` for the row-number gutter at 0.
pub fn data_col(ix: usize) -> Option<usize> {
    ix.checked_sub(1)
}

/// `sel` with `inserted` pinned rows removed: a row or cell on an inserted row is None,
/// a loaded one shifts up by `inserted`; a column or no selection is unchanged.
pub fn loaded_selection(sel: TableSelection, inserted: usize) -> Option<TableSelection> {
    match sel {
        TableSelection::Row(r) => Some(TableSelection::Row(r.checked_sub(inserted)?)),
        TableSelection::Cell(r, c) => Some(TableSelection::Cell(r.checked_sub(inserted)?, c)),
        other => Some(other),
    }
}

impl ResultsDelegate {
    fn store<'a>(&self, cx: &'a App) -> Option<&'a ResultStore> {
        self.model
            .read(cx)
            .results()
            .get(self.stmt)
            .map(|r| &r.store)
    }

    /// The tab's column view of this statement, while it still fits the result.
    fn view<'a>(&self, cx: &'a App) -> Option<&'a ColumnView> {
        self.model.read(cx).column_view(self.stmt)
    }

    /// The data column shown at visible table column `ix`; None for the gutter.
    fn data(&self, ix: usize, cx: &App) -> Option<usize> {
        match self.view(cx) {
            Some(view) => view.data_col(ix),
            None => data_col(ix),
        }
    }

    fn info<'a>(&self, ix: usize, cx: &'a App) -> Option<&'a ColumnInfo> {
        self.store(cx)?.columns().get(self.data(ix, cx)?)
    }

    /// Whether `column` is in the browse table's primary key, and the name of the table its
    /// outgoing key references; both empty off the browse result.
    fn key_marks(&self, column: &str, cx: &App) -> (bool, Option<String>) {
        let model = self.model.read(cx);
        let Some(state) = model
            .browse()
            .filter(|_| self.stmt == 0 && model.browse_result())
        else {
            return (false, None);
        };
        let pk = state
            .target()
            .column(column)
            .is_some_and(|c| c.meta().primary_key().is_some());
        let fk = state
            .foreign_keys()
            .iter()
            .find(|k| {
                k.direction() == FkDirection::Outgoing && k.columns().iter().any(|c| c == column)
            })
            .map(|k| k.referenced().name().to_owned());
        (pk, fk)
    }

    fn numeric(&self, ix: usize, cx: &App) -> bool {
        self.info(ix, cx).is_some_and(numeric_column)
    }

    /// Where table row `row` lives; staging, inserted rows included, covers only the browse
    /// result, `stmt` 0.
    fn grid_row(&self, row: usize, cx: &App) -> GridRow {
        if self.stmt == 0 {
            self.model.read(cx).grid_row(row)
        } else {
            GridRow::Loaded(row as u64)
        }
    }

    /// A staged cell shows its staged value, and an inserted row's unset cell a dimmed DEFAULT.
    fn display(&self, row: usize, ix: usize, cx: &App) -> Option<(SharedString, bool)> {
        let data = self.data(ix, cx)?;
        let store = self.store(cx)?;
        let edits = self.model.read(cx).edits();
        match self.grid_row(row, cx) {
            GridRow::Inserted(n) => {
                store.columns().get(data)?;
                Some(
                    edits
                        .new_cell(n, data)
                        .map_or_else(|| staged_display(&EditValue::Default), staged_display),
                )
            }
            GridRow::Loaded(r) => match edits.cell(r, data).filter(|_| self.stmt == 0) {
                Some(value) => Some(staged_display(value)),
                None => store.cell(r, data).map(cell_display),
            },
        }
    }
}

// Table column 0 is a fixed row-number gutter; every other visible table column `ix` shows
// data column `self.data(ix)`, and its ids, keys and menu targets carry that data index.
impl TableDelegate for ResultsDelegate {
    fn columns_count(&self, cx: &App) -> usize {
        let shown = self.view(cx).map(ColumnView::visible_count);
        self.store(cx)
            .map_or(0, |s| shown.unwrap_or(s.columns().len()) + 1)
    }

    // The pane draws why the grid is empty over it, so the kit's inbox icon stays out.
    fn render_empty(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
    }

    fn rows_count(&self, cx: &App) -> usize {
        let inserted = match self.stmt {
            0 => self.model.read(cx).edits().inserted(),
            _ => 0,
        };
        self.store(cx).map_or(0, |s| s.rows() as usize + inserted)
    }

    fn column(&self, col_ix: usize, cx: &App) -> Column {
        let Some(data) = self.data(col_ix, cx) else {
            return Column::new("#", "")
                .paddings(CELL_PADDING)
                .width(gutter_width(self.model.read(cx).run_limits().max_rows))
                .fixed_left()
                .resizable(false)
                .movable(false)
                .text_right();
        };
        let Some(info) = self.info(col_ix, cx) else {
            return Column::new(data.to_string(), "").paddings(CELL_PADDING);
        };
        let cells = self
            .store(cx)
            .and_then(|s| s.sample_chars(data))
            .unwrap_or(0);
        let width = self
            .view(cx)
            .and_then(|v| v.width(data))
            .unwrap_or_else(|| {
                column_width_at(
                    info.name(),
                    info.type_name(),
                    cells,
                    settings::grid_font(cx).1,
                )
            });
        Column::new(data.to_string(), info.name().to_owned())
            .paddings(CELL_PADDING)
            .width(width)
            .when(numeric_column(info), Column::text_right)
    }

    fn render_th(
        &mut self,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let dim = theme::dim(cx);
        let (mono, size) = settings::grid_font(cx);
        let theme = cx.theme();
        let (fg, accent) = (theme.foreground, theme.primary);
        let head = div()
            .size_full()
            .min_w_0()
            .flex()
            .items_center()
            .gap_1p5()
            .font_family(mono)
            .text_size(size);
        let Some(data) = self.data(col_ix, cx) else {
            return head.into_any_element();
        };
        let name = self.column(col_ix, cx).name;
        let type_name = self
            .info(col_ix, cx)
            .map(|i| i.type_name().to_owned())
            .filter(|t| !t.is_empty());
        // Some(sorted) where a click sorts instead of selecting: a browse result's sortable
        // column sorts on the server (an unsortable locator such as Postgres xmin keeps the
        // plain header), any other result's column sorts its loaded rows.
        let m = self.model.read(cx);
        let browsing = self.stmt == 0 && m.browse_result();
        let sort = if browsing {
            m.browse()
                .filter(|b| b.target().sortable(name.as_ref()))
                .map(|b| b.sort_of(name.as_ref()))
        } else {
            m.results().get(self.stmt).map(|r| {
                r.store
                    .local_sort()
                    .filter(|&(col, _)| col == data)
                    .map(|(_, descending)| descending)
            })
        };
        let arrow = sort.flatten().map(|descending| {
            let (icon, label) = if descending {
                (ui::Icons::ArrowDown, "descending")
            } else {
                (ui::Icons::ArrowUp, "ascending")
            };
            div()
                .id(("result-sort", data))
                .aria_label(label)
                .test_support()
                .flex_none()
                .child(ui::icon(icon, px(12.), accent))
        });
        // Key markers belong to the browse result's table; a free query's columns carry none.
        let (pk, fk) = self.key_marks(name.as_ref(), cx);
        let key_icon = |id: &'static str, icon, label: SharedString| {
            div()
                .id((id, data))
                .aria_label(label)
                .test_support()
                .flex_none()
                .child(ui::icon(icon, px(12.), dim))
        };
        let pk = pk.then(|| key_icon("result-pk", ui::Icons::KeyRound, "primary key".into()));
        let fk = fk.map(|to| {
            key_icon(
                "result-fk",
                ui::Icons::Link,
                format!("foreign key to {to}").into(),
            )
        });
        let model = self.model.clone();
        let column = name.clone();
        let menu_target = self.menu_target.clone();
        head.id(("result-col", data))
            // A right-click on the header aims the pane's menu at the whole column.
            .on_mouse_down(MouseButton::Right, move |_, _, _| {
                menu_target.set(GridTarget::Header(data + 1));
            })
            .aria_label(name.clone())
            .test_support()
            .when(self.numeric(col_ix, cx), |d| d.justify_end())
            .children(pk)
            .children(fk)
            // The name keeps its width and the type gives way first in a narrow column.
            .child(
                div()
                    .flex_shrink_0()
                    .max_w_full()
                    .truncate()
                    .font_weight(FontWeight::BOLD)
                    .text_color(fg)
                    .child(name),
            )
            .children(type_name.map(|t| div().min_w_0().truncate().text_color(dim).child(t)))
            .children(arrow)
            // Stopping propagation keeps the kit's column-header click from selecting the column.
            .when(sort.is_some(), |d| {
                let stmt = self.stmt;
                let current = sort.flatten();
                d.cursor_pointer().on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    model.update(cx, |m, cx| {
                        if browsing {
                            m.cycle_sort(&column, cx);
                        } else {
                            // Ascending, then descending, then loaded order, as `next_sort` cycles.
                            let next = match current {
                                None => Some(false),
                                Some(false) => Some(true),
                                Some(true) => None,
                            };
                            m.sort_loaded(stmt, data, next, cx);
                        }
                    });
                })
            })
            .into_any_element()
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let (dim, faint) = (theme::dim(cx), theme::faint(cx));
        // A right-click anywhere on the cell, padding included, records it for the pane's
        // menu; it runs before the kit's td handler, which stops propagation.
        // Targets and the editor count every column, hidden ones included.
        let full = self.data(col_ix, cx).map_or(0, |d| d + 1);
        let menu_target = self.menu_target.clone();
        let target = cell_layer().on_mouse_down(MouseButton::Right, move |_, _, _| {
            menu_target.set(GridTarget::Cell(row_ix, full));
        });
        let (mono, mono_size) = settings::grid_font(cx);
        let row_height = settings::active_metrics(cx).grid_row;
        let theme = cx.theme();
        let (mark, ring) = selection_marks(self.selection, row_ix, col_ix);
        // The mark spans the cell's padding, so it sits in an absolute layer over the cell.
        let overlay = mark.then(|| {
            cell_layer()
                .bg(theme.primary.opacity(0.2))
                .when(ring, |d| d.border_1().border_color(theme.primary))
        });
        // Staging covers only the browse result, statement 0; an inserted row has no loaded
        // row, so only its own mark applies.
        let at = self.grid_row(row_ix, cx);
        let edits = (self.stmt == 0).then(|| self.model.read(cx).edits());
        let (row, inserted) = match at {
            GridRow::Loaded(r) => (Some(r), None),
            GridRow::Inserted(n) => (None, Some(n)),
        };
        let deleted = edits.zip(row).is_some_and(|(e, r)| e.is_deleted(r));
        let row_mark = edits
            .and_then(|e| match at {
                GridRow::Loaded(r) => e.mark(r),
                GridRow::Inserted(n) => e.new_mark(n),
            })
            .map(SharedString::from);
        let cell = div()
            .size_full()
            .min_w_0()
            .line_height(row_height)
            .truncate()
            .font_family(mono.clone())
            .text_size(mono_size);
        let Some(data) = self.data(col_ix, cx) else {
            let modified = matches!(
                edits.zip(row).and_then(|(e, r)| e.row(r)),
                Some(RowEdit::Update { .. })
            );
            // A staged row gets a bar at the gutter's left edge, a failed one an alert icon
            // beside it whose tooltip and label carry the outcome.
            let bar = (deleted || modified).then(|| {
                let (id, color) = if deleted {
                    (format!("row-deleted-{row_ix}"), theme.danger)
                } else {
                    (format!("row-modified-{row_ix}"), theme.warning)
                };
                cell_layer()
                    .id(SharedString::from(id))
                    .test_support()
                    .right_auto()
                    .w(px(3.))
                    .bg(color)
            });
            let alert = row_mark.map(|text| {
                div()
                    .id(SharedString::from(format!("row-failed-{row_ix}")))
                    .aria_label(text.clone())
                    .test_support()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(3.) - CELL_PADDING.left + px(2.))
                    .flex()
                    .items_center()
                    .child(ui::icon(ui::Icons::TriangleAlert, px(12.), theme.danger))
                    .tooltip(move |window, cx| theme::tooltip(text.clone(), window, cx))
            });
            // Loaded rows keep their numbers; an inserted row shows "+" over its tint.
            let (label, tint) = match row {
                Some(r) => (
                    (r + 1).to_string(),
                    deleted.then(|| theme::edit_deleted(cx)),
                ),
                None => ("+".to_owned(), Some(theme::edit_inserted(cx))),
            };
            let under = cell_layer().when_some(tint, |d, c| d.bg(c));
            let under = match inserted {
                Some(_) => under
                    .id(SharedString::from(format!("row-inserted-{row_ix}")))
                    .test_support()
                    .into_any_element(),
                None => under.into_any_element(),
            };
            let number = cell.text_right().text_color(faint).child(label);
            let gutter = div()
                .size_full()
                .child(number)
                .children(bar)
                .children(alert);
            return layered(target, Some(under), gutter, overlay, None).into_any_element();
        };
        // An inserted row's cells get the inserted tint, a deleted row's the deleted tint, and
        // a staged cell the amber pending look, inset 1 px so the cursor ring draws outside it.
        // The pending edge draws above the selection fill, so a selected staged cell stays amber.
        let staged = !deleted
            && inserted.is_none()
            && edits
                .zip(row)
                .is_some_and(|(e, r)| e.cell(r, data).is_some());
        let pending = staged.then(|| theme::edit_pending(cx));
        // A staged cell's tooltip names the value it was loaded with, and after a reload of
        // its conflicting row what the database holds now.
        let was = row.filter(|_| staged).and_then(|r| {
            let (m, at) = (self.model.read(cx), GridRow::Loaded(r));
            let original = m.original_at(at, data)?;
            Some(SharedString::from(was_fresh_text(
                original,
                m.fresh_at(at, data),
            )))
        });
        let under = if inserted.is_some() {
            Some(cell_layer().bg(theme::edit_inserted(cx)).into_any_element())
        } else if deleted {
            Some(cell_layer().bg(theme::edit_deleted(cx)).into_any_element())
        } else {
            pending.map(|p| cell_layer().m(px(1.)).bg(p.fill).into_any_element())
        };
        let edge = pending.map(|pending| {
            cell_layer()
                .id(SharedString::from(format!("cell-modified-{row_ix}-{data}")))
                .test_support()
                .m(px(1.))
                .border_1()
                .border_color(pending.border)
                .into_any_element()
        });
        if let Some((_, _, input)) = self
            .editing
            .as_ref()
            .filter(|(r, c, _)| (*r, *c) == (row_ix, full))
        {
            // The input takes the cell's place with the text where the cell drew it; stopping
            // the click keeps the kit from escalating a click inside it to a row selection.
            let numeric = self.numeric(col_ix, cx);
            let editor = div()
                .id("cell-editor")
                .key_context("CellEditor")
                .test_support()
                .size_full()
                .flex()
                .items_center()
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(
                    Input::new(input)
                        .appearance(false)
                        .font_family(mono)
                        .text_size(mono_size)
                        .px_0()
                        .when(numeric, |i| i.text_right()),
                );
            return layered(target, under, editor, overlay, edge).into_any_element();
        }
        let (text, is_dim) = self.display(row_ix, col_ix, cx).unwrap_or_default();
        let value = cell
            .id(SharedString::from(format!("cell-{row_ix}-{data}")))
            .aria_label(text.clone())
            .test_support()
            .when(self.numeric(col_ix, cx), |d| d.text_right())
            .when(is_dim, |d| d.text_color(dim))
            .when(deleted, |d| d.line_through())
            .when_some(was, |d, was| {
                d.tooltip(move |window, cx| theme::tooltip(was.clone(), window, cx))
            })
            .child(text);
        layered(target, under, value, overlay, edge).into_any_element()
    }

    // The kit asks from prepaint whenever the visible end nears the last row; only the shown
    // result that is parked has more, and `pull` lets one request through at a time.
    fn has_more(&self, cx: &App) -> bool {
        let m = self.model.read(cx);
        m.has_more() && m.results().get(self.stmt).map(|r| r.stmt) == m.paused()
    }

    fn load_more_threshold(&self) -> usize {
        RunnerTuning::default().first_rows as usize
    }

    fn load_more(&mut self, _window: &mut Window, cx: &mut Context<TableState<Self>>) {
        self.model.update(cx, |m, _| m.pull());
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, cx: &App) -> String {
        self.display(row_ix, col_ix, cx)
            .map(|(text, _)| text.to_string())
            .unwrap_or_default()
    }
}
