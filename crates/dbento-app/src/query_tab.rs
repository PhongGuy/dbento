//! A query tab: the SQL editor over the results pane, Run and Cancel, the production chip,
//! the read-only badge and the confirmation for unguarded writes.

use std::borrow::Cow;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dbento_core::{
    ConnectionConfig, Engine, ExplainMode, QueryLanguage, ReadOnlyEnforcement, RelationRef,
    Unguarded, Workspace, explain_request, read_only_residual,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    Editor, EditorState, Enter, Escape, IndentInline, InputEvent, MoveDown, MoveUp,
};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, ResizableState, Sizable as _, WindowExt as _,
    resizable_panel, v_resizable,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, EntityId, EventEmitter, Hsla, InteractiveElement,
    IntoElement, ParentElement, Pixels, Render, Role, SharedString, StatefulInteractiveElement,
    Styled, Subscription, Task, TestSupportExt, WeakEntity, Window, div, px,
};

use crate::browse::{BrowseEvent, unavailable_text};
use crate::comment::toggle_line_comments;
use crate::completion_popup::CompletionPopup;
use crate::confirm::{Ask, Choice, ask};
use crate::drafts::{DRAFT_DEBOUNCE, Draft, DraftSlot};
use crate::editor_completion::SqlCompletionProvider;
use crate::format::format_edit;
use crate::grid_menu::MenuEntry;
use crate::key_view::KeyView;
use crate::keymap::{
    CancelQuery, Explain, ExplainAnalyze, FormatSql, RunAll, RunStatement, SaveEdits, SaveQuery,
    ToggleComment, all_keys_for, chip_keys, keys_for, plus_keys,
};
use crate::nav::NavHistory;
use crate::results_pane::{EditConnection, OpenTable, ResultsPane, RetryConnect, skip_notice};
use crate::run_target::run_range;
use crate::sidebar::{Sidebar, color_name, engine_label, tag_color};
use crate::status_bar::run_elapsed;
use crate::tab_model::{Discard, ROW_LIMITS, TabModel};
use crate::{staged, theme, ui};

// A restored editor height leaves this much of the window for the results and the chrome
// around them, so a height saved on a taller screen never squeezes the results shut.
const RESULTS_ROOM: Pixels = px(312.);
const EDITOR_MIN: Pixels = px(100.);
/// The height of a tab's toolbar above its query split.
pub(crate) const TOOLBAR_HEIGHT: Pixels = px(40.);

/// The `row-limit` button's text for `cap`.
pub fn row_limit_label(cap: Option<u64>) -> String {
    match cap {
        None => "no limit".into(),
        Some(n) if n >= 1_000_000 && n % 1_000_000 == 0 => format!("{}M rows", n / 1_000_000),
        Some(n) if n >= 1_000 && n % 1_000 == 0 => format!("{}k rows", n / 1_000),
        Some(n) => format!("{n} rows"),
    }
}

/// The limit after `cap` in `ROW_LIMITS`, wrapping; a cap outside the list restarts at 1k.
pub fn next_row_limit(cap: Option<u64>) -> Option<u64> {
    let at = ROW_LIMITS.iter().position(|&l| l == cap);
    ROW_LIMITS[at.map_or(0, |i| (i + 1) % ROW_LIMITS.len())]
}

/// The read-only badge's tooltip on a Postgres connection.
pub const READ_ONLY_PG_TOOLTIP: &str = "Read-only: each statement runs in its own read-only transaction, so a SET lasts one statement. Put settings in the connection's `settings` in dbento.toml.";
/// The read-only badge's tooltip on a SQLite connection.
pub const READ_ONLY_SQLITE_TOOLTIP: &str =
    "Read-only: SQLite refuses every write on this connection.";
/// The read-only badge's tooltip on a MySQL or MariaDB connection.
pub const READ_ONLY_MYSQL_TOOLTIP: &str = "Read-only: each statement runs in a read-only transaction, but GET_LOCK and writes to non-transactional tables are not blocked. Use an account with only SELECT.";
/// The read-only badge's tooltip on a DuckDB connection.
pub const READ_ONLY_DUCKDB_TOOLTIP: &str = "Read-only: DuckDB opens the file read-only with file access off, so writes, COPY TO, ATTACH and read_csv or parquet of local files are refused.";
/// The read-only badge's tooltip on a SQL Server connection with a read_only_user.
pub const READ_ONLY_MSSQL_READER_TOOLTIP: &str = "Read-only: runs as the read_only_user principal, so SQL Server refuses writes. Procedures granted to public can still write.";
/// The read-only badge's tooltip on a SQL Server connection without a read_only_user.
pub const READ_ONLY_MSSQL_BEST_EFFORT_TOOLTIP: &str = "Read-only is best effort: each batch is fenced in a rolled-back transaction and parseable writes are refused, but a hidden commit can get past it. Set read_only_user for enforcement.";
/// The SurrealDB badge's head when the session runs as a VIEWER.
pub const READ_ONLY_SURREAL_VIEWER_TOOLTIP: &str = "Read-only: this VIEWER login cannot write, and Dbento runs each statement in a transaction it cancels.";
/// The SurrealDB badge's head when the login could write and only the fence holds.
pub const READ_ONLY_SURREAL_BEST_EFFORT_TOOLTIP: &str = "Read-only is best effort: Dbento runs each statement in a transaction it cancels and refuses the writes it can see. Use a VIEWER login for enforcement.";
/// The SurrealDB badge's head before the session has reported its level.
pub const READ_ONLY_SURREAL_UNCONNECTED_TOOLTIP: &str = "Read-only: Dbento runs each statement in a transaction it cancels; the level shows once connected.";
/// The MongoDB badge when the URI user holds only read privileges.
pub const READ_ONLY_MONGO_READER_TOOLTIP: &str = "Read-only: this user holds only read privileges, so MongoDB refuses writes, and Dbento refuses the writes it can see before sending them.";
/// The MongoDB badge when the URI user can write and only Dbento's own refusal holds.
pub const READ_ONLY_MONGO_BEST_EFFORT_TOOLTIP: &str = "Read-only is best effort: this user can write, so Dbento refuses every statement it reads as a write or cannot classify before sending it. Use a user with only the read role for enforcement.";
/// The MongoDB badge before the session has reported its level.
pub const READ_ONLY_MONGO_UNCONNECTED_TOOLTIP: &str =
    "Read-only: the level follows the privileges of the user in the URI and shows once connected.";
/// The Redis badge's head when the user's ACL refused every probed write.
pub const READ_ONLY_REDIS_ACL_TOOLTIP: &str = "Read-only: this user's ACL refused every write Dbento probed at connect, so Redis refuses writes.";
/// The Redis badge's head when the ACL allows writes and only Dbento's own refusal holds.
pub const READ_ONLY_REDIS_BEST_EFFORT_TOOLTIP: &str = "Read-only is best effort: this user's ACL allows writes, so Dbento refuses every command it reads as a write or cannot classify before sending it. Use a reader ACL user like dbento_ro for enforcement.";
/// The Redis badge's head before the session has reported its level.
pub const READ_ONLY_REDIS_UNCONNECTED_TOOLTIP: &str =
    "Read-only: the level follows the user's ACL and shows once connected.";
/// The DynamoDB badge's head; its level is always best effort, since no session is read-only.
pub const READ_ONLY_DYNAMODB_TOOLTIP: &str = "Read-only is best effort: DynamoDB statements that Dbento reads as a write or cannot classify are refused.";

/// The Save button's tooltip: with `staged` edits, which effective keys save them and which
/// the query. A clause whose action is unbound is left out.
pub fn save_tooltip(staged: usize, cx: &App) -> String {
    let s = if staged == 1 { "" } else { "s" };
    let clauses: Vec<String> = [
        keys_for(&SaveEdits, cx)
            .map(|k| format!("{} saves {staged} staged edit{s}", plus_keys(&k))),
        keys_for(&SaveQuery, cx).map(|k| format!("{} saves the query", plus_keys(&k))),
    ]
    .into_iter()
    .flatten()
    .collect();
    if staged == 0 || clauses.is_empty() {
        return "Save query".to_owned();
    }
    clauses.join("; ")
}

/// The key the Save button's tooltip boxes: the save-edits keys, since the button saves
/// staged edits first and only then the query.
pub fn save_button_keys(cx: &App) -> Option<SharedString> {
    keys_for(&SaveEdits, cx)
}

/// The Format button's tooltip, naming the alias keys beside the boxed main one: every
/// effective key of FormatSql except the one the tooltip boxes.
pub fn format_tooltip(cx: &App) -> String {
    let main = keys_for(&FormatSql, cx);
    let aliases: Vec<String> = all_keys_for(&FormatSql, cx)
        .into_iter()
        .filter(|k| Some(k) != main.as_ref())
        .map(|k| plus_keys(&k))
        .collect();
    if aliases.is_empty() {
        return "Format SQL".to_owned();
    }
    format!("Format SQL (also {})", aliases.join(", "))
}

/// The read-only badge's tooltip for `engine`.
pub fn read_only_tooltip(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => READ_ONLY_PG_TOOLTIP,
        Engine::Sqlite => READ_ONLY_SQLITE_TOOLTIP,
        Engine::Mysql => READ_ONLY_MYSQL_TOOLTIP,
        Engine::Duckdb => READ_ONLY_DUCKDB_TOOLTIP,
        Engine::Dynamodb => READ_ONLY_DYNAMODB_TOOLTIP,
        Engine::Mssql | Engine::Mongodb | Engine::Surrealdb | Engine::Redis | _ => {
            "Read-only connection."
        }
    }
}

/// The badge text for `c`: SQL Server by `c.read_only_enforcement()`, every other engine as
/// `read_only_tooltip`.
pub fn read_only_tooltip_of(c: &ConnectionConfig) -> &'static str {
    match (c.engine(), c.read_only_enforcement()) {
        (Engine::Mssql, ReadOnlyEnforcement::Database) => READ_ONLY_MSSQL_READER_TOOLTIP,
        (Engine::Mssql, ReadOnlyEnforcement::BestEffort) => READ_ONLY_MSSQL_BEST_EFFORT_TOOLTIP,
        _ => read_only_tooltip(c.engine()),
    }
}

/// The badge text for `c` with the session's `level`: on SurrealDB and Redis the level's head, a
/// space and the residual; on DynamoDB its fixed head and the residual at every level; on MongoDB
/// the level's text alone; on every other engine
/// `read_only_tooltip_of(c)`.
pub fn read_only_badge_text(c: &ConnectionConfig, level: Option<ReadOnlyEnforcement>) -> String {
    match c.engine() {
        // MongoDB's level follows the URI user's privileges, which only the session knows.
        Engine::Mongodb => {
            return match level {
                Some(ReadOnlyEnforcement::Database) => READ_ONLY_MONGO_READER_TOOLTIP,
                Some(ReadOnlyEnforcement::BestEffort) => READ_ONLY_MONGO_BEST_EFFORT_TOOLTIP,
                _ => READ_ONLY_MONGO_UNCONNECTED_TOOLTIP,
            }
            .to_owned();
        }
        Engine::Dynamodb => {
            let residual = read_only_residual(Engine::Dynamodb).unwrap_or_default();
            return format!("{READ_ONLY_DYNAMODB_TOOLTIP} {residual}");
        }
        Engine::Surrealdb | Engine::Redis => {}
        Engine::Postgres | Engine::Sqlite | Engine::Mysql | Engine::Mssql | Engine::Duckdb | _ => {
            return read_only_tooltip_of(c).to_owned();
        }
    }
    // SurrealDB's level comes from the login's role and Redis's from the user's ACL, which only
    // the session knows.
    let head = match (c.engine(), level) {
        (Engine::Redis, Some(ReadOnlyEnforcement::Database)) => READ_ONLY_REDIS_ACL_TOOLTIP,
        (Engine::Redis, Some(ReadOnlyEnforcement::BestEffort)) => {
            READ_ONLY_REDIS_BEST_EFFORT_TOOLTIP
        }
        (Engine::Redis, _) => READ_ONLY_REDIS_UNCONNECTED_TOOLTIP,
        (_, Some(ReadOnlyEnforcement::Database)) => READ_ONLY_SURREAL_VIEWER_TOOLTIP,
        (_, Some(ReadOnlyEnforcement::BestEffort)) => READ_ONLY_SURREAL_BEST_EFFORT_TOOLTIP,
        _ => READ_ONLY_SURREAL_UNCONNECTED_TOOLTIP,
    };
    match read_only_residual(c.engine()) {
        Some(residual) => format!("{head} {residual}"),
        None => head.to_owned(),
    }
}

/// The confirmation's title: the kind of unguarded statement and where it would run.
pub fn confirm_title(kind: Unguarded, connection: &str) -> String {
    format!("{} on '{connection}'", kind.label())
}

/// The confirmation's one-line explanation of what the statement would do.
pub fn confirm_explanation(kind: Option<Unguarded>) -> &'static str {
    match kind {
        Some(Unguarded::UpdateWithoutWhere) => "It has no WHERE clause, so it changes every row.",
        Some(Unguarded::DeleteWithoutWhere) => "It has no WHERE clause, so it deletes every row.",
        Some(Unguarded::Drop) => "It removes the object and everything in it.",
        Some(Unguarded::Truncate) => "It removes every row of the table.",
        Some(Unguarded::UpdateAllDocuments) => {
            "Its filter is empty, so it changes every document in the collection."
        }
        Some(Unguarded::DeleteAllDocuments) => {
            "Its filter is empty, so it deletes every document in the collection."
        }
        Some(Unguarded::FlushKeys) => {
            "It deletes every key in the database, or with FLUSHALL in every database."
        }
        _ => "It may change rows.",
    }
}

/// The question asked before a guarded statement runs: Cancel (Enter and Escape) or a
/// destructive "Run anyway", with the statement boxed under the explanation and highlighted
/// as `language`.
pub fn unguarded_ask(
    kind: Option<Unguarded>,
    connection: &str,
    sql: &str,
    language: &'static str,
) -> Ask {
    let title = match kind {
        Some(kind) => confirm_title(kind, connection),
        None => format!("Run this statement on '{connection}'?"),
    };
    Ask {
        title: title.into(),
        detail: Some(confirm_explanation(kind).into()),
        sql: Some(sql.to_owned().into()),
        language,
        choices: vec![
            Choice {
                id: "confirm-skip",
                label: "Cancel".into(),
                danger: false,
            },
            Choice {
                id: "confirm-run",
                label: "Run anyway".into(),
                danger: true,
            },
        ],
        default: 0,
        cancel: 0,
    }
}

/// The question asked before an explain's ANALYZE runs `sql`: Cancel (Enter and Escape) or
/// a destructive "Run ANALYZE", naming any unguarded `kinds` the statement also is.
pub fn analyze_ask(kinds: &[Unguarded], connection: &str, sql: &str) -> Ask {
    let mut detail = "ANALYZE executes the statement to time it. Its changes are rolled back, \
        but sequence values it takes and effects outside the transaction are kept."
        .to_owned();
    if !kinds.is_empty() {
        let labels: Vec<_> = kinds.iter().map(|k| k.label()).collect();
        detail.push_str(&format!(" It is also: {}.", labels.join(", ")));
    }
    Ask {
        title: format!("EXPLAIN ANALYZE runs this statement on '{connection}'").into(),
        detail: Some(detail.into()),
        sql: Some(sql.to_owned().into()),
        language: "sql",
        choices: vec![
            Choice {
                id: "confirm-skip",
                label: "Cancel".into(),
                danger: false,
            },
            Choice {
                id: "confirm-run",
                label: "Run ANALYZE".into(),
                danger: true,
            },
        ],
        default: 0,
        cancel: 0,
    }
}

/// The connection chip's text, "name · engine · target": the Postgres database, the MySQL or
/// SQL Server database when set, SurrealDB's "namespace/database", or else the target as
/// written in dbento.toml.
pub fn connection_chip_text(c: &ConnectionConfig) -> String {
    let target = match c.engine() {
        Engine::Postgres => c.database().unwrap_or_default().to_owned(),
        Engine::Mysql | Engine::Mssql => c
            .database()
            .map_or_else(|| c.display_target(), str::to_owned),
        Engine::Surrealdb => format!(
            "{}/{}",
            c.namespace().unwrap_or_default(),
            c.database().unwrap_or_default()
        ),
        Engine::Sqlite
        | Engine::Duckdb
        | Engine::Mongodb
        | Engine::Redis
        | Engine::Dynamodb
        | _ => c.display_target(),
    };
    format!(
        "{} \u{b7} {} \u{b7} {target}",
        c.name(),
        engine_label(c.engine())
    )
}

/// The grammar the run-confirmation prompt highlights `engine`'s statement with: the editor's,
/// so the same text never colours two ways.
pub fn prompt_language(engine: Engine) -> &'static str {
    editor_language(engine)
}

/// Whether Format SQL applies to `engine`: only SQL engines, since the formatter knows no
/// other language.
pub fn formats(engine: Engine) -> bool {
    engine.capabilities().language == QueryLanguage::Sql
}

/// The highlighter language for `engine`'s query editor: "sql" for an SQL engine and PartiQL,
/// "javascript" for the MongoDB shell, "bash" for Redis command lines (close enough to colour
/// words and quotes), "text" (plain, no highlighting) for SurrealQL.
pub fn editor_language(engine: Engine) -> &'static str {
    match engine.capabilities().language {
        // PartiQL's `<<`, `>>` and `{}` fall outside the SQL grammar; its keywords still colour.
        QueryLanguage::Sql | QueryLanguage::PartiQl => "sql",
        QueryLanguage::MongoShell => "javascript",
        QueryLanguage::RedisCommands => "bash",
        QueryLanguage::SurrealQl | _ => "text",
    }
}

/// The editor's right-click menu in order: Run, Format and Save with their effective key
/// chips, then the clipboard items, whose keys the kit binds and the menu does not repeat.
/// Cut and Copy are enabled only when `selected` (the editor holds a non-empty selection);
/// both explain entries appear only when `explain` (the engine's `capabilities().explain`),
/// and Format only when `format` (the engine's query language is SQL).
pub fn editor_menu_entries(
    selected: bool,
    explain: bool,
    format: bool,
    cx: &App,
) -> Vec<MenuEntry> {
    let entry = |id, label, keys| MenuEntry {
        id,
        label,
        enabled: selected || !matches!(id, "editor-menu-cut" | "editor-menu-copy"),
        keys,
    };
    let mut entries = vec![entry(
        "editor-menu-run",
        "Run statement",
        keys_for(&RunStatement, cx),
    )];
    if format {
        entries.push(entry(
            "editor-menu-format",
            "Format SQL",
            keys_for(&FormatSql, cx),
        ));
    }
    if explain {
        entries.push(entry(
            "editor-menu-explain",
            "Explain plan",
            keys_for(&Explain, cx),
        ));
        entries.push(entry(
            "editor-menu-explain-analyze",
            "Explain analyze",
            keys_for(&ExplainAnalyze, cx),
        ));
    }
    entries.extend([
        entry("editor-menu-save", "Save query", keys_for(&SaveQuery, cx)),
        entry("editor-menu-cut", "Cut", None),
        entry("editor-menu-copy", "Copy", None),
        entry("editor-menu-paste", "Paste", None),
        entry("editor-menu-select-all", "Select all", None),
    ]);
    entries
}

/// How the workspace's config for a tab's connection compares with the one its session uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    /// The session runs on the current settings.
    Current,
    /// Session fields differ from the ones the tab's runner uses.
    Changed {
        /// `read_only` went from false to true, so the session still allows writes.
        still_read_write: bool,
    },
    /// The connection is no longer in dbento.toml.
    Gone,
}

/// What a query tab asks of the shell.
pub enum QueryTabEvent {
    /// The user asked to reconnect on the workspace's current settings.
    Reconnect,
    /// The read-only note asked to open this table, on the tab's connection, to edit it.
    OpenTable(RelationRef),
    /// The connection picker chose this connection for the tab.
    SwitchConnection(String),
    /// The connect error asked to open the session again, on the current dbento.toml.
    Retry,
    /// The connect error asked to edit the tab's connection.
    EditConnection,
}

/// One query tab over a connection.
pub struct QueryTab {
    model: Entity<TabModel>,
    // The workspace's config, for what the tab shows; the session's, for what it does.
    conn: ConnectionConfig,
    session_conn: ConnectionConfig,
    state: ConnState,
    // The current dbento.toml, which the connection picker lists.
    workspace: Arc<Workspace>,
    // The explorer whose catalog feeds completion and the read-only note, by connection name.
    sidebar: Option<WeakEntity<Sidebar>>,
    editor: Entity<EditorState>,
    // The completion list the editor's provider fills and the tab draws over the editor.
    completion: Entity<CompletionPopup>,
    results: Entity<ResultsPane>,
    /// The `(run_id, stmt)` whose dialog is on screen, so a re-render never asks twice.
    asking: Option<(u64, usize)>,
    /// The id of the apply prompt whose dialog is on screen.
    prompting: Option<u64>,
    /// The id of the discard question whose dialog is on screen.
    discarding: Option<u64>,
    // The text this tab last matched on disk or was generated with; empty for a fresh tab.
    baseline: String,
    // The saved query this tab edits, relative to the workspace; None until first saved.
    file: Option<PathBuf>,
    // The file's text when this tab last read or wrote it, so a save can see an outside change.
    disk: Option<String>,
    // Where this tab's edited text is kept; None for a tab that never writes a draft.
    draft: Option<DraftSlot>,
    // The pending debounce; replacing or dropping it cancels the write it would make.
    draft_task: Option<Task<()>>,
    // The editor-over-results split, and the editor height it starts at when set.
    split: Entity<ResizableState>,
    start_height: Option<Pixels>,
    // The "Query N" an untitled tab is named by, and the relation a table tab was opened on.
    number: Option<usize>,
    origin: Option<RelationRef>,
    // The user's name for the tab, which replaces "Query N" or "schema.name".
    label: Option<String>,
    // A Redis value tab's title, `<key> · db N`; set, the tab shows the value, not the editor.
    key_title: Option<String>,
    // The tab a reference was followed from while this tab is its preview.
    preview_of: Option<EntityId>,
    // The reference hops this tab walked, for Back and Forward.
    nav: NavHistory,
    // The last format or comment toggle, so undoing it can put back the caret it replaced.
    replace_undo: Option<ReplaceUndo>,
    _observe: Subscription,
    _editor_changes: Subscription,
    _browse: Subscription,
    _open_table: Subscription,
    _catalog: Option<Subscription>,
    _connect_error: [Subscription; 2],
}

// What one Format SQL or comment toggle replaced: the text and selection before it, and the
// range it selected for the replace.
struct ReplaceUndo {
    pre_text: String,
    pre_selection: Range<usize>,
    range: Range<usize>,
}

// A draft write or removal, ready to run off the main thread.
type DraftOp = Box<dyn FnOnce() -> std::io::Result<()> + Send>;

impl QueryTab {
    /// A tab over `model` for `conn`, with the editor focused. With `sidebar`, the editor
    /// completes from its catalog cache; with `draft`, edited text is kept as that draft.
    pub fn new(
        model: Entity<TabModel>,
        conn: ConnectionConfig,
        sidebar: Option<WeakEntity<Sidebar>>,
        draft: Option<DraftSlot>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor =
            cx.new(|cx| EditorState::new(window, cx).language(editor_language(conn.engine())));
        let completion = cx.new(|cx| CompletionPopup::new(&editor, cx));
        if let Some(sidebar) = sidebar.clone() {
            Self::complete_from(&editor, &completion, sidebar, &conn, cx);
        }
        editor.update(cx, |e, cx| e.focus(window, cx));
        let results = cx.new(|cx| {
            let mut pane = ResultsPane::new(model.clone(), window, cx);
            pane.attach_editor(&editor, cx);
            if let Some(sidebar) = sidebar.clone() {
                pane.set_catalog(sidebar, conn.name().to_owned());
            }
            pane
        });
        let observe = cx.observe_in(&model, window, |this, _, window, cx| {
            this.sync_confirm(window, cx);
            this.sync_apply(window, cx);
            this.sync_discard(window, cx);
        });
        // Any user edit leaves the browse SQL behind, so the tab stops browsing for good,
        // once staged edits are discarded.
        let editor_changes = cx.subscribe(&editor, |this, editor, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                this.restore_replace_caret(&editor, cx);
                this.model.update(cx, |m, cx| m.request_clear_browse(cx));
                this.schedule_draft(cx);
            }
        });
        // The editor shows exactly what a browse run executes; `set_sql` emits no Change, so
        // writing it keeps the state. A table that cannot be described runs its plain SQL and
        // says why on a toast.
        let browse = cx.subscribe_in(
            &model,
            window,
            |this, _, e: &BrowseEvent, window, cx| match e {
                BrowseEvent::Run(sql) => {
                    this.set_sql(sql, window, cx);
                    this.mark_saved(cx);
                    this.run_all(cx);
                }
                BrowseEvent::Unavailable(reason) => {
                    let text = unavailable_text(reason);
                    let kind = ui::NoticeKind::Warning;
                    ui::notice(window, cx, ui::Notice { text, kind });
                    this.run_all(cx);
                }
                BrowseEvent::Committed { changes, relation } => {
                    let text = staged::committed_text(*changes, relation);
                    let kind = ui::NoticeKind::Success;
                    ui::notice(window, cx, ui::Notice { text, kind });
                }
                BrowseEvent::Skipped { rows } => {
                    let text = skip_notice(*rows);
                    let kind = ui::NoticeKind::Info;
                    ui::notice(window, cx, ui::Notice { text, kind });
                }
                BrowseEvent::Cleared => {}
            },
        );
        let open_table = cx.subscribe(&results, |_, _, e: &OpenTable, cx| {
            cx.emit(QueryTabEvent::OpenTable(e.0.clone()));
        });
        // A load the open answer asked for re-asks at the same caret, so the first keystroke's
        // answer fills in without waiting for the next one.
        let catalog = sidebar
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .map(|sidebar| {
                cx.observe_in(&sidebar, window, |this, _, window, cx| {
                    if this.completion.read(cx).awaits_catalog(window, cx) {
                        this.editor
                            .update(cx, |e, cx| e.show_completions(window, cx));
                    }
                })
            });
        let connect_error = [
            cx.subscribe(&results, |_, _, _: &RetryConnect, cx| {
                cx.emit(QueryTabEvent::Retry)
            }),
            cx.subscribe(&results, |_, _, _: &EditConnection, cx| {
                cx.emit(QueryTabEvent::EditConnection)
            }),
        ];
        let workspace = Arc::clone(&model.read(cx).spec().workspace);
        QueryTab {
            workspace,
            completion,
            model,
            session_conn: conn.clone(),
            conn,
            state: ConnState::Current,
            sidebar,
            editor,
            results,
            asking: None,
            prompting: None,
            discarding: None,
            baseline: String::new(),
            file: None,
            disk: None,
            draft,
            draft_task: None,
            split: cx.new(|_| ResizableState::default()),
            start_height: None,
            number: None,
            origin: None,
            label: None,
            key_title: None,
            preview_of: None,
            replace_undo: None,
            nav: NavHistory::default(),
            _observe: observe,
            _editor_changes: editor_changes,
            _browse: browse,
            _open_table: open_table,
            _catalog: catalog,
            _connect_error: connect_error,
        }
    }

    // Points the editor's completion at `conn`'s catalog in the explorer, and its highlighter at
    // `conn`'s language, so a tab moved to another engine stops or starts reading SQL.
    fn complete_from(
        editor: &Entity<EditorState>,
        popup: &Entity<CompletionPopup>,
        sidebar: WeakEntity<Sidebar>,
        conn: &ConnectionConfig,
        cx: &mut gpui_kit::App,
    ) {
        let (name, engine) = (conn.name().to_owned(), conn.engine());
        let default = dbento_core::default_schema(engine, conn);
        let provider =
            SqlCompletionProvider::new(sidebar, popup.downgrade(), name, engine, default);
        let lang = editor_language(engine);
        editor.update(cx, |e, cx| {
            e.lsp_mut().completion_provider = Some(std::rc::Rc::new(provider));
            if e.language_name() != lang {
                e.set_highlighter(lang, cx);
            }
        });
    }

    /// Re-reads the model's connection: `conn`, `session_conn`, state Current, the completion
    /// provider, then schedules the draft (its connection changed).
    pub fn follow_connection(&mut self, cx: &mut Context<Self>) {
        let spec = self.model.read(cx).spec();
        let Some(c) = spec
            .workspace
            .connection(&spec.connection)
            .map(Cow::into_owned)
        else {
            return;
        };
        if let Some(sidebar) = self.sidebar.clone() {
            Self::complete_from(&self.editor, &self.completion, sidebar.clone(), &c, cx);
            let name = c.name().to_owned();
            self.results.update(cx, |p, _| p.set_catalog(sidebar, name));
        }
        self.conn = c.clone();
        self.session_conn = c;
        self.state = ConnState::Current;
        self.schedule_draft(cx);
        cx.notify();
    }

    /// Replaces the editor's text.
    pub fn set_sql(&mut self, sql: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.editor
            .update(cx, |e, cx| e.set_value(sql.to_owned(), window, cx));
        // set_value emits no Change event, so the draft is scheduled here.
        self.schedule_draft(cx);
    }

    /// The editor's text.
    pub fn sql(&self, cx: &gpui_kit::App) -> String {
        self.editor.read(cx).value().to_string()
    }

    /// Whether the editor holds SQL that closing the tab would lose: its trimmed text is
    /// non-empty and differs from the baseline.
    pub fn edited(&self, cx: &gpui_kit::App) -> bool {
        let sql = self.sql(cx);
        let sql = sql.trim();
        !sql.is_empty() && sql != self.baseline.trim()
    }

    /// Takes the current text as the baseline, so the tab reads as unedited.
    pub fn mark_saved(&mut self, cx: &mut Context<Self>) {
        self.baseline = self.sql(cx);
        self.schedule_draft(cx);
    }

    /// The saved query this tab edits, relative to the workspace.
    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }

    /// Ties the tab to the saved query `rel`, whose file now holds `disk`, and takes the
    /// current text as saved.
    pub fn set_file(&mut self, rel: PathBuf, disk: String, cx: &mut Context<Self>) {
        self.file = Some(rel);
        self.disk = Some(disk);
        self.mark_saved(cx);
    }

    /// The file's text when this tab last read or wrote it.
    pub fn disk(&self) -> Option<&str> {
        self.disk.as_deref()
    }

    /// Unties the tab from its saved query, which another tab now owns: it keeps its text as
    /// an edited tab with an empty baseline, and its draft follows.
    pub fn unbind_file(&mut self, cx: &mut Context<Self>) {
        self.file = None;
        self.disk = None;
        self.baseline.clear();
        self.schedule_draft(cx);
        cx.notify();
    }

    /// The id of this tab's draft, when it keeps one.
    pub fn draft_id(&self) -> Option<&str> {
        self.draft.as_ref().map(DraftSlot::id)
    }

    /// Writes the draft now when edited or labelled, else removes it; Err only when such a
    /// tab's draft could not be written. Cancels any pending debounce.
    pub fn flush_draft(&mut self, cx: &mut Context<Self>) -> std::io::Result<()> {
        self.draft_task = None;
        let writes = self.edited(cx) || self.label.is_some();
        let Some(op) = self.draft_op(cx) else {
            return Ok(());
        };
        match op() {
            Err(e) if !writes => {
                eprintln!("dbento: cannot remove a draft: {e}");
                Ok(())
            }
            result => result,
        }
    }

    /// Removes the draft now, for a tab the user closes; a failure is logged.
    pub fn discard_draft(&mut self, cx: &mut Context<Self>) {
        self.draft_task = None;
        let Some(slot) = &self.draft else {
            return;
        };
        if let Err(e) = slot.remove_later()() {
            eprintln!("dbento: cannot remove a draft: {e}");
        }
        cx.notify();
    }

    // After the text rests for DRAFT_DEBOUNCE, writes or removes the draft off the main
    // thread. The slot orders the operations, so a late one never undoes a newer one.
    fn schedule_draft(&mut self, cx: &mut Context<Self>) {
        if self.draft.is_none() {
            return;
        }
        self.draft_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DRAFT_DEBOUNCE).await;
            let Ok(Some(op)) = this.update(cx, |this, cx| this.draft_op(cx)) else {
                return;
            };
            cx.background_spawn(async move {
                if let Err(e) = op() {
                    eprintln!("dbento: cannot save a draft: {e}");
                }
            })
            .detach();
        }));
    }

    // The operation that brings the draft in line with the tab: a write when edited or
    // labelled, else a removal. It takes its place in the slot's order now.
    fn draft_op(&self, cx: &gpui_kit::App) -> Option<DraftOp> {
        let slot = self.draft.as_ref()?;
        if !self.edited(cx) && self.label.is_none() {
            return Some(Box::new(slot.remove_later()));
        }
        let spec = self.model.read(cx).spec();
        let draft = Draft::new(
            slot.id(),
            spec.workspace.dir(),
            &spec.connection,
            self.file.as_deref(),
            &self.sql(cx),
        )
        .with_label(self.label.as_deref());
        Some(Box::new(slot.write_later(draft)))
    }

    /// The tab strip's name for this tab: the file name when file-backed, else its label,
    /// else "schema.name" for a table tab, else "Query N".
    pub fn title(&self) -> String {
        if let Some(f) = self.file.as_deref().and_then(Path::file_name) {
            return f.to_string_lossy().into_owned();
        }
        if let Some(label) = &self.label {
            return label.clone();
        }
        if let Some(key) = &self.key_title {
            return key.clone();
        }
        match (&self.origin, self.number) {
            (Some(rel), _) => format!("{}.{}", rel.schema(), rel.name()),
            (None, Some(n)) => format!("Query {n}"),
            // Only between `push_tab` and its caller numbering the tab; no frame paints it.
            (None, None) => "Query".to_owned(),
        }
    }

    /// Turns this tab into Redis key `key`'s value view in DB `db`: the editor gives way to
    /// the key's header and paged grid, and the tab keeps no draft, having no text to keep.
    pub fn open_key(&mut self, db: u32, key: Vec<u8>, cx: &mut Context<Self>) {
        self.key_title = Some(KeyView::new(db, key.clone()).title());
        self.draft = None;
        self.draft_task = None;
        self.model.update(cx, |m, cx| m.open_key(db, key, cx));
        cx.notify();
    }

    /// The user's name for the tab, if any.
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// Sets the user's name for the tab, notifies and schedules a draft write.
    pub fn set_label(&mut self, label: Option<String>, cx: &mut Context<Self>) {
        self.label = label;
        self.schedule_draft(cx);
        cx.notify();
    }

    /// Names this tab "Query `n`" while it has no file and no origin.
    pub fn set_number(&mut self, n: usize) {
        self.number = Some(n);
    }

    /// The number this tab was given, if any.
    pub fn number(&self) -> Option<usize> {
        self.number
    }

    /// Marks this tab as opened on relation `rel`, which names it.
    pub fn set_origin(&mut self, rel: RelationRef) {
        self.origin = Some(rel);
    }

    /// The relation this tab was opened on, if any.
    pub fn origin(&self) -> Option<&RelationRef> {
        self.origin.as_ref()
    }

    /// The tab this preview was opened from; None for an ordinary (pinned) tab.
    pub fn preview_of(&self) -> Option<EntityId> {
        self.preview_of
    }

    /// Whether this tab is a preview that the next follow from its origin replaces.
    pub fn is_preview(&self) -> bool {
        self.preview_of.is_some()
    }

    /// Makes this tab the preview of tab `origin`.
    pub(crate) fn set_preview_of(&mut self, origin: EntityId) {
        self.preview_of = Some(origin);
    }

    /// The reference hops this tab walked.
    pub fn nav(&self) -> &NavHistory {
        &self.nav
    }

    /// The reference hops, for a follow to push onto or Back and Forward to step.
    pub fn nav_mut(&mut self) -> &mut NavHistory {
        &mut self.nav
    }

    /// Keeps this tab: no later follow replaces it.
    pub fn pin(&mut self, cx: &mut Context<Self>) {
        if self.preview_of.take().is_some() {
            cx.notify();
        }
    }

    /// Takes `ws`'s config for display and recomputes the state against the runner's workspace
    /// (`model.spec().workspace`), whose config becomes `session_conn`.
    pub fn set_workspace(&mut self, ws: &Arc<Workspace>, cx: &mut Context<Self>) {
        self.workspace = Arc::clone(ws);
        let spec = self.model.read(cx).spec();
        if let Some(session) = spec.workspace.connection(&spec.connection) {
            self.session_conn = session.into_owned();
        }
        // A gone connection keeps its last display config, so the title and colour stay.
        self.state = match ws.connection(self.session_conn.name()) {
            None => ConnState::Gone,
            Some(c) => {
                let state = if c.same_session(&self.session_conn) {
                    ConnState::Current
                } else {
                    ConnState::Changed {
                        still_read_write: c.read_only() && !self.session_conn.read_only(),
                    }
                };
                self.conn = c.into_owned();
                state
            }
        };
        cx.notify();
    }

    /// Display: production band, toolbar tint, tab colour, title.
    pub fn conn(&self) -> &ConnectionConfig {
        &self.conn
    }

    /// The session's: read-only badge, engine for `run_range` and completion, confirm titles.
    pub fn session_conn(&self) -> &ConnectionConfig {
        &self.session_conn
    }

    /// How the workspace's config compares with the session's.
    pub fn conn_state(&self) -> ConnState {
        self.state
    }

    /// The SQL editor's state.
    pub fn editor(&self) -> &Entity<EditorState> {
        &self.editor
    }

    /// The editor's completion popup.
    pub fn completion(&self) -> &Entity<CompletionPopup> {
        &self.completion
    }

    /// The state of the editor-over-results split.
    pub fn split_state(&self) -> &Entity<ResizableState> {
        &self.split
    }

    /// Opens the editor at height `h` above the results, for a tab not yet shown.
    pub fn set_editor_height(&mut self, h: Pixels) {
        self.start_height = Some(h);
    }

    /// The editor's height as last laid out, else the height it starts at; None when neither
    /// is known.
    pub fn editor_height(&self, cx: &gpui_kit::App) -> Option<f32> {
        let laid_out = self.split.read(cx).sizes().first().copied();
        laid_out.or(self.start_height).map(f32::from)
    }

    /// The model behind this tab.
    pub fn model(&self) -> &Entity<TabModel> {
        &self.model
    }

    /// The results pane under the editor.
    pub fn results_pane(&self) -> &Entity<ResultsPane> {
        &self.results
    }

    /// Runs the whole editor text.
    pub fn run_all(&mut self, cx: &mut Context<Self>) {
        self.dismiss_completion(cx);
        let sql = self.editor.read(cx).value().to_string();
        self.model.update(cx, |m, cx| m.request_run(sql, cx));
    }

    /// Runs the selection, else the statement at the caret; an editor without a statement
    /// runs nothing.
    pub fn run_statement(&mut self, cx: &mut Context<Self>) {
        self.dismiss_completion(cx);
        let e = self.editor.read(cx);
        let text = e.value();
        let Some(range) = run_range(self.session_conn.engine(), &text, e.selected_range()) else {
            return;
        };
        let (sql, start) = (text[range.clone()].to_owned(), range.start);
        self.model
            .update(cx, |m, cx| m.request_run_at(sql, start, cx));
    }

    /// Explains the selection, else the statement at the caret, in `mode`. A statement that
    /// cannot be explained shows a warning and sends nothing; no statement does nothing.
    pub fn explain(&mut self, mode: ExplainMode, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_completion(cx);
        let engine = self.session_conn.engine();
        let e = self.editor.read(cx);
        let text = e.value();
        let Some(range) = run_range(engine, &text, e.selected_range()) else {
            return;
        };
        match explain_request(engine, &text[range.clone()], mode) {
            Ok(request) => {
                self.model
                    .update(cx, |m, cx| m.request_explain(request, range.start, cx));
            }
            Err(err) => {
                let (text, kind) = (err.to_string(), ui::NoticeKind::Warning);
                ui::notice(window, cx, ui::Notice { text, kind });
            }
        }
    }

    /// The one save path for Ctrl+S and the toolbar icon: stages open cell and field editors,
    /// then saves the staged edits. Returns false when nothing is staged, leaving the caller
    /// to save the query instead.
    fn save_edits(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.results
            .update(cx, |p, cx| p.stage_open_editors(window, cx));
        if self.model.read(cx).edits().is_empty() {
            return false;
        }
        self.model.update(cx, |m, cx| m.save(cx));
        true
    }

    /// Formats the selection, else the statement at the caret, as one undo step. Never runs
    /// on its own: only the keys, the toolbar button and the editor menu call it.
    pub fn format_sql(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_completion(cx);
        if !formats(self.session_conn.engine()) {
            return;
        }
        let e = self.editor.read(cx);
        let edit = format_edit(self.session_conn.engine(), &e.value(), e.selected_range());
        let Some((range, new)) = edit else {
            return;
        };
        let (pre_text, pre_selection) = (e.value().to_string(), e.selected_range());
        // Selecting then `replace` keeps history; `set_value` would wipe it.
        self.editor.update(cx, |e, cx| {
            e.set_selected_range(range.clone(), cx);
            e.replace(new, window, cx);
        });
        self.replace_undo = Some(ReplaceUndo {
            pre_text,
            pre_selection,
            range,
        });
    }

    /// Comments out the selected lines, or the caret's, with the tab language's line-comment
    /// marker, else uncomments them; one undo step. A language without one does nothing.
    pub fn toggle_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_completion(cx);
        let language = self.session_conn.engine().capabilities().language;
        let Some(marker) = language.line_comment() else {
            return;
        };
        let e = self.editor.read(cx);
        let (pre_text, pre_selection) = (e.value().to_string(), e.selected_range());
        let Some(edit) = toggle_line_comments(&pre_text, pre_selection.clone(), marker) else {
            return;
        };
        self.editor.update(cx, |e, cx| {
            e.set_selected_range(edit.range.clone(), cx);
            e.replace(edit.text, window, cx);
            e.set_selected_range(edit.selection, cx);
        });
        self.replace_undo = Some(ReplaceUndo {
            pre_text,
            pre_selection,
            range: edit.range,
        });
    }

    // Undoing a format or comment toggle reselects the range `replace` was given, so put the
    // selection from before it back. Remove once gpui-base has a public replace-range API that
    // records the real pre-edit selection rather than the range selected for the replace.
    fn restore_replace_caret(&mut self, editor: &Entity<EditorState>, cx: &mut Context<Self>) {
        let Some(undo) = &self.replace_undo else {
            return;
        };
        // Only an undo of the replace yields its old text with exactly that range selected.
        let e = editor.read(cx);
        if *e.value() == *undo.pre_text && e.selected_range() == undo.range {
            let len = undo.pre_text.len();
            let at = undo.pre_selection.start.min(len)..undo.pre_selection.end.min(len);
            editor.update(cx, |e, cx| e.set_selected_range(at, cx));
        }
    }

    // Runs one editor menu item; Save and the clipboard focus the editor first so the
    // dispatched action reaches it, as the toolbar's Save does.
    fn editor_menu_action(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::base::input::{Copy, Cut, Paste, SelectAll};
        let action: Box<dyn gpui_kit::Action> = match id {
            "editor-menu-run" => return self.run_statement(cx),
            "editor-menu-format" => return self.format_sql(window, cx),
            "editor-menu-explain" => return self.explain(ExplainMode::Plan, window, cx),
            "editor-menu-explain-analyze" => {
                return self.explain(ExplainMode::Analyze, window, cx);
            }
            "editor-menu-save" => Box::new(SaveQuery),
            "editor-menu-cut" => Box::new(Cut),
            "editor-menu-copy" => Box::new(Copy),
            "editor-menu-paste" => Box::new(Paste),
            "editor-menu-select-all" => Box::new(SelectAll),
            _ => return,
        };
        self.editor.update(cx, |e, cx| e.focus(window, cx));
        window.dispatch_action(action, cx);
    }

    // The run keys bypass the kit's Enter, which would otherwise close an open menu.
    fn dismiss_completion(&mut self, cx: &mut Context<Self>) {
        self.completion.update(cx, |p, cx| p.close(cx));
    }

    // Runs `f` on the completion popup when it is open over the editor, and keeps the key
    // from the editor; a closed popup lets the key through.
    fn popup_key(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut CompletionPopup, &mut Window, &mut Context<CompletionPopup>),
    ) {
        if self.completion.read(cx).is_live(window, cx) {
            cx.stop_propagation();
            self.completion.update(cx, |p, cx| f(p, window, cx));
        }
    }

    /// Stops the run; one waiting on its question skips that statement and ends the script.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.model.update(cx, |m, cx| {
            m.cancel();
            cx.notify();
        });
    }

    /// Opens the dialog for a newly pending statement, and closes one whose question went
    /// away unanswered (cancelled, replaced by a new run, or the runner stopped).
    fn sync_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pending = self.model.read(cx).confirm().map(|p| {
            (
                (p.run_id, p.stmt),
                p.kinds.clone(),
                p.analyze,
                p.sql.clone(),
            )
        });
        let key = pending.as_ref().map(|(key, ..)| *key);
        if self.asking.is_some() && self.asking != key {
            self.asking = None;
            window.close_dialog(cx);
        }
        let Some((key, kinds, analyze, sql)) = pending else {
            return;
        };
        if self.asking == Some(key) {
            return;
        }
        self.asking = Some(key);
        let connection = self.session_conn.name();
        let spec = if analyze {
            analyze_ask(&kinds, connection, &sql)
        } else {
            let language = prompt_language(self.session_conn.engine());
            unguarded_ask(kinds.first().copied(), connection, &sql, language)
        };
        let reply = ask(spec, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            // A dialog that closes unanswered counts as Cancel, which stops the run.
            let run = reply.recv().await == Ok(1);
            let _ = this.update(cx, |this, cx| {
                if this.asking == Some(key) {
                    this.asking = None;
                }
                // Answer only the question this dialog asked; a later one has its own.
                this.model.update(cx, |m, cx| {
                    if m.confirm().map(|p| (p.run_id, p.stmt)) == Some(key) {
                        m.answer(run, cx);
                    }
                });
            });
        })
        .detach();
    }
}

impl QueryTab {
    /// Whether this tab's Commit or Roll back dialog is on screen.
    pub fn apply_dialog_open(&self) -> bool {
        self.prompting.is_some()
    }

    /// Asks Commit or Roll back once per new apply prompt. Enter commits; Escape, Roll back
    /// and a dialog closed unanswered all roll back, and only while that prompt still stands.
    fn sync_apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.model.read(cx).apply_prompt().map(|p| p.id);
        if self.prompting.is_some() && self.prompting != current {
            self.prompting = None;
            window.close_dialog(cx);
        }
        let Some(prompt) = self.model.read(cx).apply_prompt() else {
            return;
        };
        if self.prompting == Some(prompt.id) {
            return;
        }
        let id = prompt.id;
        let spec = Ask {
            title: prompt.title.clone().into(),
            detail: Some(prompt.detail.clone().into()),
            sql: Some(prompt.sql.clone().into()),
            language: "sql",
            choices: vec![
                Choice {
                    id: "apply-rollback",
                    label: "Roll back".into(),
                    danger: false,
                },
                Choice {
                    id: "apply-commit",
                    label: "Commit".into(),
                    danger: false,
                },
            ],
            default: 1,
            cancel: 0,
        };
        self.prompting = Some(id);
        let reply = ask(spec, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let commit = reply.recv().await == Ok(1);
            let _ = this.update(cx, |this, cx| {
                if this.prompting == Some(id) {
                    this.prompting = None;
                }
                this.model.update(cx, |m, cx| {
                    if m.apply_prompt().map(|p| p.id) == Some(id) {
                        if commit {
                            m.commit(cx);
                        } else {
                            m.rollback(cx);
                        }
                    }
                });
            });
        })
        .detach();
    }
}

impl QueryTab {
    /// Asks Keep or Discard once per discard question, and closes a dialog whose question
    /// went away unanswered. Keep after typing writes the browse SQL back; Keep after a
    /// filter puts the applied filter back into its input.
    fn sync_discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pending = self
            .model
            .read(cx)
            .discard_question()
            .map(|q| (q.id, q.title.clone()));
        let current = pending.as_ref().map(|(id, _)| *id);
        if self.discarding.is_some() && self.discarding != current {
            self.discarding = None;
            window.close_dialog(cx);
        }
        let Some((id, title)) = pending else {
            return;
        };
        if self.discarding == Some(id) {
            return;
        }
        self.discarding = Some(id);
        let spec = Ask {
            title: title.into(),
            detail: Some("They have not been saved to the database.".into()),
            sql: None,
            language: "sql",
            choices: vec![
                Choice {
                    id: "discard-keep",
                    label: "Keep".into(),
                    danger: false,
                },
                Choice {
                    id: "discard-edits",
                    label: "Discard".into(),
                    danger: true,
                },
            ],
            default: 0,
            cancel: 0,
        };
        let reply = ask(spec, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let discard = reply.recv().await == Ok(1);
            let _ = this.update_in(cx, |this, window, cx| {
                if this.discarding == Some(id) {
                    this.discarding = None;
                }
                let answered = this
                    .model
                    .update(cx, |m, cx| m.answer_discard(id, discard, cx));
                match answered {
                    Some(Discard::Edit) if !discard => {
                        let sql = this.model.read(cx).browse().map(|b| b.sql().to_owned());
                        if let Some(sql) = sql {
                            this.set_sql(&sql, window, cx);
                            this.mark_saved(cx);
                        }
                    }
                    Some(Discard::Filter(_)) if !discard => {
                        this.results.update(cx, |p, cx| p.reset_filter(window, cx));
                    }
                    _ => {}
                }
            });
        })
        .detach();
    }
}

impl EventEmitter<QueryTabEvent> for QueryTab {}

impl QueryTab {
    // The note that the session no longer matches dbento.toml, with Reconnect when the
    // connection still exists; None while the session is current.
    fn chip(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let text = match self.state {
            ConnState::Current => return None,
            ConnState::Changed {
                still_read_write: true,
            } => "Settings changed: still read-write until you reconnect",
            ConnState::Changed { .. } => "Settings changed",
            ConnState::Gone => "Not in dbento.toml",
        };
        let warning = cx.theme().warning;
        let reconnect = matches!(self.state, ConnState::Changed { .. }).then(|| {
            Button::new("conn-reconnect")
                .outline()
                .compact()
                .label("Reconnect")
                .on_click(cx.listener(|_, _, _, cx| cx.emit(QueryTabEvent::Reconnect)))
        });
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .child(
                    ui::chip("conn-chip", text, cx)
                        .border_color(warning)
                        .text_color(warning),
                )
                .children(reconnect),
        )
    }
}

impl QueryTab {
    // The chip naming the tab's connection. It opens a picker over the current dbento.toml's
    // connections, except on a browsing or closed tab, which cannot switch.
    fn connection_chip(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let text: SharedString = connection_chip_text(&self.conn).into();
        let dot = self.conn.effective_color().map(|c| {
            div()
                .size(px(8.))
                .flex_none()
                .rounded_full()
                .bg(tag_color(c))
        });
        let m = self.model.read(cx);
        if m.closed() || m.browses() {
            return ui::chip_base(cx)
                .id("query-connection")
                .aria_label(text.clone())
                .test_support()
                .children(dot)
                .child(text)
                .into_any_element();
        }
        let rows: Vec<PickRow> = self
            .workspace
            .connections()
            .iter()
            .map(|c| PickRow {
                name: c.name().to_owned(),
                engine: engine_label(c.engine()),
                dot: c.effective_color().map(tag_color),
            })
            .collect();
        let current = m.spec().connection.clone();
        let tab = cx.entity().downgrade();
        let theme = cx.theme();
        let (radius, border, popover) = (theme.radius, theme.border, theme.popover);
        Button::new("query-connection")
            .ghost()
            .xsmall()
            .h(px(22.))
            .px_2()
            .rounded(radius)
            .border_1()
            .border_color(border)
            .bg(popover)
            .accessibility_label(text.clone())
            .children(dot)
            .child(text)
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| connection_menu(menu, &tab, &rows, &current))
            .into_any_element()
    }
}

// One picker row: the connection's name, its engine and its tag colour, as the chip shows them.
struct PickRow {
    name: String,
    engine: &'static str,
    dot: Option<Hsla>,
}

// A 28 px square ghost toolbar button holding a 16 px `glyph` in `muted`, named `label`;
// it matches the split Run's height.
fn tool_button(
    id: &'static str,
    glyph: gpui_kit::assets::IconName,
    label: &'static str,
    muted: Hsla,
) -> Button {
    Button::new(id)
        .ghost()
        .size(px(28.))
        .p_0()
        .child(ui::icon(glyph, px(16.), muted))
        .accessibility_label(label)
}

// One row per connection, checked on the tab's own; picking another asks the shell to switch.
fn connection_menu(
    menu: PopupMenu,
    tab: &WeakEntity<QueryTab>,
    rows: &[PickRow],
    current: &str,
) -> PopupMenu {
    rows.iter().enumerate().fold(menu, |menu, (n, row)| {
        let label: SharedString = row.name.clone().into();
        let (engine, dot) = (row.engine, row.dot);
        let (tab, pick) = (tab.clone(), row.name.clone());
        let item = PopupMenuItem::element(move |_, cx| {
            let dot = dot.map(|color| {
                div()
                    .id(("query-connection-pick-dot", n))
                    .test_support()
                    .size(px(8.))
                    .flex_none()
                    .rounded_full()
                    .bg(color)
            });
            div()
                .flex()
                .flex_1()
                .items_center()
                .gap_2()
                .children(dot)
                .child(label.clone())
                .child(
                    div()
                        .id(("query-connection-pick-engine", n))
                        .aria_label(engine)
                        .test_support()
                        .text_color(theme::dim(cx))
                        .child(engine),
                )
                .id(("query-connection-pick", n))
                .aria_label(label.clone())
                .test_support()
        })
        .checked(row.name == current)
        .on_click(move |_, _, cx| {
            let _ = tab.update(cx, |_, cx| {
                cx.emit(QueryTabEvent::SwitchConnection(pick.clone()))
            });
        });
        menu.item(item)
    })
}

impl Render for QueryTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dim = theme::dim(cx);
        let theme = cx.theme();
        let (danger, border) = (theme.danger, theme.border);
        let (primary, primary_fg) = (theme.primary, theme.primary_foreground);
        let muted = theme.muted_foreground;
        let (mono, mono_size) = (theme.mono_font_family.clone(), theme.mono_font_size);
        // Cancel also stops a parked result, interrupts a drain and stops a table's load.
        let m = self.model.read(cx);
        let cancellable = m.running()
            || m.paused().is_some()
            || m.parked_at_cap().is_some()
            || m.settling()
            || m.target_pending().is_some();
        // A closed tab only waits for its session to go, so nothing here may act.
        let closed = m.closed();
        // Explain shows only where the session's engine has it; its keys still explain why not.
        let explain = self.session_conn.engine().capabilities().explain;
        let format = formats(self.session_conn.engine());
        let staged = m.edits().len();
        let key_mode = self.key_title.is_some();
        // A key tab shows its value's header, last error and whether pages remain.
        let key = self.key_title.is_some().then(|| {
            let view = m.key_view();
            (
                view.and_then(KeyView::header).map(str::to_owned),
                view.and_then(KeyView::error).map(str::to_owned),
                view.is_some_and(KeyView::has_more),
            )
        });
        // The summary appears once a statement has ended; its timing lives in the result footer.
        let summary = run_elapsed(m.results()).map(|_| {
            let n = m.results().len();
            let s = if n == 1 { "" } else { "s" };
            format!("{n} statement{s}")
        });
        let read_only = crate::link::shows_read_only(&self.session_conn, m.read_only());
        let badge_text: SharedString =
            read_only_badge_text(&self.session_conn, m.read_only_level()).into();
        let production = self.conn.is_production();
        // Production is a loud chip in the toolbar; its id keeps the old band's name.
        let prod_chip = production.then(|| {
            ui::production_chip(cx)
                .id("production-band")
                .aria_label("production")
                .test_support()
        });
        let chip = self.chip(cx);
        let connection = self.connection_chip(cx);
        let badge = read_only.then(|| {
            let tip = badge_text.clone();
            let tooltip = badge_text;
            ui::chip_base(cx)
                .id("read-only-badge")
                .aria_label(tip)
                .test_support()
                .text_color(dim)
                .child(ui::icon(ui::Icons::Lock, px(12.), dim))
                .child("read-only")
                .tooltip(move |window, cx| theme::tooltip(tooltip.clone(), window, cx))
        });
        let run_summary = summary.map(|text| {
            div()
                .id("run-summary")
                .ml_2()
                .aria_label(text.clone())
                .test_support()
                .flex_none()
                .whitespace_nowrap()
                .font_family(mono.clone())
                .text_size(mono_size)
                .text_color(dim)
                .child(text)
        });
        // The Run key hint, spelled from the effective keys; an unbound Run has no chip.
        let run_keys = keys_for(&RunStatement, cx).map(|k| {
            let keys = chip_keys(&k);
            ui::key_chip_colored(keys.clone(), primary_fg, primary_fg.opacity(0.4), cx)
                .id("run-keys")
                .aria_label(keys)
                .test_support()
        });
        // The toolbar sits flat on the panel; its label still names the connection's colour.
        let mut toolbar = div().id("query-toolbar").test_support();
        if let Some(c) = self.conn.effective_color() {
            toolbar = toolbar.aria_label(color_name(c));
        }
        let toolbar = toolbar
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .h(TOOLBAR_HEIGHT)
            .px_2()
            .border_b_1()
            .border_color(border)
            // The connection and its safety badges lead, so the tab says where it runs first.
            .child(connection)
            .children(badge)
            .children(prod_chip)
            // Run and Run all form one split button: square inner corners and a faint
            // divider between them, so the pair reads as two halves of one control. A key tab
            // has no text to run or save, so it shows neither.
            .when(!key_mode, |d| {
                d.child(
                    div()
                        .id("run-split")
                        .test_support()
                        .flex()
                        .flex_none()
                        .items_center()
                        .child(
                            Button::new("run")
                                .primary()
                                .small()
                                .h(px(28.))
                                .px_2()
                                .rounded_tr(px(0.))
                                .rounded_br(px(0.))
                                .icon(ui::Icons::Play)
                                .label("Run")
                                .children(run_keys)
                                .disabled(closed)
                                .on_click(cx.listener(|this, _, _, cx| this.run_statement(cx))),
                        )
                        .child(
                            div()
                                .w(px(1.))
                                .h(px(28.))
                                .bg(primary)
                                // A closed tab's disabled halves draw primary at 15 %, so the
                                // divider fades with them.
                                .when(closed, |d| d.opacity(0.15))
                                .child(div().size_full().bg(primary_fg.opacity(0.4))),
                        )
                        .child(
                            ui::accessibility_id(Button::new("run-all"), "run-all")
                                .primary()
                                .small()
                                .h(px(28.))
                                .px_2()
                                .rounded_tl(px(0.))
                                .rounded_bl(px(0.))
                                .icon(ui::Icons::FastForward)
                                .label("All")
                                .accessibility_label("Run all")
                                .tooltip_with_action("Run all", &RunAll, Some("QueryTab"))
                                .disabled(closed)
                                .on_click(cx.listener(|this, _, _, cx| this.run_all(cx))),
                        ),
                )
            })
            .when(format, |d| {
                // The kit button takes only a text tooltip, so the keyed one sits on a wrapper.
                d.child(
                    div()
                        .id("format-tip")
                        .test_support()
                        .tooltip(|window, cx| {
                            theme::tooltip_keys(
                                format_tooltip(cx),
                                keys_for(&FormatSql, cx),
                                window,
                                cx,
                            )
                        })
                        .child(
                            tool_button("format", ui::Icons::WandSparkles, "Format SQL", muted)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.format_sql(window, cx)),
                                ),
                        ),
                )
            })
            .when(explain, |d| {
                d.child(
                    div()
                        .id("explain-tip")
                        .test_support()
                        .tooltip(|window, cx| {
                            theme::tooltip_keys("Explain plan", keys_for(&Explain, cx), window, cx)
                        })
                        .child(
                            tool_button("explain", ui::Icons::ListTree, "Explain plan", muted)
                                .disabled(closed)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.explain(ExplainMode::Plan, window, cx)
                                })),
                        ),
                )
            })
            // Stop is live only while there is something to stop; its keys work regardless. Idle,
            // a disabled twin holds its place, so Save and the row limit never shift.
            .when(!cancellable, |d| {
                d.child(
                    tool_button("cancel-idle", ui::Icons::CircleStop, "Cancel", muted)
                        .disabled(true),
                )
            })
            .when(cancellable, |d| {
                d.child(
                    div()
                        .id("cancel-tip")
                        .test_support()
                        .tooltip(|window, cx| {
                            theme::tooltip_keys("Cancel", keys_for(&CancelQuery, cx), window, cx)
                        })
                        .child(
                            tool_button("cancel", ui::Icons::CircleStop, "Cancel", muted)
                                .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                        ),
                )
            })
            // Like Ctrl+S, open editors are staged first and staged edits saved; otherwise the
            // shell owns saving the query, and focusing the editor first routes the action there.
            .when(!key_mode, |d| {
                d.child(
                    div()
                        .id("query-save-tip")
                        .test_support()
                        .tooltip(move |window, cx| {
                            let keys = save_button_keys(cx);
                            theme::tooltip_keys(save_tooltip(staged, cx), keys, window, cx)
                        })
                        .child(
                            tool_button("query-save", ui::Icons::Save, "Save", muted).on_click(
                                cx.listener(|this, _, window, cx| {
                                    if !this.save_edits(window, cx) {
                                        this.editor.update(cx, |e, cx| e.focus(window, cx));
                                        window.dispatch_action(Box::new(SaveQuery), cx);
                                    }
                                }),
                            ),
                        ),
                )
            })
            .child(
                ui::accessibility_id(Button::new("row-limit"), "row-limit")
                    .ghost()
                    .small()
                    .label(row_limit_label(self.model.read(cx).max_rows()))
                    .child(ui::icon(ui::Icons::ChevronDown, px(12.), dim))
                    .font_family(mono)
                    .disabled(closed)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.model.update(cx, |m, _| {
                            m.set_max_rows(next_row_limit(m.max_rows()));
                        });
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .children(chip)
            .children(run_summary);
        let editor_max = (window.viewport_size().height - RESULTS_ROOM).max(EDITOR_MIN);
        let start_height = self.start_height.map(|h| h.min(editor_max));
        // The Editor fills its wrapper, so a click at the wrapper's centre lands in the text.
        // It draws no box of its own: the panel and the toolbar's hairline already frame it.
        let editor = div()
            .id("editor")
            .role(Role::Group)
            .accessibility_id("editor")
            .test_support()
            .size_full()
            // The kit's menu stays enabled so a right-click still moves the caret, but its builder
            // is empty: the wrapper's menu replaces the kit's LSP items.
            .child(
                Editor::new(&self.editor)
                    .bordered(false)
                    .h_full()
                    .context_menu(|menu, _, _| menu),
            )
            // The popup's keys are caught on the way down, before the editor moves its caret.
            .capture_action(cx.listener(|this, _: &MoveUp, window, cx| {
                this.popup_key(window, cx, |p, _, cx| p.select_by(-1, cx))
            }))
            .capture_action(cx.listener(|this, _: &MoveDown, window, cx| {
                this.popup_key(window, cx, |p, _, cx| p.select_by(1, cx))
            }))
            .capture_action(cx.listener(|this, a: &Enter, window, cx| {
                if !a.secondary {
                    this.popup_key(window, cx, |p, window, cx| p.accept(None, window, cx))
                }
            }))
            .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                this.popup_key(window, cx, |p, window, cx| p.accept(None, window, cx))
            }))
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                this.popup_key(window, cx, |p, _, cx| p.close(cx))
            }))
            .child(self.completion.clone())
            .context_menu({
                let weak = cx.entity().downgrade();
                move |menu, _, cx| editor_menu(menu, &weak, cx)
            });
        div()
            .key_context("QueryTab")
            .on_action(cx.listener(|this, _: &RunStatement, _, cx| this.run_statement(cx)))
            .on_action(cx.listener(|this, _: &RunAll, _, cx| this.run_all(cx)))
            .on_action(cx.listener(|this, _: &FormatSql, window, cx| this.format_sql(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleComment, window, cx| {
                this.toggle_comment(window, cx);
            }))
            .on_action(cx.listener(|this, _: &Explain, window, cx| {
                this.explain(ExplainMode::Plan, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ExplainAnalyze, window, cx| {
                this.explain(ExplainMode::Analyze, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CancelQuery, _, cx| this.cancel(cx)))
            // Ctrl+S from anywhere in the tab stages open editors, then saves staged edits;
            // with none it falls through to the SaveQuery binding of the same keys.
            .on_action(cx.listener(|this, _: &SaveEdits, window, cx| {
                if !this.save_edits(window, cx) {
                    cx.propagate();
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            // A production tab carries a 2 px danger rule along its top edge.
            .when(production, |d| d.border_t_2().border_color(danger))
            .child(toolbar)
            .when_some(key, |d, (header, error, more)| {
                d.child(key_pane(header, error, more, self.results.clone(), cx))
            })
            .when(self.key_title.is_none(), |d| {
                d.child(
                    div().flex_1().min_h_0().child(
                        v_resizable("query-split")
                            .with_state(&self.split)
                            .child(
                                resizable_panel()
                                    .when_some(start_height, |p, h| p.size(h))
                                    .child(editor),
                            )
                            .child(resizable_panel().child(self.results.clone())),
                    ),
                )
            })
    }
}

// A key tab's body: the header line, the last read's error, the value grid, and Load more
// while pages remain. Each line carries its text as its accessible name.
fn key_pane(
    header: Option<String>,
    error: Option<String>,
    more: bool,
    results: Entity<ResultsPane>,
    cx: &mut Context<QueryTab>,
) -> impl IntoElement {
    let dim = theme::dim(cx);
    let theme = cx.theme();
    let (danger, border) = (theme.danger, theme.border);
    let (mono, mono_size) = (theme.mono_font_family.clone(), theme.mono_font_size);
    let line = |id: &'static str, text: String| {
        div()
            .id(id)
            .aria_label(text.clone())
            .test_support()
            .flex_none()
            .px_2()
            .py_1()
            .child(text)
    };
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .children(header.map(|h| {
            line("key-header", h)
                .font_family(mono)
                .text_size(mono_size)
                .text_color(dim)
                .border_b_1()
                .border_color(border)
        }))
        .children(error.map(|e| line("key-error", e).text_color(danger)))
        .child(div().flex_1().min_h_0().child(results))
        .when(more, |d| {
            d.child(
                div()
                    .flex_none()
                    .p_1()
                    .border_t_1()
                    .border_color(border)
                    .child(
                        Button::new("key-load-more")
                            .outline()
                            .xsmall()
                            .label("Load more")
                            .accessibility_label("Load more")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.model.update(cx, |m, cx| {
                                    m.load_more_value(cx);
                                });
                            })),
                    ),
            )
        })
}

// Builds the editor menu; each item acts on the tab, the clipboard ones through the editor's
// own actions so the kit's selection rules apply.
fn editor_menu(
    menu: PopupMenu,
    weak: &WeakEntity<QueryTab>,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let mut menu = menu.min_w(px(180.));
    let tab = weak.upgrade();
    let selected = tab
        .as_ref()
        .is_some_and(|t| !t.read(cx).editor.read(cx).selected_range().is_empty());
    let explain = tab
        .as_ref()
        .is_some_and(|t| t.read(cx).session_conn.engine().capabilities().explain);
    let format = tab
        .as_ref()
        .is_some_and(|t| formats(t.read(cx).session_conn.engine()));
    for MenuEntry {
        id,
        label,
        keys,
        enabled,
    } in editor_menu_entries(selected, explain, format, cx)
    {
        if id == "editor-menu-cut" {
            menu = menu.separator();
        }
        let weak = weak.clone();
        let item = ui::text_menu_item(id, label, keys, cx)
            .disabled(!enabled)
            .on_click(move |_, window, cx| {
                let _ = weak.update(cx, |t, cx| t.editor_menu_action(id, window, cx));
            });
        menu = menu.item(item);
    }
    menu
}
