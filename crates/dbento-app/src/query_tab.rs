//! A query tab: the SQL editor over the results pane, Run and Cancel, the production chip,
//! the read-only badge and the confirmation for unguarded writes.

use std::borrow::Cow;
use std::cell::Cell;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use dbento_core::{
    ConnectionConfig, Engine, ExplainError, ExplainMode, ObjectKey, QueryLanguage,
    ReadOnlyEnforcement, RelationRef, Unguarded, Workspace, display_key, explain_request,
    read_only_residual,
};
use gpui_kit::base::ElementExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    Editor, EditorState, Enter, Escape, IndentInline, InputEvent, MoveDown, MoveUp,
};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, ResizableState, Sizable as _, WindowExt as _, h_flex,
    resizable_panel, v_resizable,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, EntityId, EventEmitter, Hsla,
    InteractiveElement, IntoElement, ParentElement, Pixels, Render, Role, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, TestSupportExt, WeakEntity, Window,
    div, px, uniform_list,
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
    ToggleComment, all_keys_for, chip_keys, chord_list, keys_for, plus_keys,
};
use crate::message_view::{Follow, GroupRow, MessageView, Panel, dropped_text};
use crate::nav::NavHistory;
use crate::object_view::{ObjectView, PreviewMode};
use crate::objects::s3_url;
use crate::pick_row::{ConnectionPick, pick_row, pickable_connections};
use crate::results_pane::{EditConnection, OpenTable, ResultsPane, RetryConnect, skip_notice};
use crate::run_target::run_range;
use crate::sidebar::{Sidebar, color_name, tag_color};
use crate::status_bar::run_elapsed;
use crate::tab_model::{Discard, Fence, ROW_LIMITS, TabModel, fence_text};
use crate::workspace_reload::DiskBanner;
use crate::{staged, theme, ui, update_banner};

// A restored editor height leaves this much of the window for the results and the chrome
// around them, so a height saved on a taller screen never squeezes the results shut.
const RESULTS_ROOM: Pixels = px(312.);
const EDITOR_MIN: Pixels = px(100.);
// The narrowest a toolbar chip truncates to, which still shows the start of its text.
const CHIP_MIN: Pixels = px(64.);
/// The height of a tab's toolbar above its query split.
pub(crate) const TOOLBAR_HEIGHT: Pixels = px(40.);

/// How far the toolbar has collapsed to fit: the run summary goes first, then the Run key
/// hint, then the Run, All and read-only labels; at the last level the chips truncate.
pub const TOOLBAR_LEVELS: usize = 4;

/// A staged collapse's level after a frame at `level` whose row measured `row` wide in a
/// container `width` wide, out of `N` levels. `needed[l]` is the width level `l` overflowed
/// at, so a level comes back only once the container is at least that wide.
pub fn toolbar_level<const N: usize>(
    level: usize,
    needed: &mut [Option<Pixels>; N],
    row: Pixels,
    width: Pixels,
) -> usize {
    if level < N && row > width + px(0.5) {
        needed[level] = Some(row);
        return level + 1;
    }
    let mut level = level;
    while let Some(Some(need)) = level.checked_sub(1).map(|l| needed[l])
        && need <= width
    {
        level -= 1;
    }
    level
}

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
pub const READ_ONLY_PG_TOOLTIP: &str = "Read-only: each statement runs in its own read-only transaction, so a SET lasts one statement. To keep a setting, add it to the connection's settings table in dbento.toml.";
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
    format!("Format SQL (also {})", chord_list(&aliases))
}

/// The read-only badge's tooltip for `engine`.
pub fn read_only_tooltip(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => READ_ONLY_PG_TOOLTIP,
        Engine::Sqlite => READ_ONLY_SQLITE_TOOLTIP,
        Engine::Mysql => READ_ONLY_MYSQL_TOOLTIP,
        Engine::Duckdb => READ_ONLY_DUCKDB_TOOLTIP,
        Engine::Dynamodb => READ_ONLY_DYNAMODB_TOOLTIP,
        Engine::Mssql | Engine::Mongodb | Engine::Surrealdb | Engine::Redis | Engine::S3 => {
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
        Engine::S3 => {
            return read_only_residual(Engine::S3)
                .unwrap_or_default()
                .to_owned();
        }
        Engine::Surrealdb | Engine::Redis => {}
        Engine::Postgres | Engine::Sqlite | Engine::Mysql | Engine::Mssql | Engine::Duckdb => {
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

/// The Redis flush `sql` runs, "FLUSHALL" or else "FLUSHDB", read from its first word.
fn flush_command(sql: &str) -> &'static str {
    let first = sql.split_whitespace().next().unwrap_or_default();
    match first
        .trim_matches(['"', '\''])
        .eq_ignore_ascii_case("FLUSHALL")
    {
        true => "FLUSHALL",
        false => "FLUSHDB",
    }
}

/// What the explain shortcut says when `err` refuses the statement on `engine`, which prose
/// calls `title`: a sentence listing only the statements that engine has.
pub fn explain_warning(err: &ExplainError, engine: Engine, title: &str) -> String {
    match err {
        ExplainError::UnsupportedEngine => format!("EXPLAIN is not available for {title} yet."),
        ExplainError::NotOneStatement => "Select one statement to explain.".to_owned(),
        ExplainError::AlreadyExplain => {
            "This is already an EXPLAIN. Run it to see its plan.".to_owned()
        }
        ExplainError::NotExplainable => {
            let statements = match engine {
                Engine::Sqlite => "SELECT, WITH, VALUES, INSERT, UPDATE, DELETE or REPLACE",
                Engine::Mysql => "SELECT, WITH, VALUES, TABLE, INSERT, UPDATE, DELETE or REPLACE",
                Engine::Postgres
                | Engine::Mssql
                | Engine::Duckdb
                | Engine::Mongodb
                | Engine::Surrealdb
                | Engine::Redis
                | Engine::Dynamodb
                | Engine::S3 => "SELECT, WITH, VALUES, TABLE, INSERT, UPDATE, DELETE or MERGE",
            };
            format!("Only {statements} can be explained on {title}, and not after a block comment.")
        }
        other => format!("{other}."),
    }
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
/// in `engine`'s prompt language. A flush names the one command it runs, and SurrealDB's
/// explanations speak of records.
pub fn unguarded_ask(kind: Option<Unguarded>, engine: Engine, connection: &str, sql: &str) -> Ask {
    let flush = (kind == Some(Unguarded::FlushKeys)).then(|| flush_command(sql));
    let title = match (kind, flush) {
        (_, Some(command)) => format!("{command} on '{connection}'"),
        (Some(kind), None) => confirm_title(kind, connection),
        (None, None) => format!("Run this statement on '{connection}'?"),
    };
    let detail = match (kind, flush, engine) {
        (_, Some("FLUSHALL"), _) => "It deletes every key in every database on the server.",
        (_, Some(_), _) => "It deletes every key in this database.",
        // SurrealDB stores records, not rows.
        (Some(Unguarded::UpdateWithoutWhere), _, Engine::Surrealdb) => {
            "It has no WHERE clause, so it changes every record."
        }
        (Some(Unguarded::DeleteWithoutWhere), _, Engine::Surrealdb) => {
            "It has no WHERE clause, so it deletes every record."
        }
        _ => confirm_explanation(kind),
    };
    Ask {
        title: title.into(),
        detail: Some(detail.into()),
        sql: Some(sql.to_owned().into()),
        language: prompt_language(engine),
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
    // Each unguarded kind adds the sentence its own confirmation would show, once.
    let mut said = Vec::new();
    for &kind in kinds {
        let sentence = confirm_explanation(Some(kind));
        if !said.contains(&sentence) {
            said.push(sentence);
            detail.push(' ');
            detail.push_str(sentence);
        }
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
        | Engine::S3 => c.display_target(),
    };
    format!("{} \u{b7} {} \u{b7} {target}", c.name(), c.engine_label())
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
/// "javascript" for the MongoDB shell, "bash" for Redis and S3 command lines (close enough to colour
/// words and quotes), "text" (plain, no highlighting) for SurrealQL.
pub fn editor_language(engine: Engine) -> &'static str {
    match engine.capabilities().language {
        // PartiQL's `<<`, `>>` and `{}` fall outside the SQL grammar; its keywords still colour.
        QueryLanguage::Sql | QueryLanguage::PartiQl => "sql",
        QueryLanguage::MongoShell => "javascript",
        QueryLanguage::RedisCommands | QueryLanguage::S3Cmd => "bash",
        QueryLanguage::SurrealQl => "text",
    }
}

/// The editor's right-click menu in order: Run, Format, Explain, Toggle comment and Save with
/// their effective key chips, then the clipboard items, whose keys the kit binds and the menu
/// does not repeat.
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
    // Save shows the key the keymap binds first, the one the toolbar's Save tooltip names.
    let save = all_keys_for(&SaveQuery, cx).into_iter().next();
    entries.extend([
        entry(
            "editor-menu-toggle-comment",
            "Toggle comment",
            keys_for(&ToggleComment, cx),
        ),
        entry("editor-menu-save", "Save query", save),
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

impl ConnState {
    // The fence the tab model holds for this state; a current session runs unfenced.
    fn fence(self) -> Option<Fence> {
        match self {
            ConnState::Current => None,
            ConnState::Changed { still_read_write } => Some(Fence::Changed {
                now_read_only: still_read_write,
            }),
            ConnState::Gone => Some(Fence::Gone),
        }
    }
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
    /// The disk banner asked to take the file's text.
    DiskReload,
    /// The disk banner asked to keep the tab's text.
    DiskKeep,
    /// The disk banner asked to compare the tab's text with the file's.
    DiskCompare,
    /// The disk banner asked to close the tab whose file is gone.
    DiskClose,
    /// The object tab asked to download its object to a file the user picks.
    DownloadObject(ObjectKey),
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
    // What an outside change to the file asks of the user, and the banner they last kept, which
    // stays hidden while reads keep finding it.
    disk_banner: Option<DiskBanner>,
    dismissed: Option<DiskBanner>,
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
    // The object preview's read-only editor and the mode whose text it holds.
    object_editor: Option<(PreviewMode, Entity<EditorState>)>,
    // The toolbar's collapse level, the widths each lower level needed, and the contents
    // they were measured with: new contents measure again from the top.
    toolbar_level: usize,
    toolbar_needed: [Option<Pixels>; TOOLBAR_LEVELS],
    toolbar_contents: u64,
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
        // says why on a toast, unless its engine's lasting chip already does.
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
                    if this.model.read(cx).not_editable().is_none() {
                        let text = unavailable_text(reason);
                        let kind = ui::NoticeKind::Warning;
                        ui::notice(window, cx, ui::Notice { text, kind });
                    }
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
            disk_banner: None,
            dismissed: None,
            draft,
            draft_task: None,
            split: cx.new(|_| ResizableState::default()),
            start_height: None,
            number: None,
            origin: None,
            label: None,
            key_title: None,
            object_editor: None,
            toolbar_level: 0,
            toolbar_needed: [None; TOOLBAR_LEVELS],
            toolbar_contents: 0,
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

    /// Whether Run and Run all are live: the tab is open and its session is not fenced.
    pub fn runnable(&self, cx: &gpui_kit::App) -> bool {
        let m = self.model.read(cx);
        !m.closed() && m.fence().is_none()
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
    /// current text as saved; any disk banner and its dismissal are cleared.
    pub fn set_file(&mut self, rel: PathBuf, disk: String, cx: &mut Context<Self>) {
        self.file = Some(rel);
        self.disk = Some(disk);
        self.clear_disk_banner();
        self.mark_saved(cx);
        cx.notify();
    }

    // A fresh file binding leaves no outside change to report and no kept one to hide.
    fn clear_disk_banner(&mut self) {
        self.disk_banner = None;
        self.dismissed = None;
    }

    /// The file's text when this tab last read or wrote it.
    pub fn disk(&self) -> Option<&str> {
        self.disk.as_deref()
    }

    /// Whether the editor's text differs from the baseline at all, whitespace included.
    pub fn has_local_edits(&self, cx: &App) -> bool {
        self.sql(cx) != self.baseline
    }

    /// The banner an outside change to the file shows, if any.
    pub fn disk_banner(&self) -> Option<&DiskBanner> {
        self.disk_banner.as_ref()
    }

    /// Shows `banner`, unless it is the one the user last kept; None hides the banner and
    /// forgets that dismissal, as does any other banner.
    pub fn set_disk_banner(&mut self, banner: Option<DiskBanner>, cx: &mut Context<Self>) {
        if banner.is_some() && banner == self.dismissed {
            self.disk_banner = None;
        } else {
            self.dismissed = None;
            self.disk_banner = banner;
        }
        cx.notify();
    }

    /// Keeps the tab's text over the banner's news: a changed file's text becomes the known
    /// one, a deleted file is forgotten, and a read failure is hidden until another read.
    pub fn keep_mine(&mut self, cx: &mut Context<Self>) {
        match self.disk_banner.take() {
            Some(DiskBanner::Changed { disk }) => self.disk = Some(disk),
            Some(DiskBanner::Deleted) => self.disk = None,
            failed @ Some(DiskBanner::Unreadable { .. }) => self.dismissed = failed,
            None => {}
        }
        cx.notify();
    }

    /// Moves the tab's file from under folder `from` to under `to`; false, and nothing
    /// changes, when the file is not under `from`.
    pub fn rebase_file(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) -> bool {
        let Some(rest) = self.file.as_deref().and_then(|f| f.strip_prefix(from).ok()) else {
            return false;
        };
        self.file = Some(to.join(rest));
        self.schedule_draft(cx);
        cx.notify();
        true
    }

    /// Unties the tab from its saved query, which another tab now owns: it keeps its text as
    /// an edited tab with an empty baseline, and its draft follows.
    pub fn unbind_file(&mut self, cx: &mut Context<Self>) {
        self.file = None;
        self.disk = None;
        self.clear_disk_banner();
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

    /// Turns this tab into S3 object `key`'s view in `bucket`: the editor gives way to the
    /// object's header and preview, and the tab keeps no draft.
    pub fn open_object(&mut self, bucket: String, key: String, cx: &mut Context<Self>) {
        self.key_title = Some(ObjectView::new(bucket.clone(), key.clone()).title());
        self.object_editor = None;
        self.draft = None;
        self.draft_task = None;
        self.model
            .update(cx, |m, cx| m.open_object(bucket, key, cx));
        cx.notify();
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
        let fence = self.state.fence();
        self.model.update(cx, |m, cx| m.set_fence(fence, cx));
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
                let title = crate::sidebar::engine_title(&self.session_conn);
                let text = explain_warning(&err, engine, title);
                let kind = ui::NoticeKind::Warning;
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
            "editor-menu-toggle-comment" => return self.toggle_comment(window, cx),
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
            let engine = self.session_conn.engine();
            unguarded_ask(kinds.first().copied(), engine, connection, &sql)
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

    /// Asks Commit or Roll back once per new apply prompt. Focus opens on Roll back and Enter
    /// answers the focused button; Escape and a dialog closed unanswered also roll back, and
    /// only while that prompt still stands.
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
            default: 0,
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
        let text = fence_text(self.state.fence()?);
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
                .flex_shrink(1.)
                .min_w_0()
                .items_center()
                .gap_2()
                .child(
                    ui::chip_base(cx)
                        .id("conn-chip")
                        .role(Role::Status)
                        .aria_label(text)
                        .test_support()
                        .flex_shrink(1.)
                        .min_w(CHIP_MIN)
                        .overflow_hidden()
                        .border_color(warning)
                        .text_color(warning)
                        .child(div().min_w_0().truncate().child(text))
                        .tooltip(move |window, cx| theme::tooltip(text, window, cx)),
                )
                .children(reconnect),
        )
    }
}

impl QueryTab {
    // The row under the toolbar saying the file changed, went or cannot be read, with the
    // choices each case offers; the shell acts on them.
    fn disk_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let banner = self.disk_banner.as_ref()?;
        let theme = cx.theme();
        let (says, color) = match banner {
            DiskBanner::Changed { .. } => ("Changed on disk".to_owned(), theme.foreground),
            DiskBanner::Deleted => ("Deleted on disk".to_owned(), theme.foreground),
            DiskBanner::Unreadable { reason } => (
                format!("Cannot read the file on disk: {reason}"),
                theme.danger,
            ),
        };
        let changed = matches!(banner, DiskBanner::Changed { .. });
        let deleted = matches!(banner, DiskBanner::Deleted);
        let button = |id: &'static str, text: &'static str, event: fn() -> QueryTabEvent| {
            ui::secondary(Button::new(id))
                .small()
                .label(text)
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(event())))
        };
        let row = h_flex()
            .id("disk-banner")
            .role(Role::Status)
            .test_support()
            // A narrow window wraps the row onto more lines instead of clipping its actions.
            .min_h(px(32.))
            .py_1()
            .flex_wrap()
            .flex_none()
            .gap_2()
            .px_3()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .text_sm()
            .child(update_banner::label("disk-banner-text", says, color))
            .when(changed, |d| {
                d.child(button("disk-reload", "Reload", || {
                    QueryTabEvent::DiskReload
                }))
                .child(button("disk-compare", "Compare", || {
                    QueryTabEvent::DiskCompare
                }))
            })
            .child(button(
                "disk-keep",
                if changed { "Keep mine" } else { "Keep" },
                || QueryTabEvent::DiskKeep,
            ))
            .when(deleted, |d| {
                d.child(button("disk-close", "Close", || QueryTabEvent::DiskClose))
            });
        Some(row.into_any_element())
    }

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
        // Only a toolbar collapsed to its last level squeezes the chip; then its name truncates.
        let label = div().min_w_0().truncate().child(text.clone());
        let m = self.model.read(cx);
        if m.closed() || m.browses() {
            return ui::chip_base(cx)
                .id("query-connection")
                .aria_label(text.clone())
                .test_support()
                .flex_shrink(1.)
                .min_w(CHIP_MIN)
                .overflow_hidden()
                .children(dot)
                .child(label)
                .into_any_element();
        }
        let (workspace, sidebar) = (Arc::clone(&self.workspace), self.sidebar.clone());
        let current = m.spec().connection.clone();
        let tab = cx.entity().downgrade();
        let theme = cx.theme();
        let (radius, border, popover) = (theme.radius, theme.border, theme.popover);
        // The menu wraps the button in a box of its own, so the shrinking box is this one.
        let button = Button::new("query-connection")
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
            .child(label)
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, cx| {
                let rows = pickable_connections(&workspace, sidebar.as_ref(), cx);
                connection_menu(menu, &tab, &rows, &current)
            });
        div()
            .flex_shrink(1.)
            .min_w(CHIP_MIN)
            .overflow_hidden()
            .child(button)
            .into_any_element()
    }
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
    rows: &[ConnectionPick],
    current: &str,
) -> PopupMenu {
    rows.iter().enumerate().fold(menu, |menu, (n, row)| {
        let label: SharedString = row.name.clone().into();
        let (tab, pick, shown) = (tab.clone(), row.name.clone(), row.clone());
        let item = PopupMenuItem::element(move |_, cx| {
            pick_row("query-connection-pick", n, &shown, true, cx)
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
        self.sync_object_editor(window, cx);
        // Cancel also stops a parked result, interrupts a drain and stops a table's load.
        let m = self.model.read(cx);
        let cancellable = m.running()
            || m.paused().is_some()
            || m.parked_at_cap().is_some()
            || m.settling()
            || m.target_pending().is_some();
        // A closed tab only waits for its session to go, so nothing here may act.
        let closed = m.closed();
        // A fenced session refuses runs too, so Run's halves read as unavailable then.
        let no_run = !self.runnable(cx);
        // Explain shows only where the session's engine has it; its keys still explain why not.
        let explain = self.session_conn.engine().capabilities().explain;
        let format = formats(self.session_conn.engine());
        let staged = m.edits().len();
        let key_mode = self.key_title.is_some();
        // A key tab shows its value's header, last error and whether pages remain; a stream's
        // tab pages through its message bar instead of Load more.
        let messages = m.message_view().map(MessageBar::of);
        let object = m.object_view().map(ObjectPane::of);
        let key = (self.key_title.is_some() && object.is_none()).then(|| {
            let view = m.key_view();
            (
                view.and_then(KeyView::header).map(str::to_owned),
                view.and_then(KeyView::error).map(str::to_owned),
                view.is_some_and(KeyView::has_more) && messages.is_none(),
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
        let limit = row_limit_label(m.max_rows());
        let contents = {
            let mut h = DefaultHasher::new();
            let changed = match self.state {
                ConnState::Current => 0u8,
                ConnState::Changed { still_read_write } => 1 + u8::from(still_read_write),
                ConnState::Gone => 3,
            };
            let conn = connection_chip_text(&self.conn);
            (
                conn, &summary, changed, read_only, production, explain, format, key_mode,
            )
                .hash(&mut h);
            (&badge_text, &limit).hash(&mut h);
            h.finish()
        };
        if contents != self.toolbar_contents {
            self.toolbar_contents = contents;
            self.toolbar_level = 0;
            self.toolbar_needed = [None; TOOLBAR_LEVELS];
        }
        let level = self.toolbar_level;
        let summary = summary.filter(|_| level < 1);
        let labelled = level < 3;
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
                .when(labelled, |d| d.child("read-only"))
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
        let run_keys = keys_for(&RunStatement, cx).filter(|_| level < 2).map(|k| {
            let keys = chip_keys(&k);
            ui::key_chip_colored(keys.clone(), primary_fg, primary_fg.opacity(0.4), cx)
                .id("run-keys")
                .aria_label(keys)
                .test_support()
        });
        // The row keeps its natural width until the last level, so an overflow shows how much
        // room this level needs; at the last level it fits the toolbar and the chips truncate.
        let fits = Rc::new(Cell::new(None));
        let measured = Rc::clone(&fits);
        let row = div()
            .flex()
            .items_center()
            .gap_1()
            .h_full()
            .px_2()
            .map(|d| {
                if level < TOOLBAR_LEVELS {
                    d.flex_grow(1.).flex_shrink_0()
                } else {
                    d.flex_1().min_w_0()
                }
            })
            .on_prepaint(move |bounds, _, _| measured.set(Some(bounds.size.width)));
        let tab = cx.entity().downgrade();
        // The toolbar sits flat on the panel; its label still names the connection's colour.
        let mut toolbar = div().id("query-toolbar").test_support();
        if let Some(c) = self.conn.effective_color() {
            toolbar = toolbar.aria_label(color_name(c));
        }
        let row = row
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
                                .when(labelled, |b| b.label("Run"))
                                .accessibility_label("Run")
                                .children(run_keys)
                                .disabled(no_run)
                                .on_click(cx.listener(|this, _, _, cx| this.run_statement(cx))),
                        )
                        .child(
                            div()
                                .w(px(1.))
                                .h(px(28.))
                                .bg(primary)
                                // Disabled halves draw primary at 15 %, so the divider fades
                                // with them.
                                .when(no_run, |d| d.opacity(0.15))
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
                                .when(labelled, |b| b.label("All"))
                                .accessibility_label("Run all")
                                .tooltip_with_action("Run all", &RunAll, Some("QueryTab"))
                                .disabled(no_run)
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
                    .label(limit)
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
        let toolbar = toolbar
            .flex()
            .flex_none()
            .h(TOOLBAR_HEIGHT)
            .overflow_hidden()
            .border_b_1()
            .border_color(border)
            .child(row)
            // The row prepaints first, so its width is known when the toolbar's is.
            .on_prepaint(move |bounds, _, cx| {
                let Some(row) = fits.get() else { return };
                let _ = tab.update(cx, |t, cx| {
                    let next = toolbar_level(level, &mut t.toolbar_needed, row, bounds.size.width);
                    if next != t.toolbar_level {
                        t.toolbar_level = next;
                        cx.notify();
                    }
                });
            });
        let editor_max = (crate::shell::content_size(window).height - RESULTS_ROOM).max(EDITOR_MIN);
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
            .relative()
            .child(toolbar)
            .children(self.disk_row(cx))
            .when_some(object, |d, pane| {
                let editor = self.object_editor.as_ref().map(|(_, e)| e.clone());
                d.child(object_pane(pane, self.model.clone(), editor, window, cx))
            })
            .when_some(key, |d, (header, error, more)| {
                d.child(key_pane(
                    KeyPane {
                        header,
                        error,
                        more,
                        messages,
                    },
                    self.results.clone(),
                    cx,
                ))
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
            // A production tab's 2 px danger rule lies over the toolbar's top edge, drawn last
            // so it covers it, rather than pushing the toolbar down.
            .when(production, |d| {
                d.child(
                    div()
                        .id("production-rule")
                        .test_support()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .h(px(2.))
                        .bg(danger),
                )
            })
    }
}

impl QueryTab {
    // Keeps the preview editor holding the text of the mode shown: built when the mode
    // changes, dropped in Hex, so the text reaches the editor once per switch, not per frame.
    fn sync_object_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let m = self.model.read(cx);
        let Some(view) = m.object_view() else {
            return;
        };
        let (mode, text) = match (view.mode(), view.text()) {
            (Some(mode), Some(text)) => (mode, text),
            _ => {
                self.object_editor = None;
                return;
            }
        };
        if self.object_editor.as_ref().is_some_and(|(m, _)| *m == mode) {
            return;
        }
        let text = text.to_owned();
        let editor = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .line_number(false)
                .folding(false)
                .soft_wrap(false)
                .default_value(text);
            state.set_readonly(true, cx);
            state
        });
        self.object_editor = Some((mode, editor));
    }

    // The hex rows `range` of the preview, each a mono line with its own id.
    fn hex_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let (mono, mono_size) = (
            cx.theme().mono_font_family.clone(),
            cx.theme().mono_font_size,
        );
        let Some(view) = self.model.read(cx).object_view() else {
            return Vec::new();
        };
        range
            .filter_map(|i| {
                let line = view.hex_row(i)?;
                Some(
                    div()
                        .id(("object-hex-row", i))
                        .test_support()
                        .px_2()
                        .h(px(20.))
                        .font_family(mono.clone())
                        .text_size(mono_size)
                        .whitespace_nowrap()
                        .child(line)
                        .into_any_element(),
                )
            })
            .collect()
    }
}

// What an object tab shows, copied out of the model in render.
struct ObjectPane {
    key: ObjectKey,
    header: Option<String>,
    note: Option<String>,
    error: Option<String>,
    modes: Vec<PreviewMode>,
    mode: Option<PreviewMode>,
    hex_rows: usize,
}

impl ObjectPane {
    fn of(view: &ObjectView) -> Self {
        Self {
            key: view.object_key(),
            header: view.header(),
            note: view.note(),
            error: view.error().map(str::to_owned),
            modes: view.modes(),
            mode: view.mode(),
            hex_rows: view.hex_rows(),
        }
    }
}

// An object tab's body: the metadata line with its actions, the mode switch and note, then
// the text or JSON in a read-only editor or the hex rows; a failed read shows only why.
fn object_pane(
    pane: ObjectPane,
    model: Entity<TabModel>,
    editor: Option<Entity<EditorState>>,
    window: &mut Window,
    cx: &mut Context<QueryTab>,
) -> impl IntoElement + use<> {
    let dim = theme::dim(cx);
    let theme = cx.theme();
    let (danger, border) = (theme.danger, theme.border);
    let (mono, mono_size) = (theme.mono_font_family.clone(), theme.mono_font_size);
    let line = |id: &'static str, text: String| {
        div()
            .id(id)
            .aria_label(text.clone())
            .test_support()
            .px_2()
            .py_1()
            .child(text)
    };
    let ObjectPane {
        key,
        header,
        note,
        error,
        modes,
        mode,
        hex_rows,
    } = pane;
    let action = |id: &'static str, label: &'static str| {
        Button::new(id)
            .ghost()
            .xsmall()
            .label(label)
            .accessibility_label(label)
    };
    let (shown_key, url) = (display_key(&key.key), s3_url(&key.bucket, &key.key));
    let download = key.clone();
    let actions = h_flex()
        .flex_none()
        .gap_1()
        .child(
            action("object-download", "Download").on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(QueryTabEvent::DownloadObject(download.clone()));
            })),
        )
        .child(
            action("object-copy-key", "Copy key").on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(shown_key.clone()));
            }),
        )
        .child(
            action("object-copy-url", "Copy URL").on_click(move |_, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
            }),
        );
    let head = h_flex()
        .flex_none()
        .justify_between()
        .gap_2()
        .pr_2()
        .border_b_1()
        .border_color(border)
        .children(header.map(|h| {
            line("object-header", h)
                .min_w_0()
                .font_family(mono.clone())
                .text_size(mono_size)
                .text_color(dim)
        }))
        .when(error.is_none(), |d| d.child(actions));
    // Only the modes the bytes allow are offered, each under its own id.
    let options: Vec<(&'static str, &'static str)> = modes
        .iter()
        .map(|m| match m {
            PreviewMode::Text => ("object-mode-text", "Text"),
            PreviewMode::Json => ("object-mode-json", "JSON"),
            PreviewMode::Hex => ("object-mode-hex", "Hex"),
        })
        .collect();
    let switch = (!options.is_empty()).then(|| {
        let selected = mode
            .and_then(|m| modes.iter().position(|&o| o == m))
            .unwrap_or_default();
        let modes = modes.clone();
        ui::segmented_ids(
            "object-mode",
            &options,
            selected,
            move |ix, _, cx| {
                if let Some(&to) = modes.get(*ix) {
                    model.update(cx, |m, cx| m.set_preview_mode(to, cx));
                }
            },
            window,
            cx,
        )
    });
    let bar = h_flex()
        .flex_none()
        .gap_3()
        .px_2()
        .py_1()
        .children(switch)
        .children(note.map(|n| line("object-note", n).text_color(dim)));
    let body = match mode {
        Some(PreviewMode::Hex) => uniform_list(
            "object-hex",
            hex_rows,
            cx.processor(|this, range: Range<usize>, _, cx| this.hex_rows(range, cx)),
        )
        .test_support()
        .flex_1()
        .min_h_0()
        .into_any_element(),
        Some(PreviewMode::Text | PreviewMode::Json) => match editor {
            Some(editor) => div()
                .id("object-text")
                .role(Role::Group)
                .test_support()
                .flex_1()
                .min_h_0()
                .child(Editor::new(&editor).readonly(true).bordered(false).h_full())
                .into_any_element(),
            None => div().flex_1().into_any_element(),
        },
        None => div().flex_1().into_any_element(),
    };
    div()
        .id("object-view")
        .test_support()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .child(head)
        .children(error.map(|e| line("object-error", e).text_color(danger)))
        .child(bar)
        .child(body)
}

// What a stream tab's message bar and groups panel show, copied out of the model in render so
// the pane never reads it.
struct MessageBar {
    follow: Follow,
    polling: bool,
    older: bool,
    dropped: u64,
    error: Option<String>,
    groups: Option<Panel<Vec<GroupRow>>>,
    pending: Option<(String, Panel<Vec<String>>)>,
}

impl MessageBar {
    fn of(view: &MessageView) -> Self {
        Self {
            follow: view.follow(),
            polling: view.polling(),
            older: view.has_older(),
            dropped: view.dropped(),
            error: view.error().map(str::to_owned),
            groups: view.groups().cloned(),
            pending: view.pending().map(|(g, p)| (g.to_owned(), p.clone())),
        }
    }
}

// A key tab's body as render collects it: the value's header and error, whether Load more
// shows, and a stream's message bar.
struct KeyPane {
    header: Option<String>,
    error: Option<String>,
    more: bool,
    messages: Option<MessageBar>,
}

// A key tab's body: the header line, the last read's error, a stream's message bar and groups
// panel, the value grid, and Load more while pages remain. Each line carries its text as its
// accessible name.
fn key_pane(
    pane: KeyPane,
    results: Entity<ResultsPane>,
    cx: &mut Context<QueryTab>,
) -> impl IntoElement {
    let KeyPane {
        header,
        error,
        more,
        messages,
    } = pane;
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
    let (bar, groups) = match messages {
        Some(mut m) => {
            let panel = m
                .groups
                .take()
                .map(|g| groups_panel(g, m.pending.take(), cx));
            (Some(message_bar(m, panel.is_some(), cx)), panel)
        }
        None => (None, None),
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
        .children(bar)
        .children(groups)
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

// A stream's bar: Follow toggles polling, Pause holds it, Older pages back, Groups opens the
// panel; the dropped count and the last read's error follow on the right.
fn message_bar(
    bar: MessageBar,
    groups_open: bool,
    cx: &mut Context<QueryTab>,
) -> impl IntoElement + use<> {
    let dim = theme::dim(cx);
    let theme = cx.theme();
    let (danger, border) = (theme.danger, theme.border);
    let following = bar.follow != Follow::Off;
    let follow = if following { "Following" } else { "Follow" };
    let pause = if bar.follow == Follow::Paused {
        "Resume"
    } else {
        "Pause"
    };
    let follow_next = if following { Follow::Off } else { Follow::On };
    let pause_next = if bar.follow == Follow::Paused {
        Follow::On
    } else {
        Follow::Paused
    };
    // The bar's name says whether a poll is in flight, so a scenario can wait for it to land.
    let name = if bar.polling {
        "messages, polling"
    } else {
        "messages"
    };
    h_flex()
        .id("message-bar")
        .aria_label(name)
        .test_support()
        .flex_none()
        .gap_1()
        .p_1()
        .border_b_1()
        .border_color(border)
        .child(
            Button::new("message-follow")
                .when(following, |b| b.primary())
                .when(!following, |b| b.outline())
                .xsmall()
                .label(follow)
                .accessibility_label(follow)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.model.update(cx, |m, cx| m.set_follow(follow_next, cx));
                })),
        )
        .when(following, |d| {
            d.child(
                Button::new("message-pause")
                    .outline()
                    .xsmall()
                    .label(pause)
                    .accessibility_label(pause)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.update(cx, |m, cx| m.set_follow(pause_next, cx));
                    })),
            )
        })
        .when(bar.older, |d| {
            d.child(
                Button::new("message-older")
                    .outline()
                    .xsmall()
                    .label("Older")
                    .accessibility_label("Older")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.model.update(cx, |m, cx| {
                            m.older_messages(cx);
                        });
                    })),
            )
        })
        .child(
            Button::new("message-groups")
                .outline()
                .xsmall()
                .label("Groups")
                .accessibility_label("Groups")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.model.update(cx, |m, cx| {
                        if groups_open {
                            m.close_groups(cx);
                        } else {
                            m.load_groups(cx);
                        }
                    });
                })),
        )
        .when(bar.dropped > 0, |d| {
            let text = dropped_text(bar.dropped);
            d.child(
                div()
                    .id("message-dropped")
                    .aria_label(text.clone())
                    .test_support()
                    .px_1()
                    .text_xs()
                    .text_color(dim)
                    .child(text),
            )
        })
        .children(bar.error.map(|e| {
            div()
                .id("message-error")
                .aria_label(e.clone())
                .test_support()
                .px_1()
                .text_xs()
                .text_color(danger)
                .child(e)
        }))
}

// The groups panel under the bar, capped in height: one row per group, a click lists that
// group's pending entries under its row.
fn groups_panel(
    groups: Panel<Vec<GroupRow>>,
    pending: Option<(String, Panel<Vec<String>>)>,
    cx: &mut Context<QueryTab>,
) -> impl IntoElement + use<> {
    let dim = theme::dim(cx);
    let theme = cx.theme();
    let (danger, border) = (theme.danger, theme.border);
    let (mono, mono_size) = (theme.mono_font_family.clone(), theme.mono_font_size);
    let text_row = |id: SharedString, text: String| {
        div()
            .id(id)
            .aria_label(text.clone())
            .test_support()
            .px_2()
            .py_0p5()
            .child(text)
    };
    let mut rows: Vec<AnyElement> = Vec::new();
    match groups {
        Panel::Loading => rows.push(
            text_row(
                "message-groups-loading".into(),
                "Loading groups\u{2026}".into(),
            )
            .text_color(dim)
            .into_any_element(),
        ),
        Panel::Failed(e) => rows.push(
            text_row("message-groups-error".into(), e)
                .text_color(danger)
                .into_any_element(),
        ),
        Panel::Ready(list) if list.is_empty() => rows.push(
            text_row("message-groups-empty".into(), "No consumer groups".into())
                .text_color(dim)
                .into_any_element(),
        ),
        Panel::Ready(list) => {
            for (i, group) in list.into_iter().enumerate() {
                let GroupRow { name, text } = group;
                let open = pending.as_ref().is_some_and(|(g, _)| *g == name);
                rows.push(
                    text_row(format!("message-group-{i}").into(), text)
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let name = name.clone();
                            this.model.update(cx, |m, cx| {
                                m.load_pending(name, cx);
                            });
                        }))
                        .into_any_element(),
                );
                if let Some((_, list)) = pending.as_ref().filter(|_| open) {
                    rows.extend(pending_rows(list, &text_row, dim, danger));
                }
            }
        }
    }
    div()
        .id("message-groups-panel")
        .flex_none()
        .max_h(px(160.))
        .overflow_y_scroll()
        .py_0p5()
        .border_b_1()
        .border_color(border)
        .font_family(mono)
        .text_size(mono_size)
        .children(rows)
}

// A group's pending entries, indented under its row; a failed read shows its error instead.
fn pending_rows<E: Styled + IntoElement>(
    pending: &Panel<Vec<String>>,
    text_row: &impl Fn(SharedString, String) -> E,
    dim: Hsla,
    danger: Hsla,
) -> Vec<AnyElement> {
    match pending {
        Panel::Loading => Vec::new(),
        Panel::Failed(e) => vec![
            text_row("message-pending-error".into(), e.clone())
                .pl_6()
                .text_color(danger)
                .into_any_element(),
        ],
        Panel::Ready(list) => list
            .iter()
            .enumerate()
            .map(|(i, p)| {
                text_row(format!("message-pending-{i}").into(), p.clone())
                    .pl_6()
                    .text_color(dim)
                    .into_any_element()
            })
            .collect(),
    }
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
        let item =
            ui::text_menu_item_if(id, label, keys, enabled, cx).on_click(move |_, window, cx| {
                let _ = weak.update(cx, |t, cx| t.editor_menu_action(id, window, cx));
            });
        menu = menu.item(item);
    }
    menu
}
