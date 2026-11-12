//! The app's keyboard actions, their bindings (rebuilt from the user's keymap.json), and the
//! key hints the toolbar shows for them.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::{
    Action, App, Global, KeyBinding, KeyBindingContextPredicate, Keystroke, OperatingSystem,
    SharedString,
};

use crate::copy::CopyCells;
use crate::theme::{APPEARANCE_KEYS, CycleAppearance};
use crate::ui::{Notice, NoticeKind};
use crate::user_keymap::{self, KeymapProblem, Overrides, Row};

gpui_kit::actions!(
    dbento,
    [
        /// Runs the selection, else the statement at the caret.
        RunStatement,
        /// Runs the whole editor text.
        RunAll,
        /// Formats the selection, else the statement at the caret.
        FormatSql,
        /// Shows the plan of the selection, else the statement at the caret.
        Explain,
        /// Shows the plan with ANALYZE timings; the statement runs and is rolled back.
        ExplainAnalyze,
        /// Comments out the selected lines, or the caret's, else uncomments them.
        ToggleComment,
        /// Opens a tab on the active tab's connection.
        NewTab,
        /// Closes the active tab.
        CloseTab,
        /// Saves the active tab's SQL as a query file.
        SaveQuery,
        /// Opens the palette on saved queries.
        OpenQuery,
        /// Shows or hides the history panel.
        ToggleHistory,
        /// Shows or hides the row inspector.
        ToggleRowInspector,
        /// Re-reads `dbento.toml` and installs the result.
        ReloadWorkspace,
        /// Opens the connection dialog for a new connection.
        NewConnection,
        /// Saves the connection dialog's form.
        SaveConnection,
        /// Opens the in-cell editor on the grid's selected cell.
        EditCell,
        /// Stages the in-cell editor's text and closes it.
        CommitCellEdit,
        /// Stages the in-cell editor's text, closes it and selects the cell to its right.
        CommitCellEditRight,
        /// Closes the in-cell editor without staging.
        CancelCellEdit,
        /// Marks the selected grid row deleted.
        DeleteRow,
        /// Saves the staged edits, else the tab's query.
        SaveEdits,
        /// Undoes the last change to the staged edits.
        UndoEdit,
        /// Redoes the last undone change to the staged edits.
        RedoEdit,
        /// Stages the record form's open field and closes its editor.
        CommitRecordEdit,
        /// Closes the record form's field editor without staging.
        CancelRecordEdit,
        /// Opens the explorer's selected relation, as its double click does.
        OpenSelected,
        /// Moves the explorer's or plan tree's selection up one visible row.
        TreeSelectPrev,
        /// Moves the explorer's or plan tree's selection down one visible row.
        TreeSelectNext,
        /// Moves the explorer's or plan tree's selection to the first visible row.
        TreeSelectFirst,
        /// Moves the explorer's or plan tree's selection to the last visible row.
        TreeSelectLast,
        /// Expands the explorer's or plan tree's selected row, or enters it when open.
        TreeExpand,
        /// Collapses the explorer's or plan tree's selected row, or goes up when closed.
        TreeCollapse,
        /// Cancels the active tab's run.
        CancelQuery,
        /// Steps the active tab back to its previous reference hop.
        NavBack,
        /// Steps the active tab forward to its next reference hop.
        NavForward,
        /// Opens the palette.
        OpenPalette,
        /// Opens the palette on commands.
        OpenCommands,
        /// Steps the Ctrl+Tab walk back to an older tab.
        RecentTabBack,
        /// Steps the Ctrl+Tab walk forward to a newer tab.
        RecentTabForward,
        /// Activates the first tab.
        GoToTab1,
        /// Activates the second tab.
        GoToTab2,
        /// Activates the third tab.
        GoToTab3,
        /// Activates the fourth tab.
        GoToTab4,
        /// Activates the fifth tab.
        GoToTab5,
        /// Activates the sixth tab.
        GoToTab6,
        /// Activates the seventh tab.
        GoToTab7,
        /// Activates the eighth tab.
        GoToTab8,
        /// Activates the last tab.
        GoToLastTab,
        /// Moves the active tab one place left.
        MoveTabLeft,
        /// Moves the active tab one place right.
        MoveTabRight,
        /// Opens the Settings dialog.
        OpenSettings
    ]
);

/// The keys for `RunStatement`.
pub const RUN_STATEMENT_KEYS: &str = "secondary-enter";
/// The keys for `RunAll`.
pub const RUN_ALL_KEYS: &str = "secondary-shift-enter";
/// The keys for `FormatSql` (DBeaver's Format SQL); shown in the button's tooltip.
pub const FORMAT_SQL_KEYS: &str = "secondary-shift-f";
/// Other editors' format keys, bound too: VS Code's, and DataGrip's (which KDE takes for locking).
pub const FORMAT_SQL_ALIASES: [&str; 2] = ["alt-shift-f", "secondary-alt-l"];
/// The keys for `Explain`.
pub const EXPLAIN_KEYS: &str = "secondary-e";
/// The keys for `ExplainAnalyze`.
pub const EXPLAIN_ANALYZE_KEYS: &str = "secondary-shift-e";
/// The keys for `ToggleComment`, as in VS Code, Zed and DataGrip.
pub const TOGGLE_COMMENT_KEYS: &str = "secondary-/";
/// The keys for `NewTab`.
pub const NEW_TAB_KEYS: &str = "secondary-t";
/// The keys for `CloseTab`.
pub const CLOSE_TAB_KEYS: &str = "secondary-w";
/// The keys for `SaveQuery`.
pub const SAVE_QUERY_KEYS: &str = "secondary-s";
/// Keys that always save the query, even with staged edits that Ctrl+S would save instead.
pub const SAVE_QUERY_ALWAYS_KEYS: &str = "secondary-shift-s";
/// The keys for `CopyCells`, bound in the "DataTable" context.
pub const COPY_CELLS_KEYS: &str = "secondary-c";
/// The keys for `CancelQuery`.
pub const CANCEL_QUERY_KEYS: &str = "secondary-.";
/// The keys for `OpenQuery`.
pub const OPEN_QUERY_KEYS: &str = "secondary-o";
/// The keys for `ToggleHistory`; the kit's `Input` takes `ctrl-h` for Replace.
pub const HISTORY_KEYS: &str = "secondary-shift-h";
/// The keys for `ToggleRowInspector`; debug builds give `ctrl-shift-i` to the kit's inspector.
pub const INSPECTOR_KEYS: &str = "secondary-shift-d";
/// The keys for `ReloadWorkspace`.
pub const RELOAD_WORKSPACE_KEYS: &str = "secondary-shift-r";
/// The keys for `NewConnection`.
pub const NEW_CONNECTION_KEYS: &str = "secondary-shift-n";
/// The keys for `EditCell`.
pub const EDIT_CELL_KEYS: [&str; 2] = ["enter", "f2"];
/// The keys for `DeleteRow`.
pub const DELETE_ROW_KEYS: &str = "delete";
/// The keys for `OpenSelected`, in the explorer.
pub const EXPLORER_OPEN_KEYS: &str = "enter";
/// The explorer's tree keys, as in VS Code and Zed: `TreeSelectPrev`, `TreeSelectNext`,
/// `TreeSelectFirst`, `TreeSelectLast`, `TreeExpand` and `TreeCollapse`.
pub const TREE_KEYS: [&str; 6] = ["up", "down", "home", "end", "right", "left"];
/// The keys for `UndoEdit`.
pub const UNDO_EDIT_KEYS: &str = "secondary-z";
/// The keys for `RedoEdit`.
pub const REDO_EDIT_KEYS: &str = "secondary-shift-z";
/// The keys for `NavBack`, in the results pane and the inspector.
pub const NAV_BACK_KEYS: &str = "alt-left";
/// The keys for `NavForward`, in the results pane and the inspector.
pub const NAV_FORWARD_KEYS: &str = "alt-right";
/// The keys for `OpenPalette`.
pub const PALETTE_KEYS: &str = "secondary-p";
/// The keys for `OpenCommands`.
pub const COMMANDS_KEYS: &str = "secondary-shift-p";

/// The keys for `RecentTabBack`; Ctrl on every platform, as in browsers and IDEs.
pub const RECENT_TAB_KEYS: &str = "ctrl-tab";
/// The keys for `RecentTabForward`.
pub const RECENT_TAB_FORWARD_KEYS: &str = "ctrl-shift-tab";
/// The keys for `GoToTab1` to `GoToTab8`, then `GoToLastTab`, as browsers bind them.
pub const GO_TO_TAB_KEYS: [&str; 9] = [
    "secondary-1",
    "secondary-2",
    "secondary-3",
    "secondary-4",
    "secondary-5",
    "secondary-6",
    "secondary-7",
    "secondary-8",
    "secondary-9",
];
/// The keys for `MoveTabLeft`.
pub const MOVE_TAB_LEFT_KEYS: &str = "secondary-shift-pageup";
/// The keys for `MoveTabRight`.
pub const MOVE_TAB_RIGHT_KEYS: &str = "secondary-shift-pagedown";
/// The keys for `OpenSettings`.
pub const SETTINGS_KEYS: &str = "secondary-,";

/// One registered binding. `label` names it in the shortcut list; None keeps it out.
#[derive(Clone)]
pub struct Binding {
    /// The stable command id a user keymap names; rows of one action share it.
    pub id: &'static str,
    /// The keystrokes, as GPUI parses them.
    pub keys: &'static str,
    /// Builds the action the keys dispatch.
    pub action: fn() -> Box<dyn Action>,
    /// The key context the binding is limited to; None binds it everywhere.
    pub context: Option<&'static str>,
    /// The shortcut list's name for the binding.
    pub label: Option<&'static str>,
}

fn row(
    id: &'static str,
    keys: &'static str,
    action: fn() -> Box<dyn Action>,
    context: Option<&'static str>,
    label: Option<&'static str>,
) -> Binding {
    Binding {
        id,
        keys,
        action,
        context,
        label,
    }
}

/// Every binding the app registers, in registration order.
pub fn bindings() -> Vec<Binding> {
    let qt = Some("QueryTab");
    let qti = Some("QueryTab > Input");
    vec![
        // Zed's theme chord; no kit binding uses ctrl-k or ctrl-t.
        row(
            "view.cycle_appearance",
            APPEARANCE_KEYS,
            || Box::new(CycleAppearance),
            None,
            Some("Toggle appearance"),
        ),
        row(
            "app.open_settings",
            SETTINGS_KEYS,
            || Box::new(OpenSettings),
            None,
            Some("Open settings"),
        ),
        // Copies the grid's selection; scoped to the table so editors keep their own copy.
        row(
            "grid.copy_cells",
            COPY_CELLS_KEYS,
            || Box::new(CopyCells),
            Some("DataTable"),
            Some("Copy cells"),
        ),
        // The `Input` copies match at the editor's own depth and, registered after the kit's
        // `secondary-enter` newline, win the tie, so the editor runs instead of typing.
        row(
            "query.run_statement",
            RUN_STATEMENT_KEYS,
            || Box::new(RunStatement),
            qt,
            Some("Run statement"),
        ),
        row(
            "query.run_statement",
            RUN_STATEMENT_KEYS,
            || Box::new(RunStatement),
            qti,
            Some("Run statement"),
        ),
        row(
            "query.run_all",
            RUN_ALL_KEYS,
            || Box::new(RunAll),
            qt,
            Some("Run all"),
        ),
        row(
            "query.run_all",
            RUN_ALL_KEYS,
            || Box::new(RunAll),
            qti,
            Some("Run all"),
        ),
        row(
            "query.format",
            FORMAT_SQL_ALIASES[0],
            || Box::new(FormatSql),
            qt,
            Some("Format SQL"),
        ),
        row(
            "query.format",
            FORMAT_SQL_ALIASES[0],
            || Box::new(FormatSql),
            qti,
            Some("Format SQL"),
        ),
        row(
            "query.format",
            FORMAT_SQL_ALIASES[1],
            || Box::new(FormatSql),
            qt,
            Some("Format SQL"),
        ),
        row(
            "query.format",
            FORMAT_SQL_ALIASES[1],
            || Box::new(FormatSql),
            qti,
            Some("Format SQL"),
        ),
        // Added after its aliases: GPUI shows the last binding in tooltips and chips.
        row(
            "query.format",
            FORMAT_SQL_KEYS,
            || Box::new(FormatSql),
            qt,
            Some("Format SQL"),
        ),
        row(
            "query.format",
            FORMAT_SQL_KEYS,
            || Box::new(FormatSql),
            qti,
            Some("Format SQL"),
        ),
        // No `qti` copies: the kit's `Input` binds ctrl-e only on macOS, where `secondary` is cmd.
        row(
            "query.explain",
            EXPLAIN_KEYS,
            || Box::new(Explain),
            qt,
            Some("Explain plan"),
        ),
        row(
            "query.explain_analyze",
            EXPLAIN_ANALYZE_KEYS,
            || Box::new(ExplainAnalyze),
            qt,
            Some("Explain analyze"),
        ),
        row(
            "query.toggle_comment",
            TOGGLE_COMMENT_KEYS,
            || Box::new(ToggleComment),
            qt,
            Some("Toggle line comment"),
        ),
        row(
            "query.toggle_comment",
            TOGGLE_COMMENT_KEYS,
            || Box::new(ToggleComment),
            qti,
            Some("Toggle line comment"),
        ),
        row(
            "tabs.new",
            NEW_TAB_KEYS,
            || Box::new(NewTab),
            None,
            Some("New tab"),
        ),
        row(
            "tabs.close",
            CLOSE_TAB_KEYS,
            || Box::new(CloseTab),
            None,
            Some("Close tab"),
        ),
        row(
            "query.save",
            SAVE_QUERY_KEYS,
            || Box::new(SaveQuery),
            None,
            None,
        ),
        row(
            "query.save",
            SAVE_QUERY_ALWAYS_KEYS,
            || Box::new(SaveQuery),
            None,
            Some("Save query"),
        ),
        // The `Input` copy outranks the kit's `secondary-.` code actions at the editor's depth.
        row(
            "query.cancel",
            CANCEL_QUERY_KEYS,
            || Box::new(CancelQuery),
            qt,
            Some("Cancel query"),
        ),
        row(
            "query.cancel",
            CANCEL_QUERY_KEYS,
            || Box::new(CancelQuery),
            qti,
            Some("Cancel query"),
        ),
        row(
            "query.open_saved",
            OPEN_QUERY_KEYS,
            || Box::new(OpenQuery),
            None,
            Some("Open saved query"),
        ),
        row(
            "view.toggle_history",
            HISTORY_KEYS,
            || Box::new(ToggleHistory),
            None,
            Some("Toggle history"),
        ),
        row(
            "view.toggle_row_inspector",
            INSPECTOR_KEYS,
            || Box::new(ToggleRowInspector),
            None,
            Some("Toggle row inspector"),
        ),
        row(
            "workspace.reload",
            RELOAD_WORKSPACE_KEYS,
            || Box::new(ReloadWorkspace),
            None,
            Some("Reload dbento.toml"),
        ),
        row(
            "connection.new",
            NEW_CONNECTION_KEYS,
            || Box::new(NewConnection),
            None,
            Some("New connection"),
        ),
        row(
            "explorer.open_selected",
            EXPLORER_OPEN_KEYS,
            || Box::new(OpenSelected),
            Some("Explorer"),
            Some("Open selected table"),
        ),
        // `!Input` leaves the arrows to the explorer's filter while it has focus; each id also
        // walks the plan view's tree, so one rebinding moves both.
        row(
            "explorer.select_prev",
            TREE_KEYS[0],
            || Box::new(TreeSelectPrev),
            Some("Explorer && !Input"),
            Some("Previous tree row"),
        ),
        row(
            "explorer.select_prev",
            TREE_KEYS[0],
            || Box::new(TreeSelectPrev),
            Some("PlanView"),
            Some("Previous tree row"),
        ),
        row(
            "explorer.select_next",
            TREE_KEYS[1],
            || Box::new(TreeSelectNext),
            Some("Explorer && !Input"),
            Some("Next tree row"),
        ),
        row(
            "explorer.select_next",
            TREE_KEYS[1],
            || Box::new(TreeSelectNext),
            Some("PlanView"),
            Some("Next tree row"),
        ),
        row(
            "explorer.select_first",
            TREE_KEYS[2],
            || Box::new(TreeSelectFirst),
            Some("Explorer && !Input"),
            Some("First tree row"),
        ),
        row(
            "explorer.select_first",
            TREE_KEYS[2],
            || Box::new(TreeSelectFirst),
            Some("PlanView"),
            Some("First tree row"),
        ),
        row(
            "explorer.select_last",
            TREE_KEYS[3],
            || Box::new(TreeSelectLast),
            Some("Explorer && !Input"),
            Some("Last tree row"),
        ),
        row(
            "explorer.select_last",
            TREE_KEYS[3],
            || Box::new(TreeSelectLast),
            Some("PlanView"),
            Some("Last tree row"),
        ),
        row(
            "explorer.expand",
            TREE_KEYS[4],
            || Box::new(TreeExpand),
            Some("Explorer && !Input"),
            Some("Expand tree row, or enter it"),
        ),
        row(
            "explorer.expand",
            TREE_KEYS[4],
            || Box::new(TreeExpand),
            Some("PlanView"),
            Some("Expand tree row, or enter it"),
        ),
        row(
            "explorer.collapse",
            TREE_KEYS[5],
            || Box::new(TreeCollapse),
            Some("Explorer && !Input"),
            Some("Collapse tree row, or go up"),
        ),
        row(
            "explorer.collapse",
            TREE_KEYS[5],
            || Box::new(TreeCollapse),
            Some("PlanView"),
            Some("Collapse tree row, or go up"),
        ),
        // The form's deeper context outranks the global SaveQuery binding of the same keys.
        row(
            "connection.save",
            SAVE_QUERY_KEYS,
            || Box::new(SaveConnection),
            Some("ConnectionForm"),
            None,
        ),
        row(
            "grid.edit_cell",
            EDIT_CELL_KEYS[0],
            || Box::new(EditCell),
            Some("DataTable"),
            Some("Edit cell"),
        ),
        row(
            "grid.edit_cell",
            EDIT_CELL_KEYS[1],
            || Box::new(EditCell),
            Some("DataTable"),
            Some("Edit cell"),
        ),
        row(
            "grid.delete_row",
            DELETE_ROW_KEYS,
            || Box::new(DeleteRow),
            Some("DataTable"),
            Some("Delete row"),
        ),
        // The open cell editor's `Input` takes `secondary-z` for its own typing; the pane
        // ignores a `secondary-shift-z` that reaches it from there.
        row(
            "edits.undo",
            UNDO_EDIT_KEYS,
            || Box::new(UndoEdit),
            Some("DataTable"),
            Some("Undo edit"),
        ),
        row(
            "edits.redo",
            REDO_EDIT_KEYS,
            || Box::new(RedoEdit),
            Some("DataTable"),
            Some("Redo edit"),
        ),
        row(
            "edits.undo",
            UNDO_EDIT_KEYS,
            || Box::new(UndoEdit),
            Some("RecordView"),
            Some("Undo edit"),
        ),
        row(
            "edits.redo",
            REDO_EDIT_KEYS,
            || Box::new(RedoEdit),
            Some("RecordView"),
            Some("Redo edit"),
        ),
        // A context-free binding ranks as deepest, so a `ResultsPane` one would lose to
        // SaveQuery; registered later, SaveEdits is tried first. The pane handles it, else
        // the query tab saves staged edits or propagates to SaveQuery.
        row(
            "edits.save",
            SAVE_QUERY_KEYS,
            || Box::new(SaveEdits),
            None,
            Some("Save staged edits, else the query"),
        ),
        // Registered after the kit's `Input` bindings, so they win the tie at the input's depth.
        row(
            "cell_editor.commit",
            "enter",
            || Box::new(CommitCellEdit),
            Some("CellEditor > Input"),
            Some("Stage cell edit"),
        ),
        row(
            "cell_editor.commit_right",
            "tab",
            || Box::new(CommitCellEditRight),
            Some("CellEditor > Input"),
            Some("Stage and edit the next cell"),
        ),
        row(
            "cell_editor.cancel",
            "escape",
            || Box::new(CancelCellEdit),
            Some("CellEditor > Input"),
            Some("Cancel cell edit"),
        ),
        // Also after `QueryTab > Input`, so Ctrl+Enter in a record field stages, not runs.
        row(
            "record.commit_field",
            "secondary-enter",
            || Box::new(CommitRecordEdit),
            Some("RecordEditor > Input"),
            Some("Stage field"),
        ),
        // Enter stages a one-line field; a `multiline` field keeps the kit's newline.
        row(
            "record.commit_field",
            "enter",
            || Box::new(CommitRecordEdit),
            Some("RecordEditor && !multiline > Input"),
            Some("Stage field"),
        ),
        row(
            "record.cancel_field",
            "escape",
            || Box::new(CancelRecordEdit),
            Some("RecordEditor > Input"),
            Some("Cancel field edit"),
        ),
        // `!Input` checks the whole context stack, so no input under either context sees them.
        row(
            "nav.back",
            NAV_BACK_KEYS,
            || Box::new(NavBack),
            Some("ResultsPane && !Input"),
            Some("Back"),
        ),
        row(
            "nav.back",
            NAV_BACK_KEYS,
            || Box::new(NavBack),
            Some("Inspector && !Input"),
            Some("Back"),
        ),
        row(
            "nav.forward",
            NAV_FORWARD_KEYS,
            || Box::new(NavForward),
            Some("ResultsPane && !Input"),
            Some("Forward"),
        ),
        row(
            "nav.forward",
            NAV_FORWARD_KEYS,
            || Box::new(NavForward),
            Some("Inspector && !Input"),
            Some("Forward"),
        ),
        row(
            "palette.open",
            PALETTE_KEYS,
            || Box::new(OpenPalette),
            None,
            Some("Open palette"),
        ),
        row(
            "palette.commands",
            COMMANDS_KEYS,
            || Box::new(OpenCommands),
            None,
            Some("Open commands"),
        ),
        // No kit binding uses these chords, so they reach the shell from the editor too.
        row(
            "tabs.recent",
            RECENT_TAB_KEYS,
            || Box::new(RecentTabBack),
            None,
            Some("Switch to recent tab"),
        ),
        row(
            "tabs.recent_reverse",
            RECENT_TAB_FORWARD_KEYS,
            || Box::new(RecentTabForward),
            None,
            Some("Switch to recent tab, reversed"),
        ),
        row(
            "tabs.go_to_1",
            GO_TO_TAB_KEYS[0],
            || Box::new(GoToTab1),
            None,
            Some("Go to tab 1"),
        ),
        row(
            "tabs.go_to_2",
            GO_TO_TAB_KEYS[1],
            || Box::new(GoToTab2),
            None,
            Some("Go to tab 2"),
        ),
        row(
            "tabs.go_to_3",
            GO_TO_TAB_KEYS[2],
            || Box::new(GoToTab3),
            None,
            Some("Go to tab 3"),
        ),
        row(
            "tabs.go_to_4",
            GO_TO_TAB_KEYS[3],
            || Box::new(GoToTab4),
            None,
            Some("Go to tab 4"),
        ),
        row(
            "tabs.go_to_5",
            GO_TO_TAB_KEYS[4],
            || Box::new(GoToTab5),
            None,
            Some("Go to tab 5"),
        ),
        row(
            "tabs.go_to_6",
            GO_TO_TAB_KEYS[5],
            || Box::new(GoToTab6),
            None,
            Some("Go to tab 6"),
        ),
        row(
            "tabs.go_to_7",
            GO_TO_TAB_KEYS[6],
            || Box::new(GoToTab7),
            None,
            Some("Go to tab 7"),
        ),
        row(
            "tabs.go_to_8",
            GO_TO_TAB_KEYS[7],
            || Box::new(GoToTab8),
            None,
            Some("Go to tab 8"),
        ),
        row(
            "tabs.go_to_last",
            GO_TO_TAB_KEYS[8],
            || Box::new(GoToLastTab),
            None,
            Some("Go to last tab"),
        ),
        row(
            "tabs.move_left",
            MOVE_TAB_LEFT_KEYS,
            || Box::new(MoveTabLeft),
            None,
            Some("Move tab left"),
        ),
        row(
            "tabs.move_right",
            MOVE_TAB_RIGHT_KEYS,
            || Box::new(MoveTabRight),
            None,
            Some("Move tab right"),
        ),
    ]
}

/// Every command id, once each, in table order of first appearance.
pub fn command_ids() -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = Vec::new();
    for b in bindings() {
        if !ids.contains(&b.id) {
            ids.push(b.id);
        }
    }
    ids
}

/// The keys of `id`'s last table row (the one GPUI shows); None for an unknown id.
pub fn default_keys(id: &str) -> Option<&'static str> {
    bindings()
        .into_iter()
        .rev()
        .find(|b| b.id == id)
        .map(|b| b.keys)
}

/// The kit's own bindings, snapshotted by the first `bind` (dbento rows excluded).
struct KitBindings(Vec<KeyBinding>);

impl Global for KitBindings {}

/// The effective keymap: the rows bound, the overrides behind them, the file's problems, and
/// the file's path (None without a config folder).
struct ActiveKeymap {
    rows: Vec<Row>,
    overrides: Overrides,
    problems: Vec<KeymapProblem>,
    path: Option<PathBuf>,
}

impl Global for ActiveKeymap {}

/// Binds the table with no overrides; must run after `gpui_kit::init`. The first rebuild
/// snapshots the kit's bindings so every later one can put them back ahead of ours.
pub fn bind(cx: &mut App) {
    let overrides = Overrides::default();
    apply(
        ActiveKeymap {
            rows: user_keymap::effective(&overrides),
            overrides,
            problems: Vec::new(),
            path: None,
        },
        cx,
    );
}

/// Loads the user file at `path` (moving a broken one aside), rebuilds the bindings and
/// refreshes the windows; returns the warning toast text when the file was unusable.
pub fn install_user(path: Option<PathBuf>, cx: &mut App) -> Option<String> {
    let Some(file) = &path else {
        bind(cx);
        return None;
    };
    let load = user_keymap::load(file, true);
    let overrides = load.overrides.unwrap_or_default();
    apply(
        ActiveKeymap {
            rows: user_keymap::effective(&overrides),
            overrides,
            problems: load.problems,
            path,
        },
        cx,
    );
    load.warning
}

/// What a Reload did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reloaded {
    /// How many problems the file has now.
    pub problems: usize,
    /// The file was unusable, so the previous overrides stay bound.
    pub kept_previous: bool,
}

impl Reloaded {
    /// The toast for this outcome: an info line, or a warning when the previous keys were kept.
    pub fn notice(self) -> Notice {
        let (text, kind) = match (self.kept_previous, self.problems) {
            (true, _) => (
                "keymap.json could not be read; the previous keys are kept".to_owned(),
                NoticeKind::Warning,
            ),
            (false, 0) => ("Keymap reloaded".to_owned(), NoticeKind::Info),
            (false, 1) => ("Keymap reloaded, 1 problem".to_owned(), NoticeKind::Info),
            (false, n) => (format!("Keymap reloaded, {n} problems"), NoticeKind::Info),
        };
        Notice { text, kind }
    }
}

/// Re-reads the keymap file without moving it and rebuilds; an unusable file keeps the
/// current overrides and lists only its `Unparseable` problem. No path reloads nothing.
pub fn reload(cx: &mut App) -> Reloaded {
    let (path, previous) = match cx.try_global::<ActiveKeymap>() {
        Some(k) => (k.path.clone(), k.overrides.clone()),
        None => (None, Overrides::default()),
    };
    let Some(file) = &path else {
        return Reloaded {
            problems: problems(cx).len(),
            kept_previous: false,
        };
    };
    let load = user_keymap::load(file, false);
    let kept_previous = load.overrides.is_none();
    let overrides = load.overrides.unwrap_or(previous);
    let problems = load.problems.len();
    apply(
        ActiveKeymap {
            rows: user_keymap::effective(&overrides),
            overrides,
            problems: load.problems,
            path,
        },
        cx,
    );
    Reloaded {
        problems,
        kept_previous,
    }
}

/// Rebuilds the key map from scratch: the kit snapshot first, then the effective rows, so the
/// table's ties with kit bindings still go our way. Installs `keymap` as the active one.
fn apply(keymap: ActiveKeymap, cx: &mut App) {
    // Whichever of bind, install_user or reload runs first takes the snapshot, before the
    // clear below could lose the kit's bindings.
    if !cx.has_global::<KitBindings>() {
        let kit = cx
            .key_bindings()
            .borrow()
            .bindings()
            .filter(|b| !b.action().name().starts_with("dbento::"))
            .cloned()
            .collect();
        cx.set_global(KitBindings(kit));
    }
    let kit = cx.global::<KitBindings>().0.clone();
    cx.clear_key_bindings();
    cx.bind_keys(kit);
    let rows: Vec<_> = keymap
        .rows
        .iter()
        .filter_map(|r| key_binding(r, cx))
        .collect();
    cx.bind_keys(rows);
    cx.set_global(keymap);
    cx.refresh_windows();
}

/// The GPUI binding for `row`. A user key was already parsed by `user_keymap`, so one GPUI
/// still rejects is dropped rather than panicking; a dropped table row fails
/// `bind_registers_exactly_the_bindings_table`.
fn key_binding(row: &Row, cx: &App) -> Option<KeyBinding> {
    let context = match row.context {
        Some(c) => Some(Rc::new(KeyBindingContextPredicate::parse(c).ok()?)),
        None => None,
    };
    KeyBinding::load(&row.keys, (row.action)(), context, false, None, cx).ok()
}

/// The OS that resolves `secondary-` in a key parsed without an `App`: the build target,
/// as GPUI itself decides for a native app.
pub const HOST_OS: OperatingSystem = if cfg!(target_os = "macos") {
    OperatingSystem::Mac
} else if cfg!(target_os = "windows") {
    OperatingSystem::Windows
} else {
    OperatingSystem::Linux
};

/// The keymap file's current problems; empty before a user keymap is installed.
pub fn problems(cx: &App) -> Vec<KeymapProblem> {
    cx.try_global::<ActiveKeymap>()
        .map(|k| k.problems.clone())
        .unwrap_or_default()
}

/// The keymap file's path; None without a config folder or before `install_user`.
pub fn keymap_path(cx: &App) -> Option<PathBuf> {
    cx.try_global::<ActiveKeymap>().and_then(|k| k.path.clone())
}

/// What a [`FileOpener`] calls with the file to open.
pub type OpenFile = dyn Fn(&Path, &App);

/// Opens a file with the OS; tests install a recorder because the test platform cannot.
#[derive(Clone)]
pub struct FileOpener(pub Rc<OpenFile>);

impl Global for FileOpener {}

/// Creates the keymap file as "{}\n" (and its folder) when missing, then opens it with the
/// FileOpener global, else `cx.open_with_system`. Err when there is no path or the write fails.
pub fn open_keymap_file(cx: &mut App) -> std::io::Result<PathBuf> {
    let path = keymap_path(cx)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no config folder"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // create_new, so a file that appeared meanwhile is never overwritten.
    match std::fs::File::create_new(&path) {
        Ok(mut file) => std::io::Write::write_all(&mut file, b"{}\n")?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    match cx.try_global::<FileOpener>().cloned() {
        Some(FileOpener(open)) => open(&path, cx),
        None => cx.open_with_system(&path),
    }
    Ok(path)
}

/// The effective rows, or the table's when no keymap is active.
fn rows(cx: &App) -> Vec<Row> {
    match cx.try_global::<ActiveKeymap>() {
        Some(k) => k.rows.clone(),
        None => user_keymap::effective(&Overrides::default()),
    }
}

/// The command id `action` belongs to, by action name.
fn id_of(action: &dyn Action) -> Option<&'static str> {
    bindings()
        .into_iter()
        .find(|b| (b.action)().name() == action.name())
        .map(|b| b.id)
}

/// The keys a chip shows for `action`: an overridden id's first key (None when unbound),
/// otherwise its last default row's keys (the one GPUI shows).
pub fn keys_for(action: &dyn Action, cx: &App) -> Option<SharedString> {
    let id = id_of(action)?;
    let overridden = cx
        .try_global::<ActiveKeymap>()
        .and_then(|k| k.overrides.get(id));
    match overridden {
        Some(keys) => keys.first().map(|k| SharedString::from(k.clone())),
        None => default_keys(id).map(SharedString::from),
    }
}

/// Every effective key of `action`, in row order, each once.
pub fn all_keys_for(action: &dyn Action, cx: &App) -> Vec<SharedString> {
    let mut keys: Vec<SharedString> = Vec::new();
    for r in rows(cx) {
        if (r.action)().name() == action.name() && !keys.contains(&r.keys) {
            keys.push(r.keys);
        }
    }
    keys
}

/// Every command: (id, label of its first labelled row or the id, effective keys), in
/// `command_ids` order.
pub fn commands(cx: &App) -> Vec<(&'static str, &'static str, Vec<SharedString>)> {
    let table = bindings();
    let rows = rows(cx);
    command_ids()
        .into_iter()
        .map(|id| {
            let label = table
                .iter()
                .find_map(|b| (b.id == id).then_some(b.label).flatten())
                .unwrap_or(id);
            let mut keys: Vec<SharedString> = Vec::new();
            for r in rows.iter().filter(|r| r.id == id) {
                if !keys.contains(&r.keys) {
                    keys.push(r.keys.clone());
                }
            }
            (id, label, keys)
        })
        .collect()
}

/// One row of the shortcut list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shortcut {
    /// What the keys do.
    pub label: &'static str,
    /// The keys, as a binding string.
    pub keys: SharedString,
}

/// The labelled effective rows, deduplicated by (label, keys), in row order.
pub fn shortcuts(cx: &App) -> Vec<Shortcut> {
    let mut list: Vec<Shortcut> = Vec::new();
    for r in rows(cx) {
        let Some(label) = r.label else { continue };
        let s = Shortcut {
            label,
            keys: r.keys,
        };
        if !list.contains(&s) {
            list.push(s);
        }
    }
    list
}

/// The chip text for `keys`: every keystroke of a chord through [`key_hint`], joined by spaces.
pub fn chip_keys(keys: &str) -> String {
    keys.split_whitespace()
        .flat_map(key_hint)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The hint text for `keys`: each keystroke's [`key_hint`] parts joined by "+", strokes by spaces.
pub fn plus_keys(keys: &str) -> String {
    keys.split_whitespace()
        .map(|k| key_hint(k).join("+"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The chip's parts for `keys`: "Ctrl", "Alt", "Shift", "Super" (macOS "⌘"), then the key,
/// with "enter" as "⏎", the arrows as "←", "→", "↑" and "↓", "tab" as "Tab", the page
/// keys as "PgUp" and "PgDn", "home" and "end" as "Home" and "End", and any other key
/// uppercased. Keys that do not parse give nothing.
pub fn key_hint(keys: &str) -> Vec<String> {
    let Ok(k) = Keystroke::parse_with_os(keys, HOST_OS) else {
        return Vec::new();
    };
    let m = k.modifiers;
    let platform = if cfg!(target_os = "macos") {
        "\u{2318}"
    } else {
        "Super"
    };
    let mods = [
        (m.control, "Ctrl"),
        (m.alt, "Alt"),
        (m.shift, "Shift"),
        (m.platform, platform),
    ];
    let mut parts: Vec<String> = mods
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, name)| name.to_owned())
        .collect();
    parts.push(match k.key.as_str() {
        "enter" => "\u{23CE}".to_owned(),
        "left" => "\u{2190}".to_owned(),
        "right" => "\u{2192}".to_owned(),
        "up" => "\u{2191}".to_owned(),
        "down" => "\u{2193}".to_owned(),
        "home" => "Home".to_owned(),
        "end" => "End".to_owned(),
        "tab" => "Tab".to_owned(),
        "pageup" => "PgUp".to_owned(),
        "pagedown" => "PgDn".to_owned(),
        key => key.to_uppercase(),
    });
    parts
}
