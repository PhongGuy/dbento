//! The command palette: query prefixes, the subsequence ranker, the groups of rows it shows,
//! and the dialog view that hosts them.

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use dbento_core::{RelationKind, RelationRef, SavedQuery, Workspace};
use gpui_kit::assets::IconName;
use gpui_kit::base::actions::Cancel;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::{ActiveTheme as _, IndexPath, WindowExt as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Action, App, AppContext as _, Context, Entity, EventEmitter, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Subscription,
    TestSupportExt, Window, div, px,
};

use crate::catalog::CatalogQuery;
use crate::confirm::frame;
use crate::keymap::{self, chip_keys};
use crate::link::LinkState;
use crate::schema_tree::NodeKey;
use crate::sidebar::Sidebar;
use crate::theme::{self, CycleAppearance};
use crate::ui::{self, Icons};

/// Which sources a palette query searches, picked by its leading prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// No prefix: tables, saved queries, recent tables, connections and commands.
    All,
    /// `>`: commands only.
    Commands,
    /// `#`: saved queries only.
    Queries,
    /// `?`: keyboard shortcuts only.
    Shortcuts,
}

/// One leading `>`, `#` or `?` picks Commands, Queries or Shortcuts; the rest, trimmed, is the search.
pub fn parse_query(raw: &str) -> (Mode, &str) {
    let raw = raw.trim();
    let mode = match raw.chars().next() {
        Some('>') => Mode::Commands,
        Some('#') => Mode::Queries,
        Some('?') => Mode::Shortcuts,
        _ => return (Mode::All, raw),
    };
    (mode, raw[1..].trim())
}

/// A row the ranker scores, by its searchable text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The text the query is matched against.
    pub text: String,
}

const MATCH: i64 = 1;
const WORD_START: i64 = 2;
// Larger than WORD_START, so a contiguous run beats the same letters at scattered word starts.
const ADJACENT: i64 = 3;

/// Indices of the candidates whose text holds `query` as a case-insensitive subsequence,
/// best first; an empty query keeps every index in input order.
pub fn rank(query: &str, candidates: &[Candidate]) -> Vec<usize> {
    if query.is_empty() {
        return (0..candidates.len()).collect();
    }
    let query = lower(query);
    let mut scored: Vec<(i64, usize, usize)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let text = lower(&c.text);
            score(&query, &text).map(|s| (s, text.len(), i))
        })
        .collect();
    // Higher score first, then shorter text, then input order.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    scored.into_iter().map(|(_, _, i)| i).collect()
}

fn lower(s: &str) -> Vec<char> {
    s.chars().flat_map(char::to_lowercase).collect()
}

/// The best alignment's score, or `None` when `query` is not a subsequence of `text`.
/// Row `i` of the DP holds, per text position `j`, the best score with query char `i`
/// matched at `j`; each row reads the previous one's diagonal (adjacent) and prefix maximum.
fn score(query: &[char], text: &[char]) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let gain = |j: usize| {
        let start = j == 0 || matches!(text[j - 1], '.' | '_' | '-' | '/' | ' ');
        MATCH + if start { WORD_START } else { 0 }
    };
    let mut prev: Vec<Option<i64>> = (0..text.len())
        .map(|j| (text[j] == query[0]).then(|| gain(j)))
        .collect();
    for &q in &query[1..] {
        let mut row = vec![None; text.len()];
        // Best score in prev over positions strictly before j - 1 (a gap before this match).
        let mut best_before: Option<i64> = None;
        for j in 1..text.len() {
            if j >= 2 {
                best_before = best_before.max(prev[j - 2]);
            }
            if text[j] != q {
                continue;
            }
            let adjacent = prev[j - 1].map(|s| s + ADJACENT);
            row[j] = adjacent.max(best_before).map(|s| s + gain(j));
        }
        prev = row;
    }
    prev.into_iter().flatten().max()
}

/// A relation of one connection's loaded catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableRef {
    /// The connection's name.
    pub connection: String,
    /// The relation.
    pub rel: RelationRef,
    /// Table, view, or another kind.
    pub kind: RelationKind,
}

impl TableRef {
    fn qualified(&self) -> String {
        format!("{}.{}", self.rel.schema(), self.rel.name())
    }
}

/// How far a connection's catalog has loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogStatus {
    /// Loaded; its tables are in [`Sources::tables`].
    Ready,
    /// Loading now.
    Loading,
    /// Never asked for; the palette does not connect on its own.
    Idle,
    /// The load failed with this message.
    Failed(String),
}

/// One workspace connection and its catalog's status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionSource {
    /// The connection's name.
    pub name: String,
    /// Its catalog's status.
    pub status: CatalogStatus,
}

/// What the palette searches: loaded tables, saved query paths, recent tables (newest first)
/// and every connection in workspace order.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    /// Every relation of every Ready catalog.
    pub tables: Vec<TableRef>,
    /// Saved query paths, as the workspace lists them.
    pub queries: Vec<String>,
    /// Tables opened this session, newest first.
    pub recent: Vec<TableRef>,
    /// Every connection, in workspace order.
    pub connections: Vec<ConnectionSource>,
    /// The effective shortcut list, as [`keymap::shortcuts`] gives it.
    pub shortcuts: Vec<keymap::Shortcut>,
}

/// A command the palette runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteCommand {
    /// Opens a tab.
    NewTab,
    /// Closes the active tab.
    CloseTab,
    /// Saves the active tab's query.
    SaveQuery,
    /// Runs the statement at the caret.
    RunStatement,
    /// Runs the whole editor.
    RunAll,
    /// Formats the statement at the caret.
    FormatSql,
    /// Shows the plan of the statement at the caret.
    Explain,
    /// Shows the plan with ANALYZE timings.
    ExplainAnalyze,
    /// Cancels the active run.
    CancelQuery,
    /// Opens the new connection dialog.
    NewConnection,
    /// Re-reads `dbento.toml`.
    ReloadWorkspace,
    /// Shows or hides history.
    ToggleHistory,
    /// Shows or hides the row inspector.
    ToggleRowInspector,
    /// Moves to the next appearance: System, Dark, Light.
    CycleAppearance,
    /// Opens the Settings dialog.
    OpenSettings,
    /// Switches the palette to the shortcut list.
    KeyboardShortcuts,
}

impl PaletteCommand {
    /// Every command, in the order the Commands group lists them.
    pub const ALL: [PaletteCommand; 16] = [
        Self::NewTab,
        Self::CloseTab,
        Self::SaveQuery,
        Self::RunStatement,
        Self::RunAll,
        Self::FormatSql,
        Self::Explain,
        Self::ExplainAnalyze,
        Self::CancelQuery,
        Self::NewConnection,
        Self::ReloadWorkspace,
        Self::ToggleHistory,
        Self::ToggleRowInspector,
        Self::CycleAppearance,
        Self::OpenSettings,
        Self::KeyboardShortcuts,
    ];

    /// The row's label.
    pub fn label(self) -> &'static str {
        match self {
            Self::NewTab => "New tab",
            Self::CloseTab => "Close tab",
            Self::SaveQuery => "Save query",
            Self::RunStatement => "Run statement",
            Self::RunAll => "Run all",
            Self::FormatSql => "Format SQL",
            Self::Explain => "Explain plan",
            Self::ExplainAnalyze => "Explain analyze",
            Self::CancelQuery => "Cancel query",
            Self::NewConnection => "New connection",
            Self::ReloadWorkspace => "Reload dbento.toml",
            Self::ToggleHistory => "Toggle history",
            Self::ToggleRowInspector => "Toggle row inspector",
            Self::CycleAppearance => "Toggle appearance",
            Self::OpenSettings => "Open settings",
            Self::KeyboardShortcuts => "Keyboard shortcuts",
        }
    }

    /// The keymap action it stands for; None for `KeyboardShortcuts`.
    pub fn action(self) -> Option<Box<dyn Action>> {
        Some(match self {
            Self::NewTab => Box::new(keymap::NewTab),
            Self::CloseTab => Box::new(keymap::CloseTab),
            Self::SaveQuery => Box::new(keymap::SaveQuery),
            Self::RunStatement => Box::new(keymap::RunStatement),
            Self::RunAll => Box::new(keymap::RunAll),
            Self::FormatSql => Box::new(keymap::FormatSql),
            Self::Explain => Box::new(keymap::Explain),
            Self::ExplainAnalyze => Box::new(keymap::ExplainAnalyze),
            Self::CancelQuery => Box::new(keymap::CancelQuery),
            Self::NewConnection => Box::new(keymap::NewConnection),
            Self::ReloadWorkspace => Box::new(keymap::ReloadWorkspace),
            Self::ToggleHistory => Box::new(keymap::ToggleHistory),
            Self::ToggleRowInspector => Box::new(keymap::ToggleRowInspector),
            Self::CycleAppearance => Box::new(CycleAppearance),
            Self::OpenSettings => Box::new(keymap::OpenSettings),
            Self::KeyboardShortcuts => return None,
        })
    }

    /// The keys the effective keymap binds to its action.
    pub fn keys(self, cx: &App) -> Option<SharedString> {
        self.action().and_then(|a| keymap::keys_for(a.as_ref(), cx))
    }
}

/// One palette row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    /// A loaded relation.
    Table(TableRef),
    /// A relation opened earlier this session.
    Recent(TableRef),
    /// The saved query at this index of [`Sources::queries`].
    Query(usize),
    /// A connection, by name.
    Connection(String),
    /// Loads the named connection's catalog.
    SearchIn(String),
    /// The named connection's catalog is loading.
    Loading(String),
    /// Retries the connection whose catalog failed.
    Failed {
        /// The connection.
        connection: String,
        /// Why its catalog failed.
        message: String,
    },
    /// A command.
    Command(PaletteCommand),
    /// A keyboard shortcut, shown for reference: one label with every chord bound to it.
    Shortcut {
        /// What the keys do.
        label: &'static str,
        /// Its chords, in table order.
        keys: Vec<SharedString>,
    },
}

impl Entry {
    /// The row's text: a table as "schema.name · connection", a query by its path, a
    /// connection or status row by name, a command or shortcut by its label.
    pub fn label(&self, src: &Sources) -> String {
        match self {
            Self::Table(t) | Self::Recent(t) => {
                format!("{} \u{b7} {}", t.qualified(), t.connection)
            }
            Self::Query(i) => src.queries.get(*i).cloned().unwrap_or_default(),
            Self::Connection(name) => name.clone(),
            Self::SearchIn(name) => format!("Search tables in {name}"),
            Self::Loading(name) => format!("Loading tables in {name}\u{2026}"),
            Self::Failed {
                connection,
                message,
            } => format!("Retry {connection} \u{b7} {message}"),
            Self::Command(c) => c.label().to_owned(),
            Self::Shortcut { label, .. } => (*label).to_owned(),
        }
    }

    /// The chip keys, one chord per chip: a command's keys or a shortcut's chords; empty otherwise.
    pub fn keys(&self, cx: &App) -> Vec<SharedString> {
        match self {
            Self::Command(c) => c.keys(cx).into_iter().collect(),
            Self::Shortcut { keys, .. } => keys.clone(),
            Self::Table(_)
            | Self::Recent(_)
            | Self::Query(_)
            | Self::Connection(_)
            | Self::SearchIn(_)
            | Self::Loading(_)
            | Self::Failed { .. } => Vec::new(),
        }
    }
}

/// One titled group of rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// The group's heading.
    pub title: &'static str,
    /// Its rows, best match first.
    pub entries: Vec<Entry>,
}

const TABLE_CAP: usize = 50;
const QUERY_CAP: usize = 20;
const RECENT_CAP: usize = 20;

/// The groups for `raw` (prefix included), empty groups left out. All mode: "Tables and views"
/// (50), "Saved queries" (20), "Recent tables" (20), "Connections", "Commands", then "Tables not
/// loaded" (one unranked status row per connection not Ready). `#`: "Saved queries" only, `>`:
/// "Commands" only, `?`: "Keyboard shortcuts" only, each uncapped. Tables and recent rank by
/// "schema.name", queries by path, connections by name, commands and shortcuts by label.
pub fn sections(src: &Sources, raw: &str) -> Vec<Section> {
    let (mode, query) = parse_query(raw);
    // Rank one group's texts and map the surviving indices back to rows, at most `cap` of them.
    let group = |title, texts: Vec<String>, cap: usize, entry: &dyn Fn(usize) -> Entry| {
        let candidates: Vec<Candidate> = texts.into_iter().map(|text| Candidate { text }).collect();
        let entries = rank(query, &candidates)
            .into_iter()
            .take(cap)
            .map(entry)
            .collect();
        Section { title, entries }
    };
    let tables = |cap| {
        let texts = src.tables.iter().map(TableRef::qualified).collect();
        group("Tables and views", texts, cap, &|i| {
            Entry::Table(src.tables[i].clone())
        })
    };
    let queries = |cap| group("Saved queries", src.queries.clone(), cap, &Entry::Query);
    let recent = |cap| {
        let texts = src.recent.iter().map(TableRef::qualified).collect();
        group("Recent tables", texts, cap, &|i| {
            Entry::Recent(src.recent[i].clone())
        })
    };
    let connections = || {
        let texts = src.connections.iter().map(|c| c.name.clone()).collect();
        group("Connections", texts, usize::MAX, &|i| {
            Entry::Connection(src.connections[i].name.clone())
        })
    };
    let commands = || {
        let texts = PaletteCommand::ALL
            .iter()
            .map(|c| c.label().to_owned())
            .collect();
        group("Commands", texts, usize::MAX, &|i| {
            Entry::Command(PaletteCommand::ALL[i])
        })
    };
    let shortcuts = || {
        // One row per label, so a command bound to several chords shows them side by side.
        let mut list: Vec<(&'static str, Vec<SharedString>)> = Vec::new();
        for s in &src.shortcuts {
            match list.iter_mut().find(|(label, _)| *label == s.label) {
                Some((_, keys)) => keys.push(s.keys.clone()),
                None => list.push((s.label, vec![s.keys.clone()])),
            }
        }
        let texts = list.iter().map(|(label, _)| (*label).to_owned()).collect();
        group("Keyboard shortcuts", texts, usize::MAX, &|i| {
            Entry::Shortcut {
                label: list[i].0,
                keys: list[i].1.clone(),
            }
        })
    };
    let groups = match mode {
        Mode::All => vec![
            tables(TABLE_CAP),
            queries(QUERY_CAP),
            recent(RECENT_CAP),
            connections(),
            commands(),
            not_loaded(src),
        ],
        Mode::Queries => vec![queries(usize::MAX)],
        Mode::Commands => vec![commands()],
        Mode::Shortcuts => vec![shortcuts()],
    };
    groups
        .into_iter()
        .filter(|s| !s.entries.is_empty())
        .collect()
}

/// One status row per connection whose catalog is not Ready, whatever the query.
fn not_loaded(src: &Sources) -> Section {
    let entries = src
        .connections
        .iter()
        .filter_map(|c| {
            let name = c.name.clone();
            match &c.status {
                CatalogStatus::Ready => None,
                CatalogStatus::Loading => Some(Entry::Loading(name)),
                CatalogStatus::Idle => Some(Entry::SearchIn(name)),
                CatalogStatus::Failed(message) => Some(Entry::Failed {
                    connection: name,
                    message: message.clone(),
                }),
            }
        })
        .collect();
    Section {
        title: "Tables not loaded",
        entries,
    }
}

/// Puts `t` first, dropping an equal entry, and keeps at most 20.
pub fn push_recent(list: &mut Vec<TableRef>, t: TableRef) {
    list.retain(|x| *x != t);
    list.insert(0, t);
    list.truncate(RECENT_CAP);
}

/// What a confirmed row asks the subscriber to do. The subscriber closes the dialog
/// (`close_dialog`) before acting: the palette entity drops with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteEvent {
    /// Browse a table or show a view.
    OpenTable(TableRef),
    /// Open a saved query.
    OpenQuery(SavedQuery),
    /// Open a tab on the named connection.
    OpenConnection(String),
    /// Run a command.
    Run(PaletteCommand),
}

/// What the palette reads, passed by value: `open` runs inside the shell's own update, so it
/// never reads the `Shell` entity.
pub struct PaletteInit {
    /// The explorer, whose catalog lists the tables.
    pub sidebar: Entity<Sidebar>,
    /// The open workspace, for its connections.
    pub workspace: Arc<Workspace>,
    /// The saved queries, read when the palette opened.
    pub queries: Vec<SavedQuery>,
    /// Tables opened this session, newest first.
    pub recent: Vec<TableRef>,
}

/// The view inside the dialog: root element `palette`, a kit `Command` (filterable off) over
/// [`sections`], each row `("palette-row", n)` labelled [`Entry::label`] with a chip
/// `("palette-keys", n)`, `n` counting rows across groups in display order.
pub struct Palette {
    state: Entity<CommandState>,
    init: PaletteInit,
    // Connections whose catalog the palette loads: open ones at open time, plus those asked for.
    wanted: HashSet<String>,
    // The labels of the last rendered rows, by group, to tell when rows change.
    shown: Vec<Vec<String>>,
    // While rows changed under a pointer that has not moved since, the row that keeps the
    // highlight: the kit hover-selects whatever row lands under a resting pointer.
    pinned: Option<String>,
    // The row the pointer rests on, whose hover the pin turned back.
    under: Option<IndexPath>,
    _sidebar: Subscription,
}

impl EventEmitter<PaletteEvent> for Palette {}

impl Palette {
    // A fresh snapshot each render, so a catalog that lands while the palette is open shows.
    fn sources(&self, cx: &App) -> Sources {
        let tree = self.init.sidebar.read(cx).tree();
        let mut tables = Vec::new();
        let mut connections = Vec::new();
        for conn in self.init.workspace.connections() {
            let name = conn.name();
            let schemas = tree.schemas(name);
            for schema in schemas.into_iter().flatten() {
                let Some(list) = tree.relations(name, schema.name()) else {
                    continue;
                };
                tables.extend(list.items().iter().map(|r| TableRef {
                    connection: name.to_owned(),
                    rel: RelationRef::new(schema.name(), r.name()),
                    kind: r.kind(),
                }));
            }
            // Only the catalog's own failure shows: Retry cannot clear a tab session's.
            let status = match (schemas, tree.link(name)) {
                (Some(_), _) => CatalogStatus::Ready,
                (None, LinkState::Failed(m)) => CatalogStatus::Failed(short_message(&m)),
                (None, _) if self.wanted.contains(name) => CatalogStatus::Loading,
                (None, _) => CatalogStatus::Idle,
            };
            connections.push(ConnectionSource {
                name: name.to_owned(),
                status,
            });
        }
        Sources {
            tables,
            queries: (self.init.queries.iter())
                .map(|q| q.path().display().to_string())
                .collect(),
            recent: self.init.recent.clone(),
            connections,
            shortcuts: keymap::shortcuts(cx),
        }
    }

    /// Asks the sidebar for each wanted connection's schemas and the relations of every
    /// schema loaded so far. Idempotent: levels already loading or loaded send nothing.
    fn pump(&mut self, cx: &mut Context<Self>) {
        let wanted: Vec<String> = self.wanted.iter().cloned().collect();
        self.init.sidebar.update(cx, |sidebar, cx| {
            for conn in wanted {
                let mut queries = vec![CatalogQuery::Schemas];
                let schemas = sidebar.tree().schemas(&conn).unwrap_or_default();
                queries.extend(schemas.iter().map(|s| CatalogQuery::Relations {
                    schema: s.name().to_owned(),
                }));
                sidebar.ensure_loaded(&conn, &queries, cx);
            }
        });
    }

    // Rows that open something emit for the shell; a shortcut row only closes; status rows
    // load their connection and the shortcuts command switches to `? `, both staying open.
    fn confirm(&mut self, entry: &Entry, window: &mut Window, cx: &mut Context<Self>) {
        let event = match entry {
            Entry::Table(t) | Entry::Recent(t) => PaletteEvent::OpenTable(t.clone()),
            Entry::Query(i) => match self.init.queries.get(*i) {
                Some(q) => PaletteEvent::OpenQuery(q.clone()),
                None => return,
            },
            Entry::Connection(name) => PaletteEvent::OpenConnection(name.clone()),
            Entry::Command(PaletteCommand::KeyboardShortcuts) => {
                return self.state.update(cx, |s, cx| s.set_query("? ", window, cx));
            }
            Entry::Command(c) => PaletteEvent::Run(*c),
            Entry::Shortcut { .. } => return window.close_dialog(cx),
            Entry::SearchIn(name) => return self.want(name.clone(), cx),
            Entry::Failed { connection, .. } => {
                let key = NodeKey::Connection(connection.clone());
                self.init.sidebar.update(cx, |s, cx| s.retry(key, cx));
                return self.want(connection.clone(), cx);
            }
            Entry::Loading(_) => return,
        };
        cx.emit(event);
    }

    // Rows that changed keep the highlighted entry, or the first row once it is gone, until
    // the pointer moves.
    fn pin(&mut self, shown: Vec<Vec<String>>, cx: &App) {
        if shown == self.shown {
            return;
        }
        let was = (self.state.read(cx).selected_index())
            .and_then(|ix| self.shown.get(ix.section)?.get(ix.row).cloned());
        let mut labels = shown.iter().flatten();
        self.pinned = match was {
            Some(l) if labels.clone().any(|x| *x == l) => Some(l),
            _ => labels.next().cloned(),
        };
        self.shown = shown;
    }

    // A highlight the pointer took without moving goes back to the pinned row.
    fn hovered(&mut self, ix: IndexPath, window: &mut Window, cx: &mut Context<Self>) {
        if window.last_input_was_keyboard() {
            return;
        }
        let Some(pinned) = self.pinned.as_deref() else {
            return;
        };
        let at = self.shown.iter().enumerate().find_map(|(s, rows)| {
            let r = rows.iter().position(|l| l == pinned)?;
            Some(IndexPath::new(r).section(s))
        });
        if at.is_some_and(|at| at != ix) {
            self.under = Some(ix);
            self.state
                .update(cx, |s, cx| s.set_selected_index(at, window, cx));
        }
    }

    // The pointer moved: the pin ends, and the row it rests on takes the highlight it was
    // denied, since moving within that row raises no new hover.
    fn moved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pinned = None;
        if let Some(ix) = self.under.take() {
            self.state
                .update(cx, |s, cx| s.set_selected_index(Some(ix), window, cx));
        }
    }

    fn want(&mut self, conn: String, cx: &mut Context<Self>) {
        self.wanted.insert(conn);
        self.pump(cx);
        cx.notify();
    }
}

/// An error's first line, cut to 80 characters with an ellipsis.
pub fn short_message(message: &str) -> String {
    let line = message.lines().next().unwrap_or_default();
    if line.chars().count() <= 80 {
        return line.to_owned();
    }
    let mut cut: String = line.chars().take(79).collect();
    cut.push('\u{2026}');
    cut
}

impl Render for Palette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let src = self.sources(cx);
        let query = self.state.read(cx).query(cx);
        let groups = Rc::new(sections(&src, &query));
        let shown = groups
            .iter()
            .map(|g| g.entries.iter().map(|e| e.label(&src)).collect());
        self.pin(shown.collect(), cx);
        let mut command = Command::new(&self.state)
            .bordered(false)
            .filterable(false)
            .placeholder("Search tables, queries and commands; > commands, # queries, ? shortcuts")
            .max_h(window.viewport_size().height * 0.6);
        // An empty `#` list says how to make a query; other modes keep the kit's empty slot.
        if parse_query(&query).0 == Mode::Queries && self.init.queries.is_empty() {
            let dim = theme::dim(cx);
            command = command.empty(move |_, _, _| {
                div()
                    .id("palette-empty")
                    .test_support()
                    .px_3()
                    .py_3()
                    .text_sm()
                    .text_color(dim)
                    .child("No saved queries: Ctrl+S saves one under queries/.")
            });
        }
        let mut n = 0;
        for section in groups.iter() {
            let mut group = CommandGroup::new().label(section.title);
            for entry in &section.entries {
                group = group.item(row(n, entry.label(&src), entry.keys(cx), entry_icon(entry)));
                n += 1;
            }
            command = command.group(group);
        }
        let me = cx.entity().downgrade();
        let on_query = me.clone();
        let on_select = me.clone();
        command = command
            .on_query(move |_, _, cx| {
                let _ = on_query.update(cx, |_, cx| cx.notify());
            })
            .on_select(move |ix, window, cx| {
                let _ = on_select.update(cx, |p, cx| p.hovered(ix, window, cx));
            })
            // With groups only, the path is (group, position in it), as `sections` built them.
            .on_confirm(move |ix, window, cx| {
                let Some(entry) = groups.get(ix.section).and_then(|s| s.entries.get(ix.row)) else {
                    return;
                };
                let _ = me.update(cx, |p, cx| p.confirm(entry, window, cx));
            });
        // The kit's Escape clears a non-empty query before it closes, and a bare mode prefix
        // counts as one; drop the prefix during capture so that Escape reaches the dialog.
        let state = self.state.clone();
        div()
            .id("palette")
            .test_support()
            .on_mouse_move(cx.listener(|p, _, window, cx| p.moved(window, cx)))
            .capture_action(move |_: &Cancel, window, cx| {
                let query = state.read(cx).query(cx);
                if !query.is_empty() && parse_query(&query).1.is_empty() {
                    state.update(cx, |s, cx| s.set_query("", window, cx));
                }
            })
            .child(command)
    }
}

/// Opens the palette dialog (640 px, 80 px from the top, list at most 60% of the window height)
/// with `mode`'s prefix typed and the input focused.
pub fn open(init: PaletteInit, mode: Mode, window: &mut Window, cx: &mut App) -> Entity<Palette> {
    let state = cx.new(|cx| CommandState::new(window, cx));
    let prefix = match mode {
        Mode::All => "",
        Mode::Commands => "> ",
        Mode::Queries => "# ",
        Mode::Shortcuts => "? ",
    };
    state.update(cx, |s, cx| s.set_query(prefix, window, cx));
    // Only connections already open load on their own; the rest wait for a Search row.
    let sidebar = init.sidebar.clone();
    let wanted = (init.workspace.connections().iter())
        .map(|c| c.name())
        .filter(|name| {
            let link = sidebar.read(cx).link(name);
            matches!(link, LinkState::Connected | LinkState::Connecting)
        })
        .map(str::to_owned)
        .collect();
    let palette = cx.new(|cx| Palette {
        state: state.clone(),
        init,
        wanted,
        shown: Vec::new(),
        pinned: None,
        under: None,
        _sidebar: cx.observe(&sidebar, |p: &mut Palette, _, cx| p.pump(cx)),
    });
    palette.update(cx, |p, cx| p.pump(cx));
    let view = palette.clone();
    // Deferred like `confirm::ask`, so the key that asked cannot steal focus back; the
    // dialog focuses its own handle on open, so the input takes focus after it.
    window.defer(cx, move |window, cx| {
        window.open_dialog(cx, move |dialog, _, cx| {
            frame(dialog, px(640.), None, cx)
                .margin_top(px(80.))
                .child(view.clone())
        });
        state.update(cx, |s, cx| s.focus(window, cx));
    });
    palette
}

fn entry_icon(entry: &Entry) -> IconName {
    match entry {
        Entry::Table(t) if t.kind == RelationKind::Table => Icons::Table2,
        Entry::Table(_) => Icons::Layers,
        Entry::Recent(_) => Icons::Clock,
        Entry::Query(_) => Icons::FileCode,
        Entry::Connection(_) => Icons::Database,
        Entry::SearchIn(_) => Icons::Search,
        Entry::Loading(_) => Icons::RotateCw,
        Entry::Failed { .. } => Icons::TriangleAlert,
        Entry::Command(_) => Icons::ChevronRight,
        Entry::Shortcut { .. } => Icons::KeyRound,
    }
}

/// Row `n`: icon, the label with any " · detail" tail muted, and one chip per chord at the
/// right, grouped as `("palette-keys", n)` labelled with the chips joined by ", ".
fn row(n: usize, label: String, keys: Vec<SharedString>, icon: IconName) -> CommandItem {
    CommandItem::new().label(label.clone()).child(move |_, cx| {
        let (dim, muted) = (theme::dim(cx), cx.theme().muted_foreground);
        let (main, detail) = match label.split_once(" \u{b7} ") {
            Some((main, detail)) => (main.to_owned(), Some(detail.to_owned())),
            None => (label.clone(), None),
        };
        h_flex()
            .id(("palette-row", n))
            .aria_label(label.clone())
            .test_support()
            .w_full()
            .gap_2()
            .child(ui::icon(icon, px(14.), muted))
            .child(div().min_w_0().truncate().child(main))
            .when_some(detail, |el, d| {
                el.child(div().min_w_0().truncate().text_color(dim).child(d))
            })
            .child(div().flex_1())
            .when(!keys.is_empty(), |el| {
                let chips: Vec<String> = keys.iter().map(|k| chip_keys(k)).collect();
                el.child(
                    h_flex()
                        .id(("palette-keys", n))
                        .aria_label(chips.join(", "))
                        .test_support()
                        .gap_1()
                        .children(chips.into_iter().map(|c| ui::key_chip(c, cx))),
                )
            })
    })
}
