//! The results pane: a tab per statement of the run, the grid over the selected result (by
//! default the last with columns), its footer, and the tab's connection or session error.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dbento_core::{
    ColumnInfo, EditValue, Engine, Outcome, RelationKind, RelationRef, Sort, named_source_columns,
    quote_ident,
};
use gpui_kit::ClipboardItem;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{EditorState, Input, InputEvent, InputState};
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::component::table::{
    DataTable, TableDelegate as _, TableEvent, TableSelection, TableState,
};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, Focusable as _, FontWeight,
    InteractiveElement, IntoElement, KeyDownEvent, Keystroke, ParentElement, Pixels, Render, Role,
    ScrollHandle, SharedString, StatefulInteractiveElement, Styled, Subscription, Task,
    TestSupportExt, WeakEntity, Window, div, px,
};
use gpui_kit::{DismissEvent, FocusHandle, MouseButton, MouseDownEvent, Point, anchored, deferred};

use crate::browse::{BrowseEvent, and_filter};
use crate::catalog::CatalogQuery;
use crate::confirm::{self, ChoiceStyle};
use crate::copy::{
    CopyCells, CopyFormat, copy_as, insert_maps_nothing, insert_target, insert_unwritable,
};
use crate::export::{chosen_path, export_format, export_text, write_export};
use crate::grid::{ColumnView, ResultsDelegate, data_col, loaded_selection};
use crate::grid_menu::{
    GridTarget, MenuEntry, MenuSection, filter_clause, filter_label, insert_table, menu_sections,
};
use crate::keymap::{
    CancelCellEdit, CommitCellEdit, CommitCellEditRight, DeleteRow, EditCell, RedoEdit,
    RunStatement, SaveEdits, SaveQuery, ToggleRowInspector, UndoEdit, chip_keys, keys_for,
};
use crate::plan_view::PlanView;
use crate::record::{record_at, record_position};
use crate::record_form::RecordForm;
use crate::result_body::{
    ErrorView, ResultBody, body_subject, code_label, dml_text, empty_text, error_copy_text,
    error_view, jump_target, result_body,
};
use crate::results::ResultStore;
use crate::runner::{PARK_AT_CAP, StatementEnd, StopReason};
use crate::sidebar::Sidebar;
use crate::smooth_scroll::{self, Animator};
use crate::staged::{self, GridRow, pending_label};
use crate::status_bar::{first_line, format_elapsed};
use crate::tab_model::{
    CapView, CopyKind, Expiry, SaveState, StatementResult, TabModel, raise_target,
};
use crate::{settings, theme, ui};

// How often a counting footer time repaints.
const TICK: Duration = Duration::from_millis(100);

pub use crate::labels::group_digits;

/// A row count with thousands separators and the right noun: "1 row", "1,000 rows".
pub fn format_rows(n: u64) -> String {
    format!(
        "{} {}",
        group_digits(n),
        if n == 1 { "row" } else { "rows" }
    )
}

/// The Save button's label: "Saving…" from the moment a save starts until it ends, so
/// the prompt's wait reads as a question rather than as work.
pub fn save_label(state: SaveState) -> &'static str {
    match state {
        SaveState::Idle | SaveState::Prompt => "Save",
        SaveState::Stopping | SaveState::Applying | SaveState::Finishing => "Saving…",
    }
}

/// The footer's count for a result parked at its cap: "1,000+ rows".
pub fn more_rows(n: u64) -> String {
    format!("{}+ rows", group_digits(n))
}

/// A paused or parked result's tab text: "first 1,000 rows · more available".
pub fn paused_label(rows: u64) -> String {
    format!("first {} · more available", format_rows(rows))
}

/// The `fetch-next` button's text for the run's row limit: "Fetch next 1,000".
pub fn fetch_next_label(limit: u64) -> String {
    format!("Fetch next {}", group_digits(limit))
}

/// The continuing `fetch-next` text of an expired browse: "Paused to free the connection ·
/// Fetch next 1,000".
pub fn continue_label(limit: u64) -> String {
    format!(
        "Paused to free the connection \u{b7} {}",
        fetch_next_label(limit)
    )
}

/// The `run-again` button's text for the limit it re-runs with: "Run again with 10,000".
pub fn run_again_label(limit: u64) -> String {
    format!("Run again with {}", group_digits(limit))
}

/// The notice a sorted continuation shows: it re-ran the browse and dropped `rows` loaded rows.
pub fn skip_notice(rows: u64) -> String {
    format!(
        "Re-ran the sorted table and skipped the {} rows already loaded; rows changed since may have moved.",
        group_digits(rows)
    )
}

/// A footer fetch button: ghost xsmall with a leading 14 px icon in `foreground`. The kit
/// sizes a button's own icon to the button, so icon and text go in as children; a disabled
/// button keeps the kit's dimmed text.
fn fetch_button(
    id: &'static str,
    icon: gpui_kit::assets::IconName,
    label: String,
    fg: gpui_kit::Hsla,
    disabled: bool,
) -> Button {
    let color = if disabled { fg.opacity(0.5) } else { fg };
    Button::new(id)
        .ghost()
        .xsmall()
        .gap_1()
        .when(!disabled, |b| b.text_color(fg))
        .disabled(disabled)
        .accessibility_label(label.clone())
        .child(ui::icon(icon, px(14.), color))
        .child(label)
}

/// The `raise-limit` button's text for the limit it raises to: "Raise limit to 10,000".
pub fn raise_label(cap: u64) -> String {
    format!("Raise limit to {}", group_digits(cap))
}

/// The footer note for a parked stream closed by `by`; an idle close names `PARK_AT_CAP`.
pub fn expiry_note(by: Expiry) -> String {
    match by {
        Expiry::Idle => format!("Stream closed after {} s", PARK_AT_CAP.as_secs()),
        Expiry::StatementTimeout => "Stream closed: statement timeout".into(),
        Expiry::Released => "Stream closed so another tab could commit".into(),
    }
}

/// What a result tab shows beside its number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultMark {
    /// Finished or streaming: the rows collected so far.
    Rows(u64),
    /// A completed statement without columns: what it changed, as the body's `dml_text`.
    Changed(String),
    /// The statement failed.
    Failed,
    /// The statement waits on its confirmation question.
    Waiting,
    /// The stream is parked at the fetch window with these rows.
    Paused(u64),
}

/// The mark for `r`, given the statement waiting on its question and the parked one.
pub fn result_mark(
    r: &StatementResult,
    asking: Option<usize>,
    paused: Option<usize>,
) -> ResultMark {
    if asking == Some(r.stmt) {
        ResultMark::Waiting
    } else if matches!(r.store.end(), Some(StatementEnd::Failed(_))) {
        ResultMark::Failed
    } else if paused == Some(r.stmt) {
        ResultMark::Paused(r.store.rows())
    } else if let Some(StatementEnd::Complete(o)) = r.store.end()
        && r.store.columns().is_empty()
    {
        ResultMark::Changed(dml_text(&r.sql, o.rows_affected))
    } else {
        ResultMark::Rows(r.store.rows())
    }
}

/// The footer note for a completed statement: "N affected", "any changes were rolled back",
/// or both joined by ", "; None when neither applies.
pub fn outcome_note(o: &Outcome) -> Option<String> {
    let affected = o
        .rows_affected
        .map(|n| format!("{} affected", format_rows(n)));
    let rolled_back = o
        .rolled_back
        .then(|| "any changes were rolled back".to_owned());
    match (affected, rolled_back) {
        (Some(a), Some(r)) => Some(format!("{a}, {r}")),
        (a, r) => a.or(r),
    }
}

/// One line describing how a statement ended, or "running" while it has not.
pub fn summary(r: &StatementResult) -> String {
    let outcome = |o: &Outcome| match o.rows_affected {
        Some(n) => format!("{} affected", format_rows(n)),
        None => format_rows(o.rows),
    };
    match r.store.end() {
        Some(StatementEnd::Complete(o)) => outcome(o),
        Some(StatementEnd::Truncated(o)) => format!("{} (row limit)", format_rows(o.rows)),
        Some(StatementEnd::Skipped) => "skipped".into(),
        Some(StatementEnd::Planned) => "plan".into(),
        Some(StatementEnd::Failed(e)) => e.to_string(),
        Some(StatementEnd::Stopped {
            reason: StopReason::Lost,
            ..
        }) => "session stopped".into(),
        Some(StatementEnd::Stopped {
            reason: StopReason::Closed,
            ..
        }) => "closing".into(),
        Some(StatementEnd::Stopped {
            rows,
            reason: StopReason::MemoryBudget,
        }) => format!("{} (memory budget)", format_rows(*rows)),
        Some(StatementEnd::Stopped { rows, .. }) => format!("{} (stopped)", format_rows(*rows)),
        None => "running".into(),
    }
}

/// The index of the last result that has columns, which is the one the grid shows.
pub fn shown(results: &[StatementResult]) -> Option<usize> {
    results.iter().rposition(|r| !r.store.columns().is_empty())
}

/// The result the grid shows: `selected` when it indexes `results`, else [`shown`].
pub fn displayed(results: &[StatementResult], selected: Option<usize>) -> Option<usize> {
    selected
        .filter(|&ix| ix < results.len())
        .or_else(|| shown(results))
}

/// Whether a key-down types into a selected cell: a `key_char` that is not a control
/// character, and no Ctrl, Alt, platform or function modifier (Shift allowed).
pub fn types_into_cell(keystroke: &Keystroke) -> bool {
    let m = &keystroke.modifiers;
    !(m.control || m.alt || m.platform || m.function)
        && keystroke
            .key_char
            .as_deref()
            .is_some_and(|c| !c.is_empty() && !c.chars().any(char::is_control))
}

/// A request from the read-only note to open its table in a browse tab.
pub struct OpenTable(pub RelationRef);

/// The note an edit attempt on a free query result raises; `source` is the one relation the
/// result's SQL reads, offered to open once the catalog says it is a table.
struct ReadonlyNote {
    source: Option<(Engine, RelationRef)>,
    /// Re-renders as the explorer's catalog loads the source's schema.
    _catalog: Option<Subscription>,
}

/// What the pane shows of the displayed result: the grid, or one row as a form.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ResultView {
    /// Every row in the grid.
    #[default]
    Grid,
    /// One row as a Name / Value form.
    Record,
    /// An explain's plan, chosen by the displayed result rather than set.
    Plan,
}

/// Which edit-strip buttons apply now; all false off an editable browse result.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StripActions {
    /// Add an empty inserted row.
    pub add: bool,
    /// Copy the selected row as an inserted one.
    pub duplicate: bool,
    /// Delete the selected row, or drop it when inserted.
    pub delete: bool,
    /// Step the staged edits back.
    pub undo: bool,
    /// Step them forward again.
    pub redo: bool,
    /// Drop every staged edit.
    pub revert: bool,
    /// Save the staged edits.
    pub save: bool,
}

impl StripActions {
    /// The field behind strip button `id`; None for any other id.
    pub fn enabled(&self, id: &str) -> Option<bool> {
        Some(match id {
            "edit-strip-add" => self.add,
            "edit-strip-duplicate" => self.duplicate,
            "edit-strip-delete" => self.delete,
            "undo-edits" => self.undo,
            "redo-edits" => self.redo,
            "revert-edits" => self.revert,
            "save-edits" => self.save,
            _ => return None,
        })
    }
}

/// The data columns `view` shows, in order; None when it hides nothing, as with no view.
fn visible_cols(view: Option<&ColumnView>) -> Option<&[usize]> {
    view.filter(|v| v.any_hidden()).map(ColumnView::visible)
}

/// An open grid menu: the menu, where it opened, the focus to restore and its dismiss watch.
type GridPopup = (
    Entity<PopupMenu>,
    Point<Pixels>,
    Option<FocusHandle>,
    Subscription,
);

/// The pane under a query tab's editor.
pub struct ResultsPane {
    model: Entity<TabModel>,
    table: Entity<TableState<ResultsDelegate>>,
    /// The result tab the user picked in this run; `None` follows [`shown`].
    selected: Option<usize>,
    /// The run, result index, column count and whether it has rows the table's columns were built for.
    built: Option<(u64, usize, usize, bool)>,
    /// The column view the table's columns were built with.
    built_view: Option<ColumnView>,
    /// The shown result's local sort when the grid last drew it.
    built_sort: Option<(usize, bool)>,
    /// The table's vertical scroll handle, which the wheel layer eases notches into.
    scroll_handle: ScrollHandle,
    /// The eased motion of wheel notches over the grid, shared with the layer.
    wheel: Rc<RefCell<Animator>>,
    /// The browse tab's WHERE text; it always shows the applied filter after a browse run.
    filter: Entity<InputState>,
    /// The in-cell editor's input, reused by every edit; the delegate says where it is open.
    cell_editor: Entity<InputState>,
    /// The open editor's table row and full table column (data + 1).
    editing: Option<(usize, usize)>,
    /// The inserted-row count `sync` last saw; a change moves every loaded row.
    inserted: usize,
    /// Where the last right-click on the grid landed: a cell, a header or below the rows.
    menu_target: Rc<Cell<GridTarget>>,
    /// The open grid menu, where it opened, and its dismiss subscription.
    grid_popup: Option<GridPopup>,
    /// Grid or record view of the displayed result; never Plan, which follows the result.
    view: ResultView,
    /// The plan view of the displayed plan result, keyed by run, result and whether it has output.
    plan: Option<((u64, usize, bool), Entity<PlanView>)>,
    // The shell's dock state, mirrored so the footer's inspector toggle can show it.
    inspector_open: bool,
    // The run id current when a hop asked for its single landed row to be selected.
    select_single: Option<u64>,
    /// The record view's form; it keeps its row while the grid shows.
    record: Entity<RecordForm>,
    /// Other forms over this pane's model (the inspector's); a save stages their open field
    /// and history steps wait for it, as for `record`.
    attached: Vec<(WeakEntity<RecordForm>, Subscription)>,
    /// Shown after an edit attempt on a result that is not the browse result.
    readonly: Option<ReadonlyNote>,
    /// The explorer and this tab's connection name, whose catalog says what a relation is.
    catalog: Option<(WeakEntity<Sidebar>, String)>,
    /// The tab's editor, whose text decides whether Jump to error still applies.
    editor: Option<(WeakEntity<EditorState>, Subscription)>,
    /// Repaints the footer's time while a Fetch or continuation is loading.
    tick: Option<Task<()>>,
    _observe: Subscription,
    _mirror: Subscription,
    _filter: [Subscription; 2],
}

impl ResultsPane {
    /// A pane over `model`'s results, following them as they change.
    pub fn new(model: Entity<TabModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let menu_target = Rc::<Cell<_>>::default();
        let delegate = ResultsDelegate {
            model: model.clone(),
            stmt: 0,
            selection: Default::default(),
            editing: None,
            menu_target: menu_target.clone(),
        };
        let table = cx.new(|cx| {
            TableState::new(delegate, window, cx)
                .sortable(false)
                .col_movable(false)
                .cell_selectable(true)
                .row_header(false)
        });
        let observe = cx.observe_in(&model, window, |this, _, window, cx| this.sync(window, cx));
        // Selection events carry no payload we need; the table's own value is mirrored into
        // the delegate so cells can paint it even when the pointer is elsewhere.
        let mirror = cx.subscribe_in(
            &table,
            window,
            |this, table, event: &TableEvent, window, cx| {
                this.follow_edit(event, window, cx);
                if let TableEvent::ColumnWidthsChanged(widths) = event {
                    this.record_widths(widths, cx);
                }
                if matches!(
                    event,
                    TableEvent::SelectRow(_)
                        | TableEvent::SelectColumn(_)
                        | TableEvent::SelectCell(..)
                        | TableEvent::ClearSelection
                ) {
                    // The kit scrolls to a selected row only when it is out of view, so a wheel ease
                    // still running would carry the new selection off screen: stop it where it is.
                    this.wheel.borrow_mut().cancel();
                    // The gutter header selects column 0, which copies nothing, so it marks nothing.
                    let sel = match table.read(cx).selection() {
                        TableSelection::Column(0) => TableSelection::None,
                        other => other,
                    };
                    this.table.update(cx, |t, cx| {
                        t.delegate_mut().selection = sel;
                        cx.notify();
                    });
                }
            },
        );
        // Enter applies the typed filter; every browse run then writes the applied filter
        // back, so text typed without Enter never lingers beside rows it did not select.
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("filter rows"));
        let enter = cx.subscribe_in(&filter, window, |this, input, e: &InputEvent, _, cx| {
            match e {
                InputEvent::PressEnter { .. } => {
                    let text = input.read(cx).value().to_string();
                    this.model.update(cx, |m, cx| m.apply_filter(&text, cx));
                }
                // The clear X follows whether there is text.
                InputEvent::Change => cx.notify(),
                _ => {}
            }
        });
        let applied = cx.subscribe_in(&model, window, |this, _, e: &BrowseEvent, window, cx| {
            if matches!(e, BrowseEvent::Run(_)) {
                this.reset_filter(window, cx);
            }
        });
        let cell_editor = cx.new(|cx| InputState::new(window, cx));
        let scroll_handle = table
            .read(cx)
            .vertical_scroll_handle
            .0
            .borrow()
            .base_handle
            .clone();
        let record = cx.new(|cx| RecordForm::new(model.clone(), window, cx));
        let mut pane = ResultsPane {
            model,
            table,
            selected: None,
            built: None,
            built_view: None,
            built_sort: None,
            scroll_handle,
            wheel: Rc::default(),
            filter,
            cell_editor,
            editing: None,
            inserted: 0,
            menu_target,
            grid_popup: None,
            view: ResultView::Grid,
            plan: None,
            inspector_open: false,
            select_single: None,
            record,
            attached: Vec::new(),
            readonly: None,
            catalog: None,
            editor: None,
            tick: None,
            _observe: observe,
            _mirror: mirror,
            _filter: [enter, applied],
        };
        pane.sync(window, cx);
        pane
    }

    /// The grid's table state.
    pub fn table(&self) -> &Entity<TableState<ResultsDelegate>> {
        &self.table
    }

    /// Plan while the displayed result is an explain that is pending or ended with its
    /// plan; else the grid or record view.
    pub fn view(&self) -> ResultView {
        if self.plan.is_some() {
            ResultView::Plan
        } else {
            self.view
        }
    }

    /// Whether the shell's row inspector shows, as the footer toggle draws it.
    pub fn inspector_open(&self) -> bool {
        self.inspector_open
    }

    /// Records the shell's inspector state for the footer toggle.
    pub fn set_inspector_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.inspector_open != open {
            self.inspector_open = open;
            cx.notify();
        }
    }

    /// Asks for the next run's statement 0 to select its row once it ends with exactly one.
    pub fn select_single_landed(&mut self, cx: &App) {
        self.select_single = Some(self.model.read(cx).run_id());
    }

    /// The record view's form.
    pub fn record_form(&self) -> &Entity<RecordForm> {
        &self.record
    }

    /// Record starts on the grid's selected row (0 when none) and finishes an open cell editor.
    pub fn set_view(&mut self, view: ResultView, window: &mut Window, cx: &mut Context<Self>) {
        // The active view stays as it is, on its row; Plan is never set, only shown.
        if view == self.view || view == ResultView::Plan {
            return;
        }
        self.finish_edit(true, false, window, cx);
        self.record.update(cx, |f, cx| f.stage_open(window, cx));
        match view {
            ResultView::Record => {
                let row = self.grid_row_selected(cx).unwrap_or(0);
                let stmt = displayed(self.model.read(cx).results(), self.selected).unwrap_or(0);
                self.record
                    .update(cx, |f, cx| f.show(stmt, row, window, cx));
                self.record.read(cx).focus_handle(cx).focus(window, cx);
            }
            // The grid comes back on the record's row, in the column it was on.
            ResultView::Grid => {
                let (stmt, row) = self.record.read(cx).shown();
                if row < self.record_rows(stmt, cx) {
                    self.select_grid_row(row, cx);
                }
                self.table.read(cx).focus_handle(cx).focus(window, cx);
            }
            ResultView::Plan => {}
        }
        self.view = view;
        cx.notify();
    }

    /// The index of the result the pane shows, if any.
    pub fn displayed(&self, cx: &App) -> Option<usize> {
        displayed(self.model.read(cx).results(), self.selected)
    }

    /// The record view's row, or the grid selection's row.
    pub fn current_row(&self, cx: &App) -> Option<usize> {
        match self.view {
            ResultView::Record => {
                let (stmt, row) = self.record.read(cx).shown();
                (row < self.record_rows(stmt, cx)).then_some(row)
            }
            ResultView::Grid => self.grid_row_selected(cx),
            ResultView::Plan => None,
        }
    }

    /// Selects table row `row` in the grid (after `sync`); in the Record view it also becomes
    /// the form's row.
    pub fn select_row(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        // A just-changed inserted-row count would clear the selection made here; sync first.
        self.sync(window, cx);
        self.table.update(cx, |t, cx| t.set_selected_row(row, cx));
        if self.view == ResultView::Record {
            let stmt = self.displayed(cx).unwrap_or(0);
            self.record
                .update(cx, |f, cx| f.show(stmt, row, window, cx));
        }
    }

    /// Moves the record view by `delta` rows within the displayed result and selects that row
    /// in the grid; false at either end.
    pub fn step_record(
        &mut self,
        delta: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (stmt, row) = self.record.read(cx).shown();
        let Some(next) = row
            .checked_add_signed(delta)
            .filter(|&r| r < self.record_rows(stmt, cx))
        else {
            return false;
        };
        self.record
            .update(cx, |f, cx| f.show(stmt, next, window, cx));
        self.select_grid_row(next, cx);
        true
    }

    // Selects table row `row`, keeping a cell selection's column so the cell stays selected
    // (and its status line shown) rather than widening to the whole row.
    fn select_grid_row(&mut self, row: usize, cx: &mut Context<Self>) {
        self.table.update(cx, |t, cx| match t.selection() {
            TableSelection::Cell(r, _) if r == row => {}
            TableSelection::Cell(_, col) => t.set_selected_cell(row, col, cx),
            _ => t.set_selected_row(row, cx),
        });
    }

    /// The table row of the grid's row or cell selection.
    fn grid_row_selected(&self, cx: &App) -> Option<usize> {
        match self.table.read(cx).selection() {
            TableSelection::Row(r) | TableSelection::Cell(r, _) => Some(r),
            _ => None,
        }
    }

    /// The tab's column view of the shown result, while it fits that result.
    fn column_view<'a>(&self, cx: &'a App) -> Option<&'a ColumnView> {
        let stmt = self.table.read(cx).delegate().stmt;
        self.model.read(cx).column_view(stmt)
    }

    /// The kit's selection, which counts visible columns, in full column indexes.
    fn selection(&self, cx: &App) -> TableSelection {
        let sel = self.table.read(cx).selection();
        self.column_view(cx).map_or(sel, |v| v.full_selection(sel))
    }

    /// The full column (data + 1) at visible table column `ix`.
    fn full_col(&self, ix: usize, cx: &App) -> usize {
        self.column_view(cx)
            .and_then(|v| v.data_col(ix))
            .map_or(ix, |data| data + 1)
    }

    /// The visible table column of full column `col`; None when it is hidden.
    fn table_col(&self, col: usize, cx: &App) -> Option<usize> {
        match (data_col(col), self.column_view(cx)) {
            (Some(data), Some(view)) => view.table_col(data),
            _ => Some(col),
        }
    }

    /// Records the widths a column drag left, as the kit reports them (visible order, gutter
    /// first), keeping only those that differ from what the delegate gave.
    fn record_widths(&mut self, widths: &[Pixels], cx: &mut Context<Self>) {
        let table = self.table.read(cx);
        let stmt = table.delegate().stmt;
        let changed: Vec<_> = widths
            .iter()
            .enumerate()
            .skip(1)
            .filter(|&(ix, &w)| table.delegate().column(ix, cx).width != w)
            .map(|(ix, &w)| (self.full_col(ix, cx) - 1, w))
            .collect();
        self.model
            .update(cx, |m, cx| m.set_column_widths(stmt, &changed, cx));
    }

    /// The rows the grid shows for result `stmt`: loaded, plus inserted on the browse result.
    fn record_rows(&self, stmt: usize, cx: &App) -> usize {
        let m = self.model.read(cx);
        let inserted = if stmt == 0 && m.browse_result() {
            m.edits().inserted()
        } else {
            0
        };
        m.results()
            .get(stmt)
            .map_or(0, |r| r.store.rows() as usize + inserted)
    }

    /// The in-cell editor's input.
    pub fn cell_editor(&self) -> &Entity<InputState> {
        &self.cell_editor
    }

    /// (table row, data column) of the open editor; the table row is the result row when
    /// nothing is inserted.
    pub fn editing(&self) -> Option<(u64, usize)> {
        let (row, col) = self.editing?;
        Some((row as u64, data_col(col)?))
    }

    /// Opens the editor on table cell (row, full col) with `cell_edit_text`, selected and
    /// focused; false when the cell cannot be edited or its column is hidden.
    pub fn begin_edit(
        &mut self,
        row: usize,
        col: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(data) = data_col(col).filter(|_| self.table_col(col, cx).is_some()) else {
            return false;
        };
        // Edits are keyed to the browse result, which is always result 0.
        if self.table.read(cx).delegate().stmt != 0 {
            return false;
        }
        let m = self.model.read(cx);
        let Some(text) = m.cell_text_at(m.grid_row(row), data) else {
            return false;
        };
        if self.editing.is_some() {
            self.finish_edit(true, false, window, cx);
        }
        self.cell_editor.update(cx, |s, cx| {
            s.set_value(text, window, cx);
            s.focus(window, cx);
            s.select_all(window, cx);
        });
        self.set_editing(Some((row, col)), cx);
        true
    }

    /// Closes the editor, staging its text when `stage`; `right` then selects the next cell.
    pub fn finish_edit(
        &mut self,
        stage: bool,
        right: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((row, col)) = self.editing else {
            return;
        };
        // Cleared first, so the selection events below no longer see an open editor.
        self.set_editing(None, cx);
        if stage && let Some(data) = data_col(col) {
            let text = self.cell_editor.read(cx).value().to_string();
            self.model.update(cx, |m, cx| {
                let at = m.grid_row(row);
                m.stage_text(at, data, &text, cx)
            });
        }
        let table = self.table.clone();
        table.read(cx).focus_handle(cx).focus(window, cx);
        // The next cell to the right is the next visible one.
        if right && let Some(ix) = self.table_col(col, cx) {
            table.update(cx, |t, cx| {
                if ix + 1 < t.delegate().columns_count(cx) {
                    t.set_selected_cell(row, ix + 1, cx);
                }
            });
        }
    }

    /// Marks the row of the selection deleted.
    pub fn delete_selected(&mut self, cx: &mut Context<Self>) -> bool {
        let row = match self.table.read(cx).selection() {
            TableSelection::Row(r) | TableSelection::Cell(r, _) => r,
            _ => return false,
        };
        self.model.update(cx, |m, cx| {
            let at = m.grid_row(row);
            m.delete_at(at, cx)
        })
    }

    /// The table (row, col) of a row or cell selection on a loaded or inserted row of the
    /// browse result; a row selection reports its first data column.
    fn strip_cell(&self, cx: &App) -> Option<(usize, usize)> {
        let m = self.model.read(cx);
        if self.table.read(cx).delegate().stmt != 0 || !m.browse_result() {
            return None;
        }
        let (row, col) = match self.selection(cx) {
            TableSelection::Row(r) => (r, 1),
            TableSelection::Cell(r, c) => (r, c),
            _ => return None,
        };
        let live = match m.grid_row(row) {
            GridRow::Loaded(r) => r < m.results()[0].store.rows(),
            GridRow::Inserted(ix) => ix < m.edits().inserted(),
        };
        live.then_some((row, col))
    }

    /// `was_fresh_text` of the grid-selected cell when it is staged on a loaded row of the
    /// browse result, or `fresh_text(Gone)` on a deleted row a reload found gone.
    pub fn was_text(&self, cx: &App) -> Option<String> {
        let TableSelection::Cell(row, col) = self.selection(cx) else {
            return None;
        };
        if self.table.read(cx).delegate().stmt != 0 {
            return None;
        }
        let m = self.model.read(cx);
        let (GridRow::Loaded(r), Some(data)) = (m.grid_row(row), data_col(col)) else {
            return None;
        };
        let at = GridRow::Loaded(r);
        // A conflicting delete stages no cell; what the reload found is all there is to say.
        if m.edits().is_deleted(r) && m.row_gone(at) {
            return Some(staged::fresh_text(staged::Fresh::Gone));
        }
        m.edits().cell(r, data)?;
        let original = m.original_at(at, data)?;
        Some(staged::was_fresh_text(original, m.fresh_at(at, data)))
    }

    /// What the edit strip's buttons may do now: row edits on an editable tab with no save
    /// running, history steps only while no editor is open, Revert and Save with edits staged.
    pub fn strip_actions(&self, cx: &App) -> StripActions {
        let m = self.model.read(cx);
        let idle = m.editable() && m.save_state() == SaveState::Idle;
        let at = self
            .strip_cell(cx)
            .filter(|_| idle)
            .map(|(row, _)| m.grid_row(row));
        let open = self.any_editor_open(cx);
        StripActions {
            add: idle,
            duplicate: at.is_some(),
            delete: at.is_some_and(|at| match at {
                GridRow::Loaded(r) => !m.edits().is_deleted(r),
                GridRow::Inserted(_) => true,
            }),
            undo: m.can_undo() && !open,
            redo: m.can_redo() && !open,
            revert: !m.edits().is_empty() && m.save_state() == SaveState::Idle,
            // An open editor's text is staged by the click, so it may be all there is to save.
            save: m.can_save() || (open && m.save_ready()),
        }
    }

    /// Records the editor's place on the pane and in the delegate, which renders it.
    fn set_editing(&mut self, at: Option<(usize, usize)>, cx: &mut Context<Self>) {
        self.editing = at;
        let input = self.cell_editor.clone();
        self.table.update(cx, |t, cx| {
            t.delegate_mut().editing = at.map(|(r, c)| (r, c, input));
            cx.notify();
        });
        cx.notify();
    }

    /// Opens the editor on a double-clicked cell, and stages an open one when another cell
    /// or row is selected. The kit escalates a click on the selected cell to its row, so a
    /// selection of the editing row itself leaves the editor open.
    fn follow_edit(&mut self, event: &TableEvent, window: &mut Window, cx: &mut Context<Self>) {
        match *event {
            // The kit's events carry visible columns; the editor counts every column.
            TableEvent::DoubleClickedCell(row, col) => {
                let col = self.full_col(col, cx);
                if !self.begin_edit(row, col, window, cx) {
                    self.refuse_edit(cx);
                }
            }
            TableEvent::SelectCell(row, col)
                if self
                    .editing
                    .is_some_and(|at| at != (row, self.full_col(col, cx))) =>
            {
                self.finish_edit(true, false, window, cx);
            }
            TableEvent::SelectRow(row) if self.editing.is_some_and(|(r, _)| r != row) => {
                self.finish_edit(true, false, window, cx);
            }
            _ => {}
        }
    }

    /// Lets the read-only note ask `sidebar`'s catalog, for connection `conn`, whether the
    /// relation a query reads is a table.
    pub fn set_catalog(&mut self, sidebar: WeakEntity<Sidebar>, conn: String) {
        self.catalog = Some((sidebar, conn));
    }

    /// Follows `editor` so Jump to error lands in it, and re-renders as it is typed in.
    pub fn attach_editor(&mut self, editor: &Entity<EditorState>, cx: &mut Context<Self>) {
        let observe = cx.observe(editor, |_, _, cx| cx.notify());
        self.editor = Some((editor.downgrade(), observe));
    }

    /// The subject's error position in the editor, while the editor still holds its SQL.
    fn jump_offset(&self, cx: &App) -> Option<usize> {
        let editor = self.editor.as_ref()?.0.upgrade()?;
        let results = self.model.read(cx).results();
        let r = &results[body_subject(results, self.selected)?];
        jump_target(&editor.read(cx).value(), r)
    }

    /// Puts the editor's caret on the subject's error and focuses it.
    fn jump_to_error(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(at), Some(editor)) = (
            self.jump_offset(cx),
            self.editor.as_ref().and_then(|(e, _)| e.upgrade()),
        ) else {
            return;
        };
        editor.update(cx, |e, cx| {
            e.set_selected_range(at..at, cx);
            e.focus(window, cx);
        });
    }

    /// After an edit attempt that opened nothing: on a free query result, shows the note that
    /// query results are read-only, offering the table when the SQL reads exactly one.
    fn refuse_edit(&mut self, cx: &mut Context<Self>) {
        let m = self.model.read(cx);
        if m.browse_result() {
            return;
        }
        let stmt = self.table.read(cx).delegate().stmt;
        let source = crate::grid_menu::sql_source(m, stmt);
        let sidebar = self
            .catalog
            .as_ref()
            .and_then(|(s, c)| Some((s.upgrade()?, c.clone())));
        let wait = source
            .as_ref()
            .zip(sidebar)
            .map(|((_, rel), (sidebar, conn))| {
                let schema = rel.schema().to_owned();
                sidebar.update(cx, |s, cx| {
                    s.ensure_loaded(&conn, &[CatalogQuery::Relations { schema }], cx)
                });
                cx.observe(&sidebar, |_, _, cx| cx.notify())
            });
        self.readonly = Some(ReadonlyNote {
            source,
            _catalog: wait,
        });
        cx.notify();
    }

    /// The table the note offers: its source as the catalog names it, when that is a table.
    /// SQLite matches names without regard to case, as the database does.
    fn offer(&self, cx: &App) -> Option<RelationRef> {
        let (engine, rel) = self.readonly.as_ref()?.source.as_ref()?;
        let (sidebar, conn) = self.catalog.as_ref()?;
        let sidebar = sidebar.upgrade()?;
        let tree = sidebar.read(cx).tree();
        let items = tree.relations(conn, rel.schema())?.items();
        let found = items.iter().find(|r| r.name() == rel.name()).or_else(|| {
            (*engine == Engine::Sqlite)
                .then(|| {
                    items
                        .iter()
                        .find(|r| r.name().eq_ignore_ascii_case(rel.name()))
                })
                .flatten()
        })?;
        (found.kind() == RelationKind::Table).then(|| RelationRef::new(rel.schema(), found.name()))
    }

    /// A printable key on a selected data cell opens its editor holding that text, caret at
    /// the end, and keeps the platform from inserting it a second time.
    fn type_to_edit(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // An open editor takes its own keys.
        if self.editing.is_some() || !types_into_cell(&event.keystroke) {
            return;
        }
        let TableSelection::Cell(row, col) = self.selection(cx) else {
            return;
        };
        let Some(text) = event.keystroke.key_char.clone().filter(|_| col > 0) else {
            return;
        };
        cx.stop_propagation();
        if self.begin_edit(row, col, window, cx) {
            // A single-line set_value leaves the caret after the text, unselected.
            self.cell_editor
                .update(cx, |s, cx| s.set_value(text, window, cx));
        } else {
            self.refuse_edit(cx);
        }
    }

    /// Tab in the cell editor: stages it, then opens the next editable cell to the right in
    /// the same row; with none, the selection just moves right as the editor closes.
    fn commit_right(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((row, col)) = self.editing else {
            return;
        };
        self.finish_edit(true, false, window, cx);
        let count = self.table.read(cx).delegate().columns_count(cx);
        // Steps run over visible columns, while the editor opens on full ones. Only the final
        // cell is selected: a passing selection would stage the new editor.
        let Some(ix) = self.table_col(col, cx) else {
            return;
        };
        let opened = (ix + 1..count).find(|&next| {
            let full = self.full_col(next, cx);
            self.begin_edit(row, full, window, cx)
        });
        if let Some(next) = opened.or((ix + 1 < count).then_some(ix + 1)) {
            self.table
                .update(cx, |t, cx| t.set_selected_cell(row, next, cx));
        }
    }

    /// Registers another form over this pane's model, such as the inspector's; dropped
    /// forms fall away on their own.
    pub fn attach_form(&mut self, form: &Entity<RecordForm>, cx: &mut Context<Self>) {
        self.attached.retain(|(f, _)| f.upgrade().is_some());
        // Only an opened or closed field changes the pane (its Undo and Redo buttons), so the
        // form's every row change does not re-render the grid.
        let mut open = form.read(cx).editing().is_some();
        let observe = cx.observe(form, move |_, form, cx| {
            let now = form.read(cx).editing().is_some();
            if now != open {
                open = now;
                cx.notify();
            }
        });
        self.attached.push((form.downgrade(), observe));
    }

    /// Every form over this pane's model: the record view's, then the attached ones.
    fn forms(&self) -> impl Iterator<Item = Entity<RecordForm>> + '_ {
        std::iter::once(self.record.clone())
            .chain(self.attached.iter().filter_map(|(f, _)| f.upgrade()))
    }

    /// Stages the open cell editor and every form's open field: what a save starts with.
    pub fn stage_open_editors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.finish_edit(true, false, window, cx);
        for form in self.forms().collect::<Vec<_>>() {
            form.update(cx, |f, cx| f.stage_open(window, cx));
        }
    }

    /// Whether the cell editor or any form's field is open; history steps wait for it.
    fn any_editor_open(&self, cx: &App) -> bool {
        self.editing.is_some() || self.forms().any(|f| f.read(cx).editing().is_some())
    }

    /// Ctrl+S in the pane: stages an open cell or field editor, then saves the staged edits,
    /// or with none staged hands the keys back to Save Query.
    pub fn save_edits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stage_open_editors(window, cx);
        if self.model.read(cx).edits().is_empty() {
            window.dispatch_action(Box::new(SaveQuery), cx);
        } else {
            self.model.update(cx, |m, cx| m.save(cx));
        }
    }

    /// The browse filter's input.
    pub fn filter_input(&self) -> &Entity<InputState> {
        &self.filter
    }

    /// Puts the applied filter back into the filter input, dropping text typed over it.
    pub fn reset_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self
            .model
            .read(cx)
            .browse()
            .and_then(|b| b.browse().filter.clone())
            .unwrap_or_default();
        self.filter
            .update(cx, |s, cx| s.set_value(text, window, cx));
    }

    // While a load counts, one pending repaint at a time keeps the footer's time moving.
    fn keep_ticking(&mut self, cx: &mut Context<Self>) {
        if !self.model.read(cx).ticking() {
            self.tick = None;
        } else if self.tick.is_none() {
            self.tick = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(TICK).await;
                let _ = this.update(cx, |this, cx| {
                    this.tick = None;
                    cx.notify();
                });
            }));
        }
    }

    /// Empties the filter and re-runs the browse without one.
    fn clear_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter.update(cx, |s, cx| s.set_value("", window, cx));
        self.model.update(cx, |m, cx| m.apply_filter("", cx));
    }

    /// The WHERE row of a browse tab, with the refused filter's error under it.
    fn filter_row(&self, cx: &Context<Self>) -> AnyElement {
        let dim = theme::dim(cx);
        let theme = cx.theme();
        let (border, accent, danger, muted) = (
            theme.border,
            theme.primary,
            theme.danger,
            theme.muted_foreground,
        );
        let (mono, mono_size, raised) = (
            theme.mono_font_family.clone(),
            theme.mono_font_size,
            theme.popover,
        );
        let error = self.model.read(cx).filter_error().map(|e| {
            div()
                .id("browse-filter-error")
                .aria_label(e.to_owned())
                .test_support()
                .flex_none()
                .px_3()
                .py_1()
                .border_b_1()
                .border_color(border)
                .text_xs()
                .text_color(danger)
                .child(e.to_owned())
        });
        let row = div()
            .id("browse-filter")
            .test_support()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .h(px(32.))
            .px_3()
            .bg(raised)
            .border_b_1()
            .border_color(border)
            .child(ui::icon(ui::Icons::Funnel, px(14.), dim))
            .child(
                div()
                    .id("browse-filter-where")
                    .aria_label("WHERE")
                    .test_support()
                    .flex_none()
                    .font_family(mono.clone())
                    .text_size(mono_size)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(accent)
                    .child("WHERE"),
            )
            .child(
                div()
                    .id("browse-filter-input")
                    .test_support()
                    .flex_1()
                    .min_w_0()
                    .child(
                        // Borderless, in the grid's mono, so the bar reads as one raised strip.
                        Input::new(&self.filter)
                            .appearance(false)
                            .font_family(mono)
                            .text_size(mono_size),
                    ),
            )
            // Clearing an empty filter does nothing, so the X shows only once there is text.
            .when(!self.filter.read(cx).value().is_empty(), |d| {
                d.child(
                    Button::new("browse-filter-clear")
                        .ghost()
                        .xsmall()
                        .child(ui::icon(ui::Icons::X, px(12.), muted))
                        .tooltip("Clear filter")
                        .on_click(cx.listener(|this, _, window, cx| this.clear_filter(window, cx))),
                )
            });
        div()
            .flex()
            .flex_col()
            .flex_none()
            .child(row)
            .children(error)
            .into_any_element()
    }

    /// Copies the grid's selection to the clipboard and returns the text, or `None` (leaving
    /// the clipboard alone) when nothing copyable is selected. Inserted rows have no loaded
    /// values, so a selection on one copies nothing.
    pub fn copy_selection(&mut self, cx: &mut Context<Self>) -> Option<String> {
        self.copy_selection_as(&CopyFormat::Tsv, false, cx)
    }

    /// `copy_selection` in `format`, with a header line for TSV and CSV when `headers`.
    fn copy_selection_as(
        &mut self,
        format: &CopyFormat,
        headers: bool,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let (store, sel) = self.loaded_store(cx)?;
        let text = copy_as(store, sel, format, headers)?;
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        Some(text)
    }

    /// The shown result's store and the selection on its loaded rows; inserted rows have none.
    fn loaded_store<'a>(&self, cx: &'a App) -> Option<(&'a ResultStore, TableSelection)> {
        let stmt = self.table.read(cx).delegate().stmt;
        let m = self.model.read(cx);
        let inserted = if stmt == 0 { m.edits().inserted() } else { 0 };
        let store = &m.results().get(stmt)?.store;
        Some((store, loaded_selection(self.selection(cx), inserted)?))
    }

    /// Copies the selection as INSERTs into its result's single table, each column under the
    /// table column it reads and the rest left out. Postgres names those through the session,
    /// so that copy lands once it answers; a result kept from before a connection switch or
    /// reconnect keeps its own column names. A notice says why nothing was copied.
    pub fn copy_selection_as_insert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let stmt = self.table.read(cx).delegate().stmt;
        let m = self.model.read(cx);
        let Some((engine, table)) = insert_table(m, stmt) else {
            return;
        };
        let Some(result) = m.results().get(stmt) else {
            return;
        };
        let columns: Arc<[ColumnInfo]> = result.store.columns().into();
        let run = m.run_id();
        // Only the session the result ran on can name its sources: another connection's
        // catalog reads the same OIDs as other tables, so the result's own labels stand.
        let names = named_source_columns(&table, &columns).or_else(|| {
            (result.epoch != m.epoch())
                .then(|| columns.iter().map(|c| Some(c.name().to_owned())).collect())
        });
        if let Some(names) = names {
            self.finish_insert_copy((run, stmt), engine, table, names, window, cx);
            return;
        }
        let asked = self
            .model
            .update(cx, |m, cx| m.source_columns(table.clone(), columns, cx));
        let answer = match asked {
            Ok(answer) => answer,
            Err(e) => return insert_copy_failed(&e, window, cx),
        };
        cx.spawn_in(window, async move |this, cx| {
            // The runner drops a request that meets a run in progress.
            let named =
                (answer.recv().await).unwrap_or_else(|_| Err("wait for the run to finish".into()));
            let _ = this.update_in(cx, |p, window, cx| match named {
                Ok(names) => p.finish_insert_copy((run, stmt), engine, table, names, window, cx),
                Err(e) => insert_copy_failed(&e, window, cx),
            });
        })
        .detach();
    }

    /// Copies the selection of `shown` (run id, statement) into `table` with result columns
    /// written as `columns` names them; a newer run or another statement shown meanwhile copies
    /// nothing, and a selection with no column of the table, or holding a value with no SQL
    /// literal, gives a notice instead.
    fn finish_insert_copy(
        &mut self,
        shown: (u64, usize),
        engine: Engine,
        table: RelationRef,
        columns: Vec<Option<String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if (
            self.model.read(cx).run_id(),
            self.table.read(cx).delegate().stmt,
        ) != shown
        {
            return;
        }
        let target = insert_target(engine, &table);
        let format = CopyFormat::Insert {
            engine,
            table,
            columns,
        };
        // A value with no literal refuses the whole copy rather than leave rows out of it.
        let refusal = self.loaded_store(cx).and_then(|(store, sel)| {
            if insert_maps_nothing(store, sel, &format) {
                Some(format!("no selected column comes from {target}"))
            } else {
                insert_unwritable(store, sel, &format)
                    .map(|col| format!("{col} holds a value with no SQL literal"))
            }
        });
        if let Some(reason) = refusal {
            let text = format!("Nothing copied as SQL INSERT: {reason}");
            ui::notice(
                window,
                cx,
                ui::Notice {
                    text,
                    kind: ui::NoticeKind::Warning,
                },
            );
            return;
        }
        self.copy_selection_as(&format, false, cx);
    }

    /// Asks for a file in the workspace folder and writes result `stmt`'s loaded rows there,
    /// as the file's extension asks (`export::export_format`), off the main thread; a notice
    /// says how it went. Nothing re-runs, and a run that replaced the rows meanwhile fails it.
    fn export_result(&mut self, stmt: usize, window: &mut Window, cx: &mut Context<Self>) {
        let m = self.model.read(cx);
        if m.results()
            .get(stmt)
            .is_none_or(|r| r.store.columns().is_empty())
        {
            return;
        }
        let name = insert_table(m, stmt).map_or_else(
            || "result.csv".to_owned(),
            |(_, t)| format!("{}.csv", t.name()),
        );
        let run = m.run_id();
        let reply = cx.prompt_for_new_path(m.spec().workspace.dir(), Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            // A dialog that fails says so, like a failed write; a cancelled one says nothing.
            let path = match chosen_path(reply.await) {
                Ok(Some(path)) => path,
                Ok(None) => return,
                Err(text) => {
                    let notice = ui::Notice {
                        text,
                        kind: ui::NoticeKind::Warning,
                    };
                    let _ = this.update_in(cx, |_, window, cx| ui::notice(window, cx, notice));
                    return;
                }
            };
            // Only the snapshot is taken here; rendering a large result would stall the window.
            let Ok(snapshot) = this.update(cx, |this, cx| {
                let m = this.model.read(cx);
                let store = &m.results().get(stmt).filter(|_| m.run_id() == run)?.store;
                Some(store.snapshot())
            }) else {
                return;
            };
            let written = match snapshot {
                Some(store) => {
                    let (to, rows) = (path.clone(), store.rows());
                    cx.background_spawn(async move {
                        write_export(&to, &export_text(&store, export_format(&to)))
                    })
                    .await
                    .map(|()| rows)
                    .map_err(|e| e.to_string())
                }
                None => Err("the result changed while choosing the file".to_owned()),
            };
            let file = path.file_name().map_or_else(
                || path.display().to_string(),
                |f| f.to_string_lossy().into_owned(),
            );
            let notice = match written {
                Ok(rows) => ui::Notice {
                    text: format!("Exported {} to {file}", format_rows(rows)),
                    kind: ui::NoticeKind::Success,
                },
                Err(e) => ui::Notice {
                    text: format!("Export failed: {e}"),
                    kind: ui::NoticeKind::Warning,
                },
            };
            let _ = this.update_in(cx, |_, window, cx| ui::notice(window, cx, notice));
        })
        .detach();
    }

    /// The run and inserted-row count that fix which row each table row shows.
    fn row_layout(&self, cx: &Context<Self>) -> (u64, usize) {
        let m = self.model.read(cx);
        (m.run_id(), m.edits().inserted())
    }

    /// Capture-phase mouse down on the grid: clears the menu target, and on a right-click
    /// opens the grid menu once the cell and header handlers have aimed it.
    fn grid_mouse_down(&mut self, e: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.menu_target.set(GridTarget::Below);
        if e.button != MouseButton::Right {
            return;
        }
        // The kit's table menus own the bubble phase and build an empty, focus-taking menu a
        // defer later, so ours opens two defers out and takes focus last.
        // A menu opened over an open one restores what the first saved, never the dead menu.
        let open = self.grid_popup.as_ref().filter(|(menu, ..)| {
            let focus = menu.focus_handle(cx);
            focus.is_focused(window) || focus.contains_focused(window, cx)
        });
        let prior = match open {
            Some((_, _, saved, _)) => saved.clone(),
            None => window.focused(cx),
        };
        let at = e.position;
        let weak = cx.entity().downgrade();
        window.defer(cx, move |window, cx| {
            window.defer(cx, move |window, cx| {
                Self::show_grid_menu(weak, at, prior, window, cx);
            });
        });
    }

    /// Builds the grid menu for the current target outside any pane borrow (the builder
    /// reads the pane) and shows it at `at`, restoring `prior` focus on a plain dismiss.
    fn show_grid_menu(
        weak: WeakEntity<Self>,
        at: Point<Pixels>,
        prior: Option<FocusHandle>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let menu = PopupMenu::build(window, cx, {
            let weak = weak.clone();
            move |menu, window, cx| grid_menu(menu, &weak, window, cx)
        });
        if menu.read(cx).is_empty() {
            return;
        }
        let _ = weak.update(cx, |p, cx| {
            let saved = prior.clone();
            let sub = cx.subscribe_in(
                &menu,
                window,
                move |p, menu, _: &DismissEvent, window, cx| {
                    if p.grid_popup.as_ref().is_some_and(|(m, ..)| m == menu) {
                        p.grid_popup = None;
                    }
                    let lost =
                        window.focused(cx).is_none() || menu.focus_handle(cx).is_focused(window);
                    if let (true, Some(prior)) = (lost, prior.as_ref()) {
                        window.focus(prior, cx);
                    }
                    cx.notify();
                },
            );
            menu.focus_handle(cx).focus(window, cx);
            p.grid_popup = Some((menu, at, saved, sub));
            cx.notify();
        });
    }

    /// Opens the context menu where the right-click landed: selects that cell or column and
    /// returns the target with the menu's sections (`menu_sections`).
    fn open_menu(&mut self, cx: &mut Context<Self>) -> (MenuTarget, Vec<MenuSection>) {
        let grid = self.menu_target.get();
        let target = MenuTarget {
            grid,
            layout: self.row_layout(cx),
        };
        // The target counts every column; the kit selects by visible ones.
        match grid {
            GridTarget::Cell(row, col) => {
                if let Some(ix) = self.table_col(col, cx) {
                    self.table
                        .update(cx, |t, cx| t.set_selected_cell(row, ix, cx));
                }
            }
            GridTarget::Header(col) => {
                if let Some(ix) = self.table_col(col, cx) {
                    self.table.update(cx, |t, cx| t.set_selected_col(ix, cx));
                }
            }
            GridTarget::Below => {}
        }
        let stmt = self.table.read(cx).delegate().stmt;
        (target, menu_sections(self.model.read(cx), stmt, grid, cx))
    }

    /// Runs menu item `id` on `target`, after staging an open editor; nothing runs once a
    /// re-run or a changed inserted-row count has moved the rows under the open menu.
    fn menu_action(
        &mut self,
        id: &str,
        target: MenuTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finish_edit(true, false, window, cx);
        if self.row_layout(cx) != target.layout {
            return;
        }
        if let GridTarget::Header(col) = target.grid {
            if let Some(data) = data_col(col) {
                self.header_action(id, data, window, cx);
            }
            if id.starts_with("header-menu-") {
                return;
            }
        }
        // A cell filter ANDs onto the applied filter, never onto unapplied WHERE-bar text,
        // and runs through the discard guard.
        if id.starts_with("grid-menu-filter-") {
            let m = self.model.read(cx);
            let stmt = self.table.read(cx).delegate().stmt;
            if let Some(clause) = filter_clause(m, stmt, target.grid, id) {
                let applied = m
                    .browse()
                    .map(|b| (b.target().engine(), b.browse().filter.clone()));
                if let Some(filter) = applied
                    .and_then(|(engine, applied)| and_filter(engine, applied.as_deref(), &clause))
                {
                    self.model.update(cx, |m, cx| m.apply_filter(&filter, cx));
                }
            }
            return;
        }
        let target = match target.grid {
            GridTarget::Cell(row, col) => Some((row, col)),
            _ => None,
        };
        let at = target.map(|(row, _)| self.model.read(cx).grid_row(row));
        let col = target.and_then(|(_, col)| data_col(col));
        let model = self.model.clone();
        match id {
            // Deferred past the menu's dismiss, which would focus the grid over the editor.
            "grid-menu-edit" => {
                if let Some((row, col)) = target {
                    cx.defer_in(window, move |this, window, cx| {
                        this.begin_edit(row, col, window, cx);
                    });
                }
            }
            "grid-menu-set-null" => {
                if let (Some(at), Some(col)) = (at, col) {
                    model.update(cx, |m, cx| m.set_value(at, col, EditValue::Null, cx));
                }
            }
            "grid-menu-set-default" => {
                if let (Some(at), Some(col)) = (at, col) {
                    model.update(cx, |m, cx| m.set_value(at, col, EditValue::Default, cx));
                }
            }
            "grid-menu-add-row" => {
                let ix = model.update(cx, |m, cx| m.add_row(cx));
                self.select_inserted(ix, window, cx);
            }
            "grid-menu-duplicate-row" => {
                let ix = at.and_then(|at| model.update(cx, |m, cx| m.duplicate_row(at, cx)));
                self.select_inserted(ix, window, cx);
            }
            "grid-menu-delete-row" => {
                if let Some(at) = at {
                    model.update(cx, |m, cx| m.delete_at(at, cx));
                }
            }
            "grid-menu-revert-row" => {
                if let Some(at) = at {
                    model.update(cx, |m, cx| m.revert_row(at, cx));
                }
            }
            // The format copies act on the selection `open_menu` made at the target.
            "grid-menu-copy" => {
                self.copy_selection(cx);
            }
            "grid-menu-copy-headers" => {
                self.copy_selection_as(&CopyFormat::Tsv, true, cx);
            }
            "grid-menu-copy-csv" => {
                self.copy_selection_as(&CopyFormat::Csv, true, cx);
            }
            "grid-menu-copy-json" => {
                self.copy_selection_as(&CopyFormat::Json, false, cx);
            }
            "grid-menu-copy-markdown" => {
                self.copy_selection_as(&CopyFormat::Markdown, false, cx);
            }
            "grid-menu-copy-sql" => self.copy_selection_as_insert(window, cx),
            "grid-menu-export" => {
                let stmt = self.table.read(cx).delegate().stmt;
                self.export_result(stmt, window, cx);
            }
            copy => {
                let kind = match copy {
                    "grid-menu-copy-insert" => CopyKind::Insert,
                    "grid-menu-copy-update" => CopyKind::Update,
                    "grid-menu-copy-delete" => CopyKind::Delete,
                    _ => return,
                };
                if let Some(text) = at.and_then(|at| model.read(cx).copy_query(at, kind)) {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
        }
    }

    /// Runs header item `id` on data column `data` of the shown result: a browse sorts on the
    /// server through the discard guard, any other result sorts its loaded rows.
    fn header_action(
        &mut self,
        id: &str,
        data: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stmt = self.table.read(cx).delegate().stmt;
        let m = self.model.read(cx);
        let Some(name) = m
            .results()
            .get(stmt)
            .and_then(|r| r.store.columns().get(data))
            .map(|c| c.name().to_owned())
        else {
            return;
        };
        let browse = stmt == 0 && m.browse_result();
        let sort = match id {
            "header-menu-sort-asc" => Some(Some(false)),
            "header-menu-sort-desc" => Some(Some(true)),
            "header-menu-sort-clear" => Some(None),
            _ => None,
        };
        if let Some(sort) = sort {
            self.model.update(cx, |m, cx| {
                if browse {
                    m.set_sort(sort.map(|d| Sort::new(name, d)), cx);
                } else {
                    m.sort_loaded(stmt, data, sort, cx);
                }
            });
            return;
        }
        match id {
            "header-menu-copy-name" => cx.write_to_clipboard(ClipboardItem::new_string(name)),
            "header-menu-hide" => {
                self.model.update(cx, |m, cx| m.hide_column(stmt, data, cx));
            }
            "header-menu-show-all" => {
                self.model.update(cx, |m, cx| m.show_all_columns(stmt, cx));
            }
            "header-menu-fit" => {
                self.model.update(cx, |m, cx| m.fit_column(stmt, data, cx));
            }
            // Prefilled over the applied filter, not run; deferred past the menu's dismiss,
            // which would focus the grid over the input.
            "header-menu-filter" if browse => {
                let spec = m.spec();
                let Some(engine) = spec
                    .workspace
                    .connection(&spec.connection)
                    .map(|c| c.engine())
                else {
                    return;
                };
                let applied = m.browse().and_then(|b| b.browse().filter.clone());
                let clause = format!("{} = ", quote_ident(engine, &name));
                let Some(text) = and_filter(engine, applied.as_deref(), &clause) else {
                    return;
                };
                // The caret goes after the new clause, in front of a commented filter.
                let caret = if text.starts_with(&clause) {
                    clause.len()
                } else {
                    text.len()
                };
                cx.defer_in(window, move |this, window, cx| {
                    this.filter.update(cx, |s, cx| {
                        s.set_value(text, window, cx);
                        s.focus(window, cx);
                        s.set_selected_range(caret..caret, cx);
                    });
                });
            }
            _ => {}
        }
    }

    /// Selects inserted row `ix`'s first settable cell. `sync` runs first, since its later
    /// run on the new inserted count would otherwise clear this selection.
    fn select_inserted(&mut self, ix: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = ix else { return };
        self.sync(window, cx);
        let m = self.model.read(cx);
        let columns = m.results().first().map_or(0, |r| r.store.columns().len());
        let col = (0..columns)
            .find(|&c| {
                m.settable(GridRow::Inserted(ix), c).is_some()
                    && self.table_col(c + 1, cx).is_some()
            })
            .unwrap_or(0);
        let col = self.table_col(col + 1, cx).unwrap_or(1);
        self.table
            .update(cx, |t, cx| t.set_selected_cell(ix, col, cx));
    }

    /// Shows result `ix` in the grid until the next run.
    fn select(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(ix);
        self.sync(window, cx);
    }

    /// Closes the record form's field editor unstaged: after rows shift or a new result
    /// lands, its row would address different data.
    fn drop_record_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.record
            .update(cx, |f, cx| f.finish_edit(false, window, cx));
    }

    /// Closes the editor without staging; the dropped input is no longer rendered, so the
    /// grid takes its focus back, or arrows, F2, Delete and Ctrl+S would reach nothing.
    fn drop_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editor_focused = self
            .cell_editor
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        if self.editing.take().is_some() && editor_focused {
            self.table.read(cx).focus_handle(cx).focus(window, cx);
        }
    }

    /// Rebuilds the table's columns only when a different result (or a result's columns)
    /// becomes the displayed one; new rows need just a render, since row counts are read live.
    /// A change in the inserted-row count shifts every loaded row, so it drops the editor and
    /// the selection.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let inserted = self.model.read(cx).edits().inserted();
        if inserted != self.inserted {
            self.drop_record_edit(window, cx);
            self.follow_record_row(self.inserted, inserted, window, cx);
            self.inserted = inserted;
            self.drop_editor(window, cx);
            self.table.update(cx, |t, cx| {
                t.delegate_mut().selection = TableSelection::None;
                t.delegate_mut().editing = None;
                t.clear_selection(cx);
            });
        }
        let m = self.model.read(cx);
        // A pick belongs to the run it was made in; a new run falls back to `shown`.
        if self.built.is_some_and(|(run, ..)| run != m.run_id()) {
            self.selected = None;
        }
        // Rows arriving flips the last field once, which re-sizes columns from the first batch.
        let key = displayed(m.results(), self.selected).map(|ix| {
            let store = &m.results()[ix].store;
            (m.run_id(), ix, store.columns().len(), store.rows() > 0)
        });
        if key != self.built {
            self.built = key;
            // The note belongs to the result it was raised on; a run or another result drops it.
            self.readonly = None;
            self.drop_editor(window, cx);
            self.drop_record_edit(window, cx);
            let stmt = key.map_or(usize::MAX, |(_, ix, ..)| ix);
            self.table.update(cx, |t, cx| {
                t.delegate_mut().stmt = stmt;
                // The kit keeps its selection across a refresh; a new result starts unselected.
                t.delegate_mut().selection = TableSelection::None;
                t.delegate_mut().editing = None;
                t.clear_selection(cx);
                t.refresh(cx);
            });
        }
        // A hidden, shown, fitted or dragged column rebuilds the kit's columns; a changed set of
        // visible columns also moves what each selected index means, so it drops the selection.
        let view = self.column_view(cx).cloned();
        if view != self.built_view {
            let moved = visible_cols(view.as_ref()) != visible_cols(self.built_view.as_ref());
            self.built_view = view;
            if moved {
                self.drop_editor(window, cx);
            }
            self.table.update(cx, |t, cx| {
                if moved {
                    t.delegate_mut().selection = TableSelection::None;
                    t.delegate_mut().editing = None;
                    t.clear_selection(cx);
                }
                t.refresh(cx);
            });
        }
        // A local sort reorders the rows under an index-based selection, so it drops it.
        let sort = key.and_then(|(_, ix, ..)| self.model.read(cx).results()[ix].store.local_sort());
        if sort != self.built_sort {
            self.built_sort = sort;
            self.drop_editor(window, cx);
            self.table.update(cx, |t, cx| {
                t.delegate_mut().selection = TableSelection::None;
                t.delegate_mut().editing = None;
                t.clear_selection(cx);
            });
        }
        // The record view follows the displayed result and stays on one of its rows.
        if self.view == ResultView::Record {
            let stmt = key.map_or(0, |(_, ix, ..)| ix);
            let (_, row) = self.record.read(cx).shown();
            let rows = self.record_rows(stmt, cx);
            let row = if rows > 0 { row.min(rows - 1) } else { row };
            self.record
                .update(cx, |f, cx| f.show(stmt, row, window, cx));
        }
        self.sync_plan(window, cx);
        // A hop's mark waits for a later run's statement 0 to stop, then selects a lone row.
        let m = self.model.read(cx);
        let landed = self
            .select_single
            .filter(|&run| m.run_id() != run && !m.running());
        if landed.is_some() {
            let single = m.results().first().is_some_and(|r| r.store.rows() == 1);
            self.select_single = None;
            if single {
                self.select_row(0, window, cx);
            }
        }
        cx.notify();
    }

    /// Builds the plan view when the body's result is a pending or planned explain, again when
    /// its output lands, and drops it for any other result.
    fn sync_plan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let m = self.model.read(cx);
        let results = m.results();
        let subject = body_subject(results, self.selected).and_then(|ix| {
            let r = &results[ix];
            let plan = r.plan.as_ref()?;
            matches!(r.store.end(), None | Some(StatementEnd::Planned)).then_some((ix, plan))
        });
        let Some((ix, plan)) = subject else {
            self.plan = None;
            return;
        };
        let key = (m.run_id(), ix, plan.output.is_some());
        if self.plan.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        let plan = plan.clone();
        let view = cx.new(|cx| PlanView::new(&plan, window, cx));
        self.plan = Some((key, view));
    }

    /// Keeps the record view on its grid row when the inserted-row count goes from `old` to
    /// `new`, or on table row 0 when that row is gone (an inserted row undone or reverted).
    fn follow_record_row(
        &mut self,
        old: usize,
        new: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (stmt, row) = self.record.read(cx).shown();
        if self.view != ResultView::Record || stmt != 0 || !self.model.read(cx).browse_result() {
            return;
        }
        let row = match staged::grid_row(row, old) {
            GridRow::Inserted(ix) if ix >= new => 0,
            at => at.table_row(new),
        };
        self.record
            .update(cx, |f, cx| f.show(stmt, row, window, cx));
    }
}

impl ResultsPane {
    /// Why the grid is empty, over its body: one row below the header when the result has
    /// columns, else centred; a filtered browse adds Clear filter.
    fn empty_overlay(
        &self,
        text: String,
        header_shown: bool,
        clearable: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let clear = clearable.then(|| {
            Button::new("result-clear-filter")
                .ghost()
                .xsmall()
                .label("Clear filter")
                .on_click(cx.listener(|this, _, window, cx| this.clear_filter(window, cx)))
        });
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .top(if header_shown {
                settings::active_metrics(cx).grid_row
            } else {
                px(0.)
            })
            .flex()
            .flex_col()
            .items_center()
            .gap_2()
            .when(header_shown, |d| d.pt_4())
            .when(!header_shown, |d| d.justify_center())
            .text_sm()
            .text_color(theme::dim(cx))
            .child(
                div()
                    .id("result-empty")
                    .role(Role::Status)
                    .aria_label(text.clone())
                    .test_support()
                    .child(text),
            )
            .children(clear)
            .into_any_element()
    }

    /// The failed statement's error in place of the grid: heading and code, message,
    /// detail, hint and statement, with Copy and (given a position) Jump to error.
    /// The code is labelled by `engine`, the one the statement ran on.
    fn error_body(&self, v: ErrorView, engine: Option<Engine>, cx: &Context<Self>) -> AnyElement {
        let (dim, theme) = (theme::dim(cx), cx.theme());
        let (danger, fg, border) = (theme.danger, theme.foreground, theme.border);
        let (mono, mono_size) = (theme.mono_font_family.clone(), theme.mono_font_size);
        // Labels read by tests and assistive tech carry the shown text, as `footer-rows` does.
        let label = |id: &'static str, text: String| {
            ui::accessibility_id(div().id(id), id)
                .role(Role::Label)
                .aria_value(text.clone())
                .aria_label(text.clone())
                .test_support()
                .child(text)
        };
        let code = code_label(engine, &v);
        let line = |id: &'static str, name: &'static str, text: Option<String>| {
            text.map(|t| {
                div()
                    .flex()
                    .gap_2()
                    .child(div().flex_none().text_color(dim).child(name))
                    .child(label(id, t).min_w_0())
            })
        };
        let copy_text = error_copy_text(&v);
        let jump = v.position.map(|_| {
            ui::accessibility_id(Button::new("result-error-jump"), "result-error-jump")
                .ghost()
                .xsmall()
                .label("Jump to error")
                .disabled(self.jump_offset(cx).is_none())
                .on_click(cx.listener(|this, _, window, cx| this.jump_to_error(window, cx)))
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .overflow_hidden()
            .text_sm()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(ui::icon(ui::Icons::CircleX, px(16.), danger))
                    .child(
                        label("result-error", "Query failed".into())
                            .text_color(danger)
                            .font_weight(FontWeight::SEMIBOLD),
                    )
                    .children(code.map(|c| {
                        label("result-error-code", c)
                            .font_family(mono.clone())
                            .text_size(mono_size)
                            .text_color(dim)
                    }))
                    .child(div().flex_1())
                    .child(
                        ui::accessibility_id(Button::new("result-error-copy"), "result-error-copy")
                            .ghost()
                            .xsmall()
                            .icon(ui::Icons::Copy)
                            .label("Copy")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()))
                            }),
                    )
                    .children(jump),
            )
            .child(label("result-error-message", v.message.clone()).text_color(fg))
            .children(line("result-error-detail", "Detail", v.detail.clone()))
            .children(line("result-error-hint", "Hint", v.hint.clone()))
            .child(
                label("result-error-sql", v.sql.clone())
                    .p_2()
                    .border_1()
                    .border_color(border)
                    .rounded_md()
                    .font_family(mono)
                    .text_size(mono_size)
                    .text_color(dim),
            )
            .into_any_element()
    }

    /// The edit strip at the statement list's right end on a browse tab: why editing is
    /// blocked, or Row, Duplicate, Delete, Undo, Redo, the staged count, Revert and Save,
    /// each disabled by `strip_actions` when it does not apply.
    fn edit_strip(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let m = self.model.read(cx);
        m.browse()?;
        let strip = div()
            .id("edit-strip")
            .test_support()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .px_2();
        if let Some(block) = m.edit_block() {
            let chip =
                ui::chip("browse-not-editable", block.reason(), cx).text_color(theme::dim(cx));
            return Some(strip.child(chip).into_any_element());
        }
        let can = self.strip_actions(cx);
        let (n, state) = (m.edits().len(), m.save_state());
        let pending = theme::edit_pending(cx);
        // A disabled Save fades its fill, so its keys take the neutral chip colours menus use.
        let chip_style = if can.save {
            ChoiceStyle::Primary
        } else {
            ChoiceStyle::Neutral
        };
        let (key_text, key_edge) = confirm::chip_colours(chip_style, cx);
        let revert_model = self.model.clone();
        // Row edits run the grid menu's own actions on the selected row.
        let row_button =
            |id: &'static str, menu: &'static str, icon, tip: &'static str, on: bool| {
                Button::new(id)
                    .ghost()
                    .xsmall()
                    .icon(icon)
                    .tooltip(tip)
                    .disabled(!on)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let target = MenuTarget {
                            grid: this
                                .strip_cell(cx)
                                .map_or(GridTarget::Below, |(r, c)| GridTarget::Cell(r, c)),
                            layout: this.row_layout(cx),
                        };
                        this.menu_action(menu, target, window, cx);
                    }))
            };
        Some(
            strip
                .child(
                    row_button(
                        "edit-strip-add",
                        "grid-menu-add-row",
                        ui::Icons::Plus,
                        "Add row",
                        can.add,
                    )
                    .label("Row"),
                )
                .child(row_button(
                    "edit-strip-duplicate",
                    "grid-menu-duplicate-row",
                    ui::Icons::CopyPlus,
                    "Duplicate row",
                    can.duplicate,
                ))
                .child(row_button(
                    "edit-strip-delete",
                    "grid-menu-delete-row",
                    ui::Icons::Trash,
                    "Delete row",
                    can.delete,
                ))
                .child(self.history_button(false, can.undo, cx))
                .child(self.history_button(true, can.redo, cx))
                .when(n > 0, |d| {
                    d.child(
                        ui::chip("pending-changes", pending_label(n), cx)
                            .bg(pending.fill)
                            .border_color(pending.border)
                            .text_color(pending.chip_text),
                    )
                })
                .child(
                    Button::new("revert-edits")
                        .ghost()
                        .xsmall()
                        .label("Revert")
                        .disabled(!can.revert)
                        .on_click(move |_, _, cx| {
                            revert_model.update(cx, |m, cx| m.revert_edits(cx))
                        }),
                )
                .child(
                    // Sized like the pending chip beside it, so the 18 px key boxes fit inside.
                    Button::new("save-edits")
                        .primary()
                        .xsmall()
                        .h(px(22.))
                        .px_2()
                        .icon(ui::Icons::Save)
                        .label(save_label(state))
                        .children(
                            keys_for(&SaveEdits, cx).map(|k| {
                                ui::key_chip_colored(chip_keys(&k), key_text, key_edge, cx)
                            }),
                        )
                        .disabled(!can.save)
                        // Like Ctrl+S, an open editor's text is staged before the save starts.
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.stage_open_editors(window, cx);
                            this.model.update(cx, |m, cx| m.save(cx));
                        })),
                )
                .into_any_element(),
        )
    }

    /// The ghost `undo-edits` or (`redo`) `redo-edits` icon button with its keys in the
    /// tooltip; disabled unless `enabled` (`strip_actions`, which waits for open editors).
    fn history_button(&self, redo: bool, enabled: bool, cx: &Context<Self>) -> AnyElement {
        let (id, tip, icon, keys) = if redo {
            (
                "redo-edits",
                "Redo",
                ui::Icons::Redo2,
                keys_for(&RedoEdit, cx),
            )
        } else {
            (
                "undo-edits",
                "Undo",
                ui::Icons::Undo2,
                keys_for(&UndoEdit, cx),
            )
        };
        // The icon is a 14 px child in the affordance colour; disabled, it fades like the
        // kit's own disabled text.
        let color = theme::affordance(cx);
        let color = if enabled { color } else { color.opacity(0.5) };
        let glyph = div()
            .id(SharedString::from(format!("{id}-icon")))
            .test_support()
            .size(px(14.))
            .child(ui::icon(icon, px(14.), color));
        // The kit button takes only a text tooltip, so the keyed one sits on a wrapper.
        div()
            .id(SharedString::from(format!("{id}-tip")))
            .tooltip(move |window, cx| theme::tooltip_keys(tip, keys.clone(), window, cx))
            .child(
                Button::new(id)
                    .ghost()
                    .xsmall()
                    .child(glyph)
                    .disabled(!enabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.any_editor_open(cx) {
                            this.model
                                .update(cx, |m, cx| if redo { m.redo(cx) } else { m.undo(cx) });
                        }
                    })),
            )
            .into_any_element()
    }

    /// The strip's Export button over the displayed result; disabled, like the menu's Export,
    /// while it has no rows.
    fn export_button(&self, cx: &Context<Self>) -> AnyElement {
        let stmt = self.displayed(cx);
        let rows = stmt
            .and_then(|ix| self.model.read(cx).results().get(ix))
            .is_some_and(|r| r.store.rows() > 0);
        Button::new("result-export")
            .ghost()
            .xsmall()
            .icon(ui::Icons::Download)
            .tooltip("Export")
            .disabled(!rows)
            .on_click(cx.listener(move |this, _, window, cx| {
                if let Some(stmt) = stmt {
                    this.export_result(stmt, window, cx);
                }
            }))
            .into_any_element()
    }

    /// The record view's footer group: its position, then previous and next; None without
    /// a row to show.
    fn record_nav(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let (stmt, row) = self.record.read(cx).shown();
        let rows = self.record_rows(stmt, cx);
        if row >= rows {
            return None;
        }
        let m = self.model.read(cx);
        let loaded = m.results().get(stmt).map_or(0, |r| r.store.rows());
        let text = record_position(record_at(m, stmt, row), loaded);
        let dim = theme::dim(cx);
        let step = |id: &'static str, icon, delta: isize, can: bool| {
            Button::new(id)
                .ghost()
                .xsmall()
                .icon(icon)
                .disabled(!can)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.step_record(delta, window, cx);
                    this.record.read(cx).focus_handle(cx).focus(window, cx);
                }))
        };
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .id("record-position")
                        .aria_label(text.clone())
                        .test_support()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_size(cx.theme().mono_font_size)
                        .text_color(dim)
                        .child(text),
                )
                .child(step("record-prev", ui::Icons::ChevronLeft, -1, row > 0))
                .child(step(
                    "record-next",
                    ui::Icons::ChevronRight,
                    1,
                    row + 1 < rows,
                ))
                .into_any_element(),
        )
    }

    /// The footer's row-view pair: `inspector-toggle` opens the shell's dock (keys in its
    /// tooltip), `row-view-expand` swaps the grid for the record view; each is accented while
    /// on.
    fn row_view_buttons(&self, cx: &Context<Self>) -> AnyElement {
        let (accent, idle) = (cx.theme().primary, theme::affordance(cx));
        let glyph = |id: &'static str, icon, on: bool| {
            div().id(id).test_support().size(px(14.)).child(ui::icon(
                icon,
                px(14.),
                if on { accent } else { idle },
            ))
        };
        let expanded = self.view == ResultView::Record;
        // The kit button takes only a text tooltip, so the keyed one sits on a wrapper.
        let toggle = div()
            .id("inspector-toggle-tip")
            .tooltip(|window, cx| {
                let keys = keys_for(&ToggleRowInspector, cx);
                theme::tooltip_keys("Row inspector", keys, window, cx)
            })
            .child(
                Button::new("inspector-toggle")
                    .ghost()
                    .xsmall()
                    .accessibility_label("Row inspector")
                    .toggled(self.inspector_open)
                    .child(glyph(
                        "inspector-toggle-icon",
                        ui::Icons::PanelRight,
                        self.inspector_open,
                    ))
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(ToggleRowInspector), cx)
                    }),
            );
        let expand = Button::new("row-view-expand")
            .ghost()
            .xsmall()
            .accessibility_label("Expand row")
            .tooltip(if expanded {
                "Back to grid"
            } else {
                "Expand row"
            })
            .toggled(expanded)
            .child(glyph("row-view-expand-icon", ui::Icons::FileText, expanded))
            .on_click(cx.listener(move |this, _, window, cx| {
                let view = if expanded {
                    ResultView::Grid
                } else {
                    ResultView::Record
                };
                this.set_view(view, window, cx);
            }));
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .child(toggle)
            .child(expand)
            .into_any_element()
    }

    /// The read-only note under the strip, with "Open <table> to edit" when the result's SQL
    /// reads one relation and the catalog says it is a table.
    fn readonly_note(&self, cx: &Context<Self>) -> Option<AnyElement> {
        self.readonly.as_ref()?;
        let dim = theme::dim(cx);
        let border = cx.theme().border;
        let open = self.offer(cx).map(|rel| {
            Button::new("readonly-open")
                .ghost()
                .xsmall()
                .label(format!("Open {} to edit", rel.name()))
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(OpenTable(rel.clone()))))
        });
        Some(
            div()
                .id("readonly-note")
                .role(Role::Status)
                .aria_label("Query results are read-only.")
                .test_support()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .px_3()
                .py_1()
                .border_b_1()
                .border_color(border)
                .text_xs()
                .text_color(dim)
                .child(ui::icon(ui::Icons::Lock, px(12.), dim))
                .child("Query results are read-only.")
                .children(open)
                .into_any_element(),
        )
    }

    /// Why the last save committed nothing, under the strip; styled like the filter's error.
    /// With conflicting rows it offers to save the rest without them, or to reload them.
    fn edit_banner(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let m = self.model.read(cx);
        let text = m.edit_banner()?.to_owned();
        let conflicts = !m.conflicts().is_empty();
        let reload = conflicts && m.browse().is_some_and(|b| b.target().stable_key());
        let reload_disabled = !m.can_reload_conflicts();
        let (drop_model, reload_model) = (self.model.clone(), self.model.clone());
        // Outlined like the reconnect banner's action: a dialog's secondary fill vanishes on the
        // light pane, while the input-colored border reads as a button in both themes.
        let drop = conflicts.then(|| {
            Button::new("edit-banner-drop")
                .outline()
                .xsmall()
                .label("Save the rest, drop conflicting rows")
                .on_click(move |_, _, cx| {
                    drop_model.update(cx, |m, cx| m.drop_conflicts(cx));
                })
        });
        let reload = reload.then(|| {
            Button::new("edit-banner-reload")
                .outline()
                .xsmall()
                .label("Reload these rows")
                .disabled(reload_disabled)
                .on_click(move |_, _, cx| {
                    reload_model.update(cx, |m, cx| m.reload_conflicts(cx));
                })
        });
        let theme = cx.theme();
        let (danger, border) = (theme.danger, theme.border);
        Some(
            div()
                .id("edit-banner")
                .aria_label(text.clone())
                .test_support()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .px_3()
                .py_1()
                .border_b_1()
                .border_color(border)
                .child(ui::icon(ui::Icons::CircleX, px(12.), danger))
                .child(div().text_xs().text_color(danger).child(text))
                .children(drop)
                .children(reload)
                .into_any_element(),
        )
    }

    /// Committed rows the filter no longer matches, muted above the grid, the edit banner's layout.
    fn kept_band(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let kept = self.model.read(cx).kept_rows();
        if kept.is_empty() {
            return None;
        }
        let (dim, border) = (theme::dim(cx), cx.theme().border);
        let rows = kept
            .iter()
            .take(staged::KEPT_SHOWN)
            .enumerate()
            .map(|(i, k)| {
                div()
                    .id(("kept-row", i))
                    .aria_label(k.label.clone())
                    .test_support()
                    .text_xs()
                    .text_color(dim)
                    .child(k.label.clone())
            });
        let more = staged::kept_more(kept.len()).map(|text| {
            div()
                .id("kept-more")
                .aria_label(text.clone())
                .test_support()
                .text_xs()
                .text_color(dim)
                .child(text)
        });
        Some(
            div()
                .id("kept-rows")
                .test_support()
                .flex()
                .flex_none()
                .gap_2()
                .px_3()
                .py_1()
                .border_b_1()
                .border_color(border)
                .child(
                    div()
                        .pt(px(2.))
                        .child(ui::icon(ui::Icons::Funnel, px(12.), dim)),
                )
                .child(div().flex().flex_col().children(rows).children(more))
                .into_any_element(),
        )
    }
}

impl Render for ResultsPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.keep_ticking(cx);
        // The binding lives on the DataTable context inside this root, so the action bubbles here.
        let (dim, fill) = (theme::dim(cx), theme::editor_fill(cx));
        let pending_text = theme::edit_pending(cx).chip_text;
        let theme = cx.theme();
        let (danger, warning, border, accent) =
            (theme.danger, theme.warning, theme.border, theme.primary);
        let (fg, muted, hover) = (
            theme.foreground,
            theme.muted_foreground,
            theme.secondary_hover,
        );
        let (mono, mono_size) = (theme.mono_font_family.clone(), theme.mono_font_size);
        let filter_row = self
            .model
            .read(cx)
            .browse()
            .is_some()
            .then(|| self.filter_row(cx));
        let (edit_strip, banner) = (self.edit_strip(cx), self.edit_banner(cx));
        let kept = self.kept_band(cx);
        let export = self.export_button(cx);
        let readonly = self.readonly_note(cx);
        let m = self.model.read(cx);
        let pending =
            (m.browse().is_some() && !m.edits().is_empty()).then(|| pending_label(m.edits().len()));
        let results = m.results();
        let current = displayed(results, self.selected);
        // A park at the cap reads like a fetch-window pause on its tab.
        let (asking, paused) = (
            m.confirm().map(|p| p.stmt),
            m.paused().or(m.parked_at_cap()),
        );
        // Each tab is named by today's one-line summary; it shows only its number and
        // its mark (`result_mark`), and the tooltip carries the summary and the SQL.
        let tabs = results.iter().enumerate().map(|(ix, r)| {
            let mark = result_mark(r, asking, paused);
            let text = match mark {
                ResultMark::Waiting => "waiting for confirmation".to_owned(),
                ResultMark::Paused(n) => paused_label(n),
                _ => summary(r),
            };
            let tip: SharedString = match first_line(&r.sql) {
                Some(sql) => format!("{text}\n{sql}").into(),
                None => text.clone().into(),
            };
            let active = current == Some(ix);
            let count = |n: u64| {
                div()
                    .font_family(mono.clone())
                    .text_size(mono_size)
                    .text_color(dim)
                    .child(format_rows(n))
            };
            let word = |w: SharedString| div().text_color(dim).child(w);
            // A plan has no rows to count, so its tab carries only a failure or a wait.
            let shows_mark = r.plan.is_none() || !matches!(mark, ResultMark::Rows(_));
            let mark = match mark {
                ResultMark::Rows(n) => count(n).into_any_element(),
                ResultMark::Changed(text) => word(text.into()).into_any_element(),
                ResultMark::Failed => {
                    ui::icon(ui::Icons::CircleX, px(14.), danger).into_any_element()
                }
                ResultMark::Waiting => word("waiting".into()).into_any_element(),
                ResultMark::Paused(n) => div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(count(n))
                    .child(word("more available".into()))
                    .into_any_element(),
            };
            div()
                .id(("statement-result", ix))
                .aria_label(text)
                .test_support()
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .h_full()
                .px_3()
                .text_sm()
                .when(active, |d| {
                    d.bg(fill).text_color(fg).font_weight(FontWeight::SEMIBOLD)
                })
                .when(!active, |d| d.text_color(muted).hover(|s| s.bg(hover)))
                .on_click(cx.listener(move |this, _, window, cx| this.select(ix, window, cx)))
                .tooltip(move |window, cx| theme::tooltip(tip.clone(), window, cx))
                .child(match r.plan {
                    Some(_) => "Plan".to_owned(),
                    None => format!("Result {}", ix + 1),
                })
                .children(shows_mark.then_some(mark))
                .when(active, |d| {
                    d.child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom_0()
                            .h(px(2.))
                            .bg(accent),
                    )
                })
        });
        // The footer follows the grid; with no grid result it names the last statement, so
        // a failed or write-only run still shows its error or count.
        let body = result_body(results, self.selected);
        let failed = body == ResultBody::Failed;
        let dml = match &body {
            ResultBody::Dml(text) => Some(text.clone()),
            _ => None,
        };
        // The empty text never covers rows the user is inserting into an empty table.
        let filter = m.browse().and_then(|b| b.browse().filter.clone());
        let busy = m.running() || m.browses();
        let run_keys = keys_for(&RunStatement, cx);
        let empty = empty_text(
            results,
            self.selected,
            busy,
            filter.as_deref(),
            run_keys.as_deref(),
        )
        .filter(|_| body == ResultBody::Grid && m.edits().inserted() == 0);
        let plan_shown = self.plan.as_ref().map(|(_, v)| v.clone());
        let header_shown = body_subject(results, self.selected)
            .is_some_and(|ix| !results[ix].store.columns().is_empty());
        let clearable = !results.is_empty() && filter.is_some_and(|f| !f.is_empty());
        let error = body_subject(results, self.selected)
            .filter(|_| failed)
            .and_then(|ix| Some((error_view(&results[ix])?, results[ix].engine)));
        let footer = body_subject(results, self.selected).map(|ix| {
            let r = &results[ix];
            let cap = m.cap_view(r.stmt);
            let expired = match cap {
                Some(CapView::Ended { expired: Some(by) }) => Some(by),
                _ => None,
            };
            // An expired browse continues in place, so its footer offers that instead of a note.
            let continuing = (ix == 0)
                .then(|| m.continuation())
                .flatten()
                .and(m.run_limits().max_rows);
            let note = match r.store.end() {
                _ if continuing.is_some() => None,
                Some(StatementEnd::Truncated(_)) if expired.is_some() => {
                    expired.map(|by| (expiry_note(by), warning))
                }
                Some(StatementEnd::Truncated(o)) => {
                    Some((format!("truncated at {}", format_rows(o.rows)), warning))
                }
                Some(StatementEnd::Complete(o)) => outcome_note(o).map(|n| (n, dim)),
                Some(StatementEnd::Failed(e)) => Some((e.to_string(), danger)),
                _ => None,
            };
            let read_only = (m.read_only() == Some(true)).then(|| {
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .text_color(dim)
                    .child(ui::icon(ui::Icons::Lock, px(12.), dim))
                    .child("read-only")
            });
            // At the cap the count says more rows wait, and Fetch next and Fetch all continue
            // the stream; a park that expired offers the next row limit for a later run.
            let at_cap = match cap {
                Some(CapView::Parked { .. }) => Some(false),
                Some(CapView::Fetching) => Some(true),
                _ => None,
            };
            let rows = match at_cap {
                Some(_) => more_rows(r.store.rows()),
                None => format_rows(r.store.rows()),
            };
            let model = self.model.clone();
            let fetch = at_cap.map(|fetching| {
                let next = m.run_limits().max_rows.map(|limit| {
                    let model = model.clone();
                    let label = fetch_next_label(limit);
                    fetch_button("fetch-next", ui::Icons::ArrowDown, label, fg, fetching).on_click(
                        move |_, _, cx| {
                            model.update(cx, |m, cx| {
                                m.fetch_next();
                                cx.notify();
                            })
                        },
                    )
                });
                let model = model.clone();
                let all = fetch_button(
                    "fetch-all",
                    ui::Icons::Rows3,
                    "Fetch all".into(),
                    fg,
                    fetching,
                )
                .on_click(move |_, _, cx| {
                    model.update(cx, |m, cx| {
                        m.fetch_all();
                        cx.notify();
                    })
                });
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1()
                    .children(next)
                    .child(all)
            });
            let resume = continuing.map(|limit| {
                let model = model.clone();
                let label = continue_label(limit);
                fetch_button("fetch-next", ui::Icons::ArrowDown, label, fg, false).on_click(
                    move |_, _, cx| {
                        model.update(cx, |m, cx| {
                            m.continue_browse(cx);
                        })
                    },
                )
            });
            // A single expired query re-runs with the next limit; a script only raises it.
            let run_again = m.can_run_again();
            let raise = expired
                .filter(|_| continuing.is_none())
                .and_then(|_| raise_target(m.max_rows()))
                .filter(|_| matches!(r.store.end(), Some(StatementEnd::Truncated(_))))
                .map(|t| {
                    if run_again {
                        return Button::new("run-again")
                            .ghost()
                            .xsmall()
                            .label(run_again_label(t))
                            .on_click(move |_, _, cx| {
                                model.update(cx, |m, cx| {
                                    m.run_again(cx);
                                })
                            });
                    }
                    Button::new("raise-limit")
                        .ghost()
                        .xsmall()
                        .label(raise_label(t))
                        .on_click(move |_, _, cx| {
                            model.update(cx, |m, cx| {
                                m.set_max_rows(Some(t));
                                cx.notify();
                            })
                        })
                });
            let record_nav = (self.view() == ResultView::Record)
                .then(|| self.record_nav(cx))
                .flatten();
            let local_sort = r.store.local_sort().and_then(|(col, descending)| {
                let name = r.store.columns().get(col)?.name();
                let way = if descending {
                    "descending"
                } else {
                    "ascending"
                };
                let text = format!("Loaded rows sorted by {name} {way}");
                Some(
                    div()
                        .id("local-sort")
                        .role(Role::Status)
                        .aria_label(text.clone())
                        .test_support()
                        .flex_none()
                        .text_color(dim)
                        .child(text),
                )
            });
            let elapsed = m.elapsed_at(r.stmt, Instant::now()).map(|t| {
                let t = format_elapsed(t);
                div()
                    .id("footer-elapsed")
                    .aria_label(t.clone())
                    .test_support()
                    .flex_none()
                    .font_family(mono.clone())
                    .text_size(mono_size)
                    .text_color(dim)
                    .child(t)
            });
            div()
                .id("result-footer")
                .test_support()
                .flex()
                .flex_none()
                .items_center()
                .gap_3()
                .h(px(28.))
                .px_3()
                .border_t_1()
                .border_color(border)
                .text_xs()
                .child(
                    div()
                        .flex()
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .gap_3()
                        // A failure or a write has no rows to count; its note says what happened.
                        .when(!failed && dml.is_none() && plan_shown.is_none(), |d| {
                            d.child(
                                div()
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .gap_1()
                                    .child(ui::icon(ui::Icons::Rows3, px(12.), dim))
                                    .child(
                                        div()
                                            .id("footer-rows")
                                            .role(Role::Label)
                                            .accessibility_id("footer-rows")
                                            // AT-SPI names a Label by its value, not its label.
                                            .aria_value(rows.clone())
                                            .aria_label(rows.clone())
                                            .test_support()
                                            .child(rows),
                                    ),
                            )
                        })
                        .children(local_sort)
                        .children(pending.map(|text| {
                            div()
                                .id("footer-pending")
                                .aria_label(text.clone())
                                .test_support()
                                .flex_none()
                                .text_color(pending_text)
                                .child(text)
                        }))
                        .children(note.map(|(text, color)| {
                            div()
                                .id("footer-note")
                                .aria_label(text.clone())
                                .test_support()
                                .min_w_0()
                                .truncate()
                                .text_color(color)
                                .child(text)
                        }))
                        .children(fetch)
                        .children(resume)
                        .children(raise)
                        .children(read_only),
                )
                .children(record_nav)
                .when(plan_shown.is_none(), |d| d.child(self.row_view_buttons(cx)))
                .children(elapsed)
        });
        let message = m.error().map(|e| {
            div()
                .id("result-message")
                .aria_label(e.to_owned())
                .test_support()
                .px_3()
                .py_1()
                .text_sm()
                .text_color(danger)
                .child(e.to_owned())
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .key_context("ResultsPane")
            // Esc hides the note; capture sees it before the DataTable's own Cancel handler.
            .capture_action(
                cx.listener(|this, _: &gpui_kit::base::actions::Cancel, _, cx| {
                    if this.readonly.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_action(cx.listener(|this, _: &CopyCells, _, cx| {
                this.copy_selection(cx);
            }))
            .on_action(cx.listener(|this, _: &EditCell, window, cx| {
                // Only a single data cell opens; a row, column or gutter selection does not.
                if let TableSelection::Cell(row, col) = this.selection(cx)
                    && col > 0
                    && !this.begin_edit(row, col, window, cx)
                {
                    this.refuse_edit(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CommitCellEdit, window, cx| {
                this.finish_edit(true, false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &CommitCellEditRight, window, cx| {
                this.commit_right(window, cx);
            }))
            .on_action(cx.listener(|this, _: &CancelCellEdit, window, cx| {
                this.finish_edit(false, false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &DeleteRow, _, cx| {
                this.delete_selected(cx);
            }))
            .on_action(cx.listener(|this, _: &SaveEdits, window, cx| {
                this.save_edits(window, cx);
            }))
            // An open cell or field editor's own Ctrl+Z undoes typing; a history step under it
            // would change the value it edits, so both keys do nothing until it closes.
            .on_action(cx.listener(|this, _: &UndoEdit, _, cx| {
                if !this.any_editor_open(cx) {
                    this.model.update(cx, |m, cx| m.undo(cx));
                }
            }))
            .on_action(cx.listener(|this, _: &RedoEdit, _, cx| {
                if !this.any_editor_open(cx) {
                    this.model.update(cx, |m, cx| m.redo(cx));
                }
            }))
            .children(message)
            .when(!results.is_empty(), |d| {
                d.child(
                    div()
                        .id("statement-list")
                        .test_support()
                        .flex()
                        .flex_none()
                        .items_center()
                        .h(px(32.))
                        .border_b_1()
                        .border_color(border)
                        .child(
                            div()
                                .id("statement-tabs")
                                .flex()
                                .flex_1()
                                .min_w_0()
                                .h_full()
                                .overflow_x_scroll()
                                .children(tabs),
                        )
                        // A plan has no rows to export, so the button goes with the grid.
                        .when(plan_shown.is_none(), |d| d.child(export))
                        .children(edit_strip),
                )
            })
            .children(banner)
            .children(readonly)
            .children(filter_row)
            .children(kept)
            .map(|d| match (plan_shown.clone(), self.view) {
                (Some(plan), _) => d.child(div().flex_1().min_h_0().child(plan)),
                (None, ResultView::Record | ResultView::Plan) => d.child(self.record.clone()),
                // The context menu belongs to the grid alone, so the form has none.
                (None, ResultView::Grid) => d.child(
                    div()
                        .id("result-grid")
                        .role(Role::Group)
                        .accessibility_id("result-grid")
                        .test_support()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .on_key_down(cx.listener(Self::type_to_edit))
                        // Capture clears the target and, on a right-click, schedules our menu;
                        // the cell and header handlers aim it before it is built.
                        .capture_any_mouse_down(cx.listener(Self::grid_mouse_down))
                        .map(|d| match (error, dml) {
                            (Some((v, engine)), _) => d.child(self.error_body(v, engine, cx)),
                            (None, Some(text)) => d.child(status_body("result-dml", text)),
                            (None, None) => d
                                .child(smooth_scroll::layer(
                                    self.scroll_handle.clone(),
                                    self.wheel.clone(),
                                ))
                                .child(
                                    DataTable::new(&self.table)
                                        .with_size(settings::active_metrics(cx).grid_row)
                                        .stripe(true)
                                        .bordered(false),
                                )
                                .children(empty.map(|text| {
                                    self.empty_overlay(text, header_shown, clearable, cx)
                                })),
                        })
                        .children(self.grid_popup.as_ref().map(|(menu, at, ..)| {
                            deferred(
                                anchored()
                                    .position(*at)
                                    .snap_to_window_with_margin(px(8.))
                                    .child(menu.clone()),
                            )
                            .with_priority(gpui_kit::base::POPUP_PRIORITY)
                        })),
                ),
            })
            .children(footer)
    }
}

impl EventEmitter<OpenTable> for ResultsPane {}

/// A centred status line filling the body, labelled by its text.
fn status_body(id: &'static str, text: String) -> impl IntoElement {
    div()
        .id(id)
        .role(Role::Status)
        .aria_label(text.clone())
        .test_support()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_sm()
        .child(text)
}

/// Where a context menu opened and the row layout it saw.
#[derive(Clone, Copy)]
struct MenuTarget {
    grid: GridTarget,
    layout: (u64, usize),
}

/// The glyph a grid menu item leads with, by its id.
fn grid_menu_icon(id: &str) -> gpui_kit::assets::IconName {
    match id {
        "grid-menu-edit" => ui::Icons::Pencil,
        "grid-menu-set-null" => ui::Icons::Ban,
        "grid-menu-set-default" => ui::Icons::RotateCcw,
        "grid-menu-add-row" => ui::Icons::Plus,
        "grid-menu-duplicate-row" => ui::Icons::CopyPlus,
        "grid-menu-delete-row" => ui::Icons::Trash,
        "grid-menu-revert-row" => ui::Icons::Undo2,
        "grid-menu-export" => ui::Icons::Download,
        "header-menu-sort-asc" => ui::Icons::ArrowUp,
        "header-menu-sort-desc" => ui::Icons::ArrowDown,
        "header-menu-sort-clear" => ui::Icons::RotateCcw,
        "header-menu-show-all" => ui::Icons::Eye,
        "header-menu-hide" => ui::Icons::EyeOff,
        "header-menu-fit" => ui::Icons::Columns3,
        "header-menu-filter"
        | "grid-menu-filter-eq"
        | "grid-menu-filter-ne"
        | "grid-menu-filter-null" => ui::Icons::Funnel,
        _ => ui::Icons::Copy,
    }
}

/// The grid's context menu: its sections apart by separators, and within the Edit section the
/// row actions and the row copies each following one.
fn grid_menu(
    menu: PopupMenu,
    weak: &WeakEntity<ResultsPane>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Ok((target, sections)) = weak.update(cx, |p, cx| p.open_menu(cx)) else {
        return menu;
    };
    // The kit snaps the menu 8 px inside the window but never shortens it, so a long menu
    // scrolls within the window's height instead of running off its bottom.
    let fit = window.bounds().size.height - px(2. * 8.);
    let mut menu = menu.min_w(px(180.)).max_h(fit).scrollable(true);
    for (s, section) in sections.into_iter().enumerate() {
        if s > 0 {
            menu = menu.separator();
        }
        for (i, entry) in section.entries.into_iter().enumerate() {
            let MenuEntry {
                id,
                label,
                enabled,
                keys,
            } = entry;
            let lead = matches!(id, "grid-menu-add-row" | "grid-menu-copy-insert");
            if lead && i > 0 {
                menu = menu.separator();
            }
            // A cell filter shows the clause it would apply, which only the model knows.
            let clause = id
                .starts_with("grid-menu-filter-")
                .then(|| {
                    weak.update(cx, |p, cx| {
                        let stmt = p.table.read(cx).delegate().stmt;
                        filter_clause(p.model.read(cx), stmt, target.grid, id)
                    })
                    .ok()
                    .flatten()
                })
                .flatten();
            let label: SharedString = clause.map_or(label.into(), |c| filter_label(&c).into());
            let weak = weak.clone();
            let item = ui::menu_item(id, Some(grid_menu_icon(id)), label, keys, cx)
                .disabled(!enabled)
                .on_click(move |_, window, cx| {
                    let _ = weak.update(cx, |p, cx| p.menu_action(id, target, window, cx));
                });
            menu = menu.item(item);
        }
    }
    menu
}

/// The notice for a Copy as SQL INSERT whose column names could not be read.
fn insert_copy_failed(reason: &str, window: &mut Window, cx: &mut App) {
    let notice = ui::Notice {
        text: format!("Copy as SQL INSERT failed: {reason}"),
        kind: ui::NoticeKind::Warning,
    };
    ui::notice(window, cx, notice);
}
