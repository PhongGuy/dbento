//! The workspace explorer: a collapsible folder tree of connections, marked by colour tag,
//! with each expanded connection's schema tree loaded lazily below it.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use dbento_core::{
    Color, ConnectionConfig, Definition, Engine, FolderPath, ObjectRef, RelationKind, RelationRef,
    Workspace, WorkspaceItem, quote_ident,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, ClipboardItem, Context, DismissEvent, Entity,
    EventEmitter, FocusHandle, Hsla, InteractiveElement, IntoElement, MouseButton, ParentElement,
    Pixels, Render, Role, ScrollStrategy, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Task, TestSupportExt, UniformListScrollHandle, WeakEntity, Window, div, hsla, px,
    relative, uniform_list,
};

use crate::app::Services;
use crate::catalog::{
    CatalogClosing, CatalogData, CatalogQuery, CatalogReply, CatalogRunner, spawn_catalog,
};
use crate::explorer::{
    DraggedItem, DropBar, ExplorerRow, connection_a11y_id, connection_id, drop_allowed, drop_bar,
    explorer_rows, folder_id, part_id, shown_depth,
};
use crate::keymap::{
    NewConnection, OpenSelected, ReloadWorkspace, TreeCollapse, TreeExpand, TreeSelectFirst,
    TreeSelectLast, TreeSelectNext, TreeSelectPrev, keys_for,
};
use crate::link::{LinkState, shows_read_only};
use crate::runner::RunnerSpec;
use crate::schema_tree::{
    CatalogRow, CatalogTree, LoadRequest, NodeKey, ObjectGroup, RevealStep, RowKind,
    catalog_entries, count_text, group_title, insert_sql, lone_schema, node_connection,
    object_group_title, qualified_name, select_text, short_type,
};
use crate::ui::{self, Icons, Notice, NoticeKind};
use crate::{settings, theme, tokio_bridge};

/// What the connection list asks its owner to do.
#[derive(Debug, Clone, PartialEq)]
pub enum SidebarEvent {
    /// Open a query tab on the connection at this index of `Workspace::connections`.
    Open(usize),
    /// Open a tab on connection `connection` (index into `Workspace::connections`) on `rel`:
    /// a browse tab for a table, a `select_text` tab for any other kind.
    OpenRelation {
        /// The connection's index.
        connection: usize,
        /// The relation to select from.
        rel: RelationRef,
        /// The relation's kind, which decides the tab it opens as.
        kind: RelationKind,
    },
    /// Re-read `dbento.toml` and install the result.
    Reload,
    /// Open the connection dialog for a new connection.
    NewConnection,
    /// Open the connection dialog on the named connection.
    Edit(String),
    /// A folder was collapsed or expanded.
    CollapseChanged,
    /// A connection or folder was dropped into folder `to` (None = top level).
    Move {
        /// The dragged connection or folder.
        item: WorkspaceItem,
        /// The folder it was dropped into.
        to: Option<FolderPath>,
    },
    /// Ask for a new folder's name under `parent` (None = top level).
    NewFolder {
        /// The folder the new one goes in.
        parent: Option<FolderPath>,
    },
    /// Ask for a new name for the item.
    Rename(WorkspaceItem),
    /// Set or clear the item's own colour.
    Recolor {
        /// The connection or folder to recolour.
        item: WorkspaceItem,
        /// The new colour; None clears it.
        color: Option<Color>,
    },
    /// Ask, then delete the item.
    Delete(WorkspaceItem),
    /// Open a new tab on the named connection holding `sql` (empty for a blank tab), unrun.
    NewQuery {
        /// The connection's name.
        connection: String,
        /// The tab's text.
        sql: String,
    },
    /// Show a toast; how a windowless catalog pump reports a result.
    Notice(Notice),
    /// Close the named connection's sessions, asking first when edits are staged.
    Disconnect(String),
    /// Open the connection dialog on a new connection copied from the named one.
    Duplicate(String),
    /// Show the definition of `object` on the named connection.
    ViewDefinition {
        /// The connection's name.
        connection: String,
        /// The object to show.
        object: ObjectRef,
    },
    /// A definition load's answer, or its error message.
    Definition {
        /// The connection's name.
        connection: String,
        /// The object asked for.
        object: ObjectRef,
        /// The definition, or the message to show.
        result: Result<Definition, String>,
    },
}

/// The context menu's swatches after "none" at index 0, in `("menu-color", i)` order.
pub const MENU_COLORS: [Color; 8] = [
    Color::Red,
    Color::Orange,
    Color::Yellow,
    Color::Green,
    Color::Teal,
    Color::Blue,
    Color::Purple,
    Color::Gray,
];

/// One row of the explorer list: a folder or connection, or a catalog row with its index
/// among the catalog rows (the `tree-row` id) and its connection, whose folder takes a drop on
/// it.
#[derive(Clone)]
enum SidebarRow {
    Explorer(ExplorerRow),
    Catalog(Box<CatalogRow>, usize, CatalogOwner),
}

/// The connection a catalog row belongs to.
#[derive(Clone)]
struct CatalogOwner {
    name: String,
    folder: Option<FolderPath>,
}

/// The explorer's one selected row, keyed by identity so it survives rebuilds and filtering.
#[derive(Debug, Clone, PartialEq)]
enum Selection {
    Folder(FolderPath),
    Connection(String),
    Node(NodeKey),
    Object { conn: String, object: ObjectRef },
}

/// What opens or closes a row: a folder's collapse flag or a node of the catalog tree.
enum RowToggle {
    Folder(FolderPath),
    Node(NodeKey),
}

// The selection a row takes from a click or a tree key; columns and status rows take none.
// An object row shares its group's key, so it selects by its object instead.
fn row_selection(row: &SidebarRow) -> Option<Selection> {
    match row {
        SidebarRow::Explorer(r) => match &r.item {
            WorkspaceItem::Folder(path) => Some(Selection::Folder(path.clone())),
            WorkspaceItem::Connection(name) => Some(Selection::Connection(name.clone())),
            _ => None,
        },
        SidebarRow::Catalog(r, _, owner) => match &r.kind {
            RowKind::Object { object, .. } => Some(Selection::Object {
                conn: owner.name.clone(),
                object: object.clone(),
            }),
            RowKind::Schema | RowKind::Kind(_) | RowKind::Relation(_) | RowKind::Group(_) => {
                Some(Selection::Node(r.key.clone()))
            }
            _ => None,
        },
    }
}

/// A relation the explorer is opening its way down to; `shown` once every ancestor is open,
/// so the next rebuild scrolls its row into view.
struct PendingReveal {
    conn: String,
    rel: RelationRef,
    shown: bool,
}

/// The explorer over a workspace's folders, connections and their schema trees.
pub struct Sidebar {
    workspace: Arc<Workspace>,
    services: Services,
    selection: Option<Selection>,
    collapsed: HashSet<FolderPath>,
    tree: CatalogTree,
    // One runner per connection expanded so far, with the task that applies its replies.
    catalogs: HashMap<String, (CatalogRunner, Task<()>)>,
    // Set by `close_catalogs`: the window is going, so no new catalog session starts.
    closed: bool,
    catalogs_started: usize,
    filter: Entity<InputState>,
    rows: Rc<Vec<SidebarRow>>,
    scroll: UniformListScrollHandle,
    revealing: Option<PendingReveal>,
    // The row last right-clicked, for the context menu; None is the empty list space.
    menu_target: Option<ExplorerTarget>,
    // Taken on a row click, so Enter and the tree keys reach the "Explorer" context.
    focus: FocusHandle,
    // A Generate INSERT waiting for its relation's columns to load; the newest request wins.
    pending_insert: Option<(String, RelationRef, Engine)>,
    // The merged link of every tab session, per connection name; the shell keeps it current.
    session_links: HashMap<String, LinkState>,
    // The connections with a tab session that reported read-only; the shell keeps it current.
    session_read_only: HashSet<String>,
    // Set while the context menu shows, so no row tooltip draws over it.
    menu_open: bool,
    // The connection row under the pointer, which shows its gear alongside the selected one.
    hovered: Option<String>,
    // Counts selection changes, so the shell can tell a selection made since a tab activation.
    selection_serial: u64,
    // The row last clicked, keyed by its node and whether it is a failed status row; it holds
    // back its tooltip until the pointer leaves it.
    quiet_tip: Option<(NodeKey, bool)>,
    // Connections whose lone schema has opened itself; a later collapse then sticks.
    auto_opened: HashSet<String>,
    _filter_changes: Subscription,
}

impl EventEmitter<SidebarEvent> for Sidebar {}

const INDENT: f32 = 14.;
const INSET: f32 = 10.;
pub(crate) const LEAD: f32 = 12.;
const GLYPH: f32 = 14.;

/// How long the pointer rests on a tree row before its tooltip shows.
pub const TREE_TOOLTIP_DELAY: std::time::Duration = std::time::Duration::from_millis(600);

impl Sidebar {
    /// An explorer over `workspace` with every folder expanded, every connection collapsed
    /// and nothing selected; catalog sessions use `services` for their secrets.
    pub fn new(
        workspace: Arc<Workspace>,
        services: Services,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter loaded names"));
        // `rebuild` reads the filter's text, so a change only needs a render.
        let filter_changes = cx.subscribe_in(&filter, window, |_, _, e: &InputEvent, _, cx| {
            if matches!(e, InputEvent::Change) {
                cx.notify();
            }
        });
        Sidebar {
            workspace,
            services,
            selection: None,
            collapsed: HashSet::new(),
            tree: CatalogTree::new(),
            catalogs: HashMap::new(),
            closed: false,
            catalogs_started: 0,
            filter,
            rows: Rc::default(),
            scroll: UniformListScrollHandle::new(),
            revealing: None,
            menu_target: None,
            focus: cx.focus_handle(),
            pending_insert: None,
            session_links: HashMap::new(),
            session_read_only: HashSet::new(),
            menu_open: false,
            hovered: None,
            selection_serial: 0,
            quiet_tip: None,
            auto_opened: HashSet::new(),
            _filter_changes: filter_changes,
        }
    }

    /// The workspace index of the connection that owns the selection: a selected connection,
    /// or the connection of a selected catalog node. None for a folder or nothing.
    pub fn selected(&self) -> Option<usize> {
        let name = match self.selection.as_ref()? {
            Selection::Connection(name) => name,
            Selection::Node(key) => node_connection(key),
            Selection::Object { conn, .. } => conn,
            Selection::Folder(_) => return None,
        };
        self.workspace
            .connections()
            .iter()
            .position(|c| c.name() == name)
    }

    /// How many times the selection has been set; any `select`, `reveal` or row click moves it.
    pub fn selection_serial(&self) -> u64 {
        self.selection_serial
    }

    /// Replaces the merged tab-session state per connection name; absent names are Idle.
    pub fn set_session_links(&mut self, links: HashMap<String, LinkState>, cx: &mut Context<Self>) {
        if self.session_links != links {
            self.session_links = links;
            cx.notify();
        }
    }

    /// Replaces the connections whose tab session reported read-only.
    pub fn set_session_read_only(&mut self, names: HashSet<String>, cx: &mut Context<Self>) {
        if self.session_read_only != names {
            self.session_read_only = names;
            cx.notify();
        }
    }

    /// Whether connection `c`'s row draws the lock: the badge's rule over its tab sessions.
    pub fn shows_lock(&self, c: &ConnectionConfig) -> bool {
        let session = self.session_read_only.contains(c.name()).then_some(true);
        shows_read_only(c, session)
    }

    /// The catalog's state merged with the sessions'.
    pub fn link(&self, conn: &str) -> LinkState {
        let session = self.session_links.get(conn).cloned().unwrap_or_default();
        self.tree.link(conn).merge(session)
    }

    /// The selected catalog node, if the selection is one.
    pub fn selected_node(&self) -> Option<NodeKey> {
        match &self.selection {
            Some(Selection::Node(key)) => Some(key.clone()),
            _ => None,
        }
    }

    /// The selected object row's connection and object, if the selection is one.
    pub fn selected_object(&self) -> Option<(String, ObjectRef)> {
        match &self.selection {
            Some(Selection::Object { conn, object }) => Some((conn.clone(), object.clone())),
            _ => None,
        }
    }

    /// The selected folder, if the selection is one.
    pub fn selected_folder(&self) -> Option<FolderPath> {
        match &self.selection {
            Some(Selection::Folder(path)) => Some(path.clone()),
            _ => None,
        }
    }

    /// Selects connection `ix`; an index outside the workspace clears the selection.
    pub fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.revealing = None;
        self.selection = self
            .workspace
            .connections()
            .get(ix)
            .map(|c| Selection::Connection(c.name().to_owned()));
        self.selection_serial += 1;
        cx.notify();
    }

    fn set_selection(&mut self, selection: Selection, cx: &mut Context<Self>) {
        self.revealing = None;
        self.selection = Some(selection);
        self.selection_serial += 1;
        cx.notify();
    }

    /// Selects `key` at once and, for a relation, opens its ancestors through their lazy
    /// loads and scrolls its row into view. A newer reveal or select replaces it; a relation
    /// that is gone or a failed load leaves just the selection.
    pub fn reveal(&mut self, key: NodeKey, cx: &mut Context<Self>) {
        self.set_selection(Selection::Node(key.clone()), cx);
        self.expand_folders_of(node_connection(&key), cx);
        // After close no runner answers, so the reveal stops at the selection.
        if let NodeKey::Relation { conn, rel } = key
            && !self.closed
        {
            self.revealing = Some(PendingReveal {
                conn,
                rel,
                shown: false,
            });
            self.advance_reveal(cx);
            self.load_objects(cx);
        }
    }

    // Force-opens every folder above `conn`, so its rows can show; the session saves it.
    fn expand_folders_of(&mut self, conn: &str, cx: &mut Context<Self>) {
        let mut folder = self
            .workspace
            .connection(conn)
            .and_then(|c| c.folder().cloned());
        let mut changed = false;
        while let Some(path) = folder {
            changed |= self.collapsed.remove(&path);
            folder = path.parent();
        }
        if changed {
            cx.emit(SidebarEvent::CollapseChanged);
        }
    }

    // Opens what the loaded levels allow and sends the next load; each applied reply calls
    // this again until the path is open or turns out to be missing.
    fn advance_reveal(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.revealing.as_mut().filter(|p| !p.shown) else {
            return;
        };
        match self.tree.reveal(&p.conn, &p.rel) {
            RevealStep::Waiting(loads) => loads.into_iter().for_each(|r| self.send(r, cx)),
            RevealStep::Shown => p.shown = true,
            RevealStep::Missing => self.revealing = None,
        }
        cx.notify();
    }

    // Once the path is open, scrolls to the relation's row and ends the reveal; a filter or
    // a collapsed folder that hides the row ends it without scrolling.
    fn scroll_to_reveal(&mut self) {
        let Some(p) = self.revealing.take_if(|p| p.shown) else {
            return;
        };
        let found = self.rows.iter().position(|row| {
            matches!(row, SidebarRow::Catalog(r, ..)
                if matches!(r.kind, RowKind::Relation(_))
                    && matches!(&r.key, NodeKey::Relation { conn, rel }
                        if *conn == p.conn && *rel == p.rel))
        });
        if let Some(ix) = found {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
    }

    /// The catalog cache, for completion (m3e) and tests.
    pub fn tree(&self) -> &CatalogTree {
        &self.tree
    }

    /// The filter input's state.
    pub fn filter_state(&self) -> &Entity<InputState> {
        &self.filter
    }

    /// Toggles `key`, starting the connection's catalog runner and its load when needed.
    /// A pending reveal stops, so its next load does not undo the user's collapse.
    pub fn toggle_node(&mut self, key: NodeKey, cx: &mut Context<Self>) {
        self.revealing = None;
        if let Some(request) = self.tree.toggle(&key) {
            self.send(request, cx);
        }
        self.load_objects(cx);
        cx.notify();
    }

    // Starts the objects level of every expanded relation and schema that has none yet.
    fn load_objects(&mut self, cx: &mut Context<Self>) {
        for request in self.tree.objects_to_load() {
            self.send(request, cx);
        }
    }

    /// Sends a definition load of `object` on `connection`'s catalog runner; its reply leaves
    /// as `SidebarEvent::Definition`, never reaching the tree.
    pub fn load_definition(
        &mut self,
        connection: &str,
        object: &ObjectRef,
        cx: &mut Context<Self>,
    ) {
        let request = LoadRequest {
            conn: connection.to_owned(),
            generation: self.tree.generation(),
            query: CatalogQuery::Definition {
                object: object.clone(),
            },
        };
        self.send(request, cx);
    }

    /// Applies `replies` as if `conn`'s catalog runner had sent them; a test seam.
    #[doc(hidden)]
    pub fn apply_replies(
        &mut self,
        conn: &str,
        replies: Vec<CatalogReply>,
        cx: &mut Context<Self>,
    ) {
        self.take_replies(conn, replies, cx);
    }

    /// Reloads the failed level under `key`.
    pub fn retry(&mut self, key: NodeKey, cx: &mut Context<Self>) {
        if let Some(request) = self.tree.retry(&key) {
            self.send(request, cx);
        }
        cx.notify();
    }

    /// Drops `conn`'s cached levels and reloads the expanded ones; other connections keep theirs.
    pub fn refresh_connection(&mut self, conn: &str, cx: &mut Context<Self>) {
        for request in self.tree.refresh_connection(conn) {
            self.send(request, cx);
        }
        // A waiting Generate INSERT on `conn` reloads its columns, as in `refresh`.
        if let Some((pending, rel, _)) = self.pending_insert.clone()
            && pending == conn
        {
            self.ensure_loaded(&pending, &[CatalogQuery::Columns { rel }], cx);
        }
        cx.notify();
    }

    /// Closes `conn`'s catalog runner and forgets its levels (collapsing it); None when it had none.
    pub fn disconnect(&mut self, conn: &str) -> Option<CatalogClosing> {
        self.tree.forget(conn);
        self.auto_opened.remove(conn);
        // A Generate INSERT waiting on this session would never hear back.
        if self
            .pending_insert
            .as_ref()
            .is_some_and(|(c, ..)| c == conn)
        {
            self.pending_insert = None;
        }
        self.catalogs
            .remove(conn)
            .map(|(runner, _pump)| runner.close())
    }

    /// Whether the explorer's context menu is open; row tooltips are off while it is.
    pub fn menu_open(&self) -> bool {
        self.menu_open
    }

    /// Whether connection `connection`'s row holds back its tooltip after a click on it,
    /// until the pointer leaves the row.
    pub fn tooltip_suppressed(&self, connection: &str) -> bool {
        self.tooltip_suppressed_at(&NodeKey::Connection(connection.to_owned()), false)
    }

    /// Whether the row of `key` (its failed status row when `failed`) holds back its tooltip.
    #[doc(hidden)]
    pub fn tooltip_suppressed_at(&self, key: &NodeKey, failed: bool) -> bool {
        self.quiet_tip
            .as_ref()
            .is_some_and(|(k, f)| k == key && *f == failed)
    }

    // Whether a row's tooltip may show: never while the context menu is open, nor on the row
    // clicked last while the pointer is still on it.
    fn tip_allowed(&self, key: &NodeKey, failed: bool) -> bool {
        !self.menu_open && !self.tooltip_suppressed_at(key, failed)
    }

    // A click quiets the clicked row's tooltip; entering any other row or leaving this one
    // lifts it.
    fn quiet_tip_on_click(&mut self, key: &NodeKey, failed: bool, cx: &mut Context<Self>) {
        self.quiet_tip = Some((key.clone(), failed));
        cx.notify();
    }

    fn quiet_tip_on_hover(
        &mut self,
        key: &NodeKey,
        failed: bool,
        over: bool,
        cx: &mut Context<Self>,
    ) {
        let own = self.quiet_tip.as_ref() == Some(&(key.clone(), failed));
        if self.quiet_tip.is_some() && own != over {
            self.quiet_tip = None;
            cx.notify();
        }
    }

    /// Drops every cached level and reloads what is expanded; late replies are ignored.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        for request in self.tree.refresh() {
            self.send(request, cx);
        }
        // A waiting Generate INSERT reloads its columns, since its old reply is now stale.
        if let Some((conn, rel, _)) = self.pending_insert.clone() {
            self.ensure_loaded(&conn, &[CatalogQuery::Columns { rel }], cx);
        }
        cx.notify();
    }

    /// Starts each absent level of `queries` through `CatalogTree::ensure` and the runner.
    pub fn ensure_loaded(&mut self, conn: &str, queries: &[CatalogQuery], cx: &mut Context<Self>) {
        let requests: Vec<_> = queries
            .iter()
            .filter_map(|q| self.tree.ensure(conn, q))
            .collect();
        if requests.is_empty() {
            return;
        }
        requests.into_iter().for_each(|r| self.send(r, cx));
        cx.notify();
    }

    /// Closes every catalog runner; each closing resolves once its session has closed.
    /// No catalog runner starts afterwards.
    pub fn close_catalogs(&mut self) -> Vec<CatalogClosing> {
        self.closed = true;
        self.catalogs
            .drain()
            .map(|(_, (runner, _pump))| runner.close())
            .collect()
    }

    /// How many catalog runners are open.
    pub fn catalog_sessions(&self) -> usize {
        self.catalogs.len()
    }

    /// Catalog runners started since this sidebar was made.
    pub fn catalogs_started(&self) -> usize {
        self.catalogs_started
    }

    /// Whether the folder at `path` is collapsed.
    pub fn is_collapsed(&self, path: &FolderPath) -> bool {
        self.collapsed.contains(path)
    }

    /// The collapsed folder paths, sorted.
    pub fn collapsed_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self
            .collapsed
            .iter()
            .map(|p| p.as_str().to_owned())
            .collect();
        paths.sort();
        paths
    }

    /// Collapses exactly `paths`, leaving out any that are not folders of the workspace.
    pub fn set_collapsed(
        &mut self,
        paths: impl IntoIterator<Item = FolderPath>,
        cx: &mut Context<Self>,
    ) {
        let folders = self.workspace.folders();
        self.collapsed = paths
            .into_iter()
            .filter(|path| folders.iter().any(|f| f.path() == path))
            .collect();
        cx.notify();
    }

    /// Moves each collapsed path to where `map` says it went; a path mapped to None expands.
    pub fn remap_folders(
        &mut self,
        map: impl Fn(&FolderPath) -> Option<FolderPath>,
        cx: &mut Context<Self>,
    ) {
        self.collapsed = self.collapsed.iter().filter_map(map).collect();
        cx.notify();
    }

    /// Swaps the workspace, keeping selection, collapse and expansion by name and path; closes the
    /// catalog sessions of connections whose session fields changed or that are gone.
    pub fn set_workspace(
        &mut self,
        ws: Arc<Workspace>,
        cx: &mut Context<Self>,
    ) -> Vec<CatalogClosing> {
        // Swap first, so every runner `send` starts from here on opens on the new settings.
        let old = std::mem::replace(&mut self.workspace, ws);
        let stale: Vec<String> = self
            .catalogs
            .keys()
            .filter(|name| {
                let (was, now) = (old.connection(name), self.workspace.connection(name));
                !matches!((was, now), (Some(a), Some(b)) if a.same_session(b))
            })
            .cloned()
            .collect();
        let closings: Vec<CatalogClosing> = stale
            .iter()
            .filter_map(|name| self.catalogs.remove(name))
            .map(|(runner, _pump)| runner.close())
            .collect();
        let gone: Vec<String> = old
            .connections()
            .iter()
            .map(|c| c.name().to_owned())
            .filter(|name| self.workspace.connection(name).is_none())
            .collect();
        for name in &gone {
            self.tree.forget(name);
            self.auto_opened.remove(name);
            // Keyed by name, so a later connection of that name starts clean.
            self.session_links.remove(name);
            if self.hovered.as_ref() == Some(name) {
                self.hovered = None;
            }
        }
        if self
            .pending_insert
            .as_ref()
            .is_some_and(|(c, _, _)| gone.contains(c))
        {
            self.pending_insert = None;
            cx.emit(SidebarEvent::Notice(Notice {
                text: "Generate INSERT cancelled: the connection is gone".into(),
                kind: NoticeKind::Warning,
            }));
        }
        if self
            .revealing
            .as_ref()
            .is_some_and(|p| gone.contains(&p.conn))
        {
            self.revealing = None;
        }
        // A changed connection's expanded levels reload through a fresh runner; unchanged ones
        // reload through their open sessions.
        if !stale.is_empty() || !gone.is_empty() {
            self.refresh(cx);
        }
        // The selection is keyed by name and path, so it carries over as is; `selected()`
        // resolves it against the new workspace.
        let folders = self.workspace.folders();
        self.collapsed
            .retain(|path| folders.iter().any(|f| f.path() == path));
        cx.notify();
        closings
    }

    // The runner starts on its connection's first load; the pump applies replies in batches
    // so a burst of loads renders once.
    fn send(&mut self, request: LoadRequest, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        let LoadRequest {
            conn,
            generation,
            query,
        } = request;
        if !self.catalogs.contains_key(&conn) {
            let spec = RunnerSpec {
                workspace: self.workspace.clone(),
                connection: conn.clone(),
                store: self.services.store.clone(),
                env: self.services.env.clone(),
            };
            let runner = spawn_catalog(&tokio_bridge::handle(cx), spec);
            self.catalogs_started += 1;
            let replies = runner.replies();
            let name = conn.clone();
            let pump = cx.spawn(async move |this, cx| {
                while let Ok(first) = replies.recv().await {
                    let mut queued = vec![first];
                    while let Ok(next) = replies.try_recv() {
                        queued.push(next);
                    }
                    let alive = this.update(cx, |s, cx| s.take_replies(&name, queued, cx));
                    if alive.is_err() {
                        return;
                    }
                }
            });
            self.catalogs.insert(conn.clone(), (runner, pump));
        }
        if let Some((runner, _)) = self.catalogs.get(&conn) {
            runner.load(generation, query);
        }
    }

    // Counts leave the batch as notices and definitions as events, and neither reaches the
    // tree; the levels are applied, then a Generate INSERT waiting on one of them copies its
    // SQL or warns that it failed.
    fn take_replies(&mut self, conn: &str, replies: Vec<CatalogReply>, cx: &mut Context<Self>) {
        let mut failed_insert = None;
        let mut schemas_landed = false;
        for r in replies {
            schemas_landed |= matches!(r.query, CatalogQuery::Schemas);
            if let CatalogQuery::Count { rel } = &r.query {
                let notice = match &r.result {
                    Ok(CatalogData::Count(n)) => Notice {
                        text: count_text(rel, *n),
                        kind: NoticeKind::Info,
                    },
                    Ok(_) => Notice {
                        text: "Count failed: unexpected catalog reply".into(),
                        kind: NoticeKind::Warning,
                    },
                    Err(e) => Notice {
                        text: format!("Count failed: {e}"),
                        kind: NoticeKind::Warning,
                    },
                };
                cx.emit(SidebarEvent::Notice(notice));
                continue;
            }
            if let CatalogQuery::Definition { object } = &r.query {
                let result = match r.result {
                    Ok(CatalogData::Definition(d)) => Ok(d),
                    Ok(_) => Err("unexpected catalog reply".to_owned()),
                    Err(e) => Err(e),
                };
                cx.emit(SidebarEvent::Definition {
                    connection: conn.to_owned(),
                    object: object.clone(),
                    result,
                });
                continue;
            }
            if let (Some((c, rel, _)), CatalogQuery::Columns { rel: got }, Err(e)) =
                (&self.pending_insert, &r.query, &r.result)
                && c == conn
                && rel == got
                && self.tree.is_current(conn, r.generation)
            {
                failed_insert = Some(e.clone());
            }
            self.tree.apply(conn, r);
        }
        if let Some(e) = failed_insert {
            self.pending_insert = None;
            cx.emit(SidebarEvent::Notice(Notice {
                text: format!("Generate INSERT failed: {e}"),
                kind: NoticeKind::Warning,
            }));
        } else if self
            .pending_insert
            .as_ref()
            .is_some_and(|(c, _, _)| c == conn)
        {
            self.copy_insert_when_loaded(cx);
        }
        if schemas_landed {
            self.open_lone_schema(conn, cx);
        }
        self.advance_reveal(cx);
        self.load_objects(cx);
        cx.notify();
    }

    // A connection showing a single schema opens it once, so its tables show straight away;
    // a failed load is not an open, so Retry still gets one.
    fn open_lone_schema(&mut self, conn: &str, cx: &mut Context<Self>) {
        if self.auto_opened.contains(conn)
            || !self.tree.is_expanded(&NodeKey::Connection(conn.to_owned()))
        {
            return;
        }
        let Some(schema) = self.tree.schemas(conn).and_then(lone_schema) else {
            return;
        };
        let key = NodeKey::Schema {
            conn: conn.to_owned(),
            schema: schema.to_owned(),
        };
        self.auto_opened.insert(conn.to_owned());
        if let Some(request) = self.tree.expand(&key) {
            self.send(request, cx);
        }
    }

    // Copies the pending INSERT once its columns are in the tree; until then it keeps waiting.
    fn copy_insert_when_loaded(&mut self, cx: &mut Context<Self>) {
        let Some((conn, rel, engine)) = &self.pending_insert else {
            return;
        };
        let Some(columns) = self.tree.columns(conn, rel) else {
            return;
        };
        let names: Vec<&str> = columns.iter().map(|c| c.name()).collect();
        let sql = insert_sql(*engine, rel, &names);
        cx.write_to_clipboard(ClipboardItem::new_string(sql));
        // A delayed copy can land long after the click, so every copy says what it wrote.
        let text = format!("INSERT for {}.{} copied", rel.schema(), rel.name());
        self.pending_insert = None;
        cx.emit(SidebarEvent::Notice(Notice {
            text,
            kind: NoticeKind::Success,
        }));
    }

    // Generate INSERT: copies at once when the columns are loaded, else loads them without
    // expanding the row and copies when they land.
    fn generate_insert(
        &mut self,
        conn: &str,
        rel: &RelationRef,
        engine: Engine,
        cx: &mut Context<Self>,
    ) {
        self.pending_insert = Some((conn.to_owned(), rel.clone(), engine));
        self.copy_insert_when_loaded(cx);
        if self.pending_insert.is_some() {
            // An absent level loads; one that failed earlier is retried rather than awaited.
            let columns = CatalogQuery::Columns { rel: rel.clone() };
            self.ensure_loaded(conn, &[columns], cx);
            let node = NodeKey::Relation {
                conn: conn.to_owned(),
                rel: rel.clone(),
            };
            self.retry(node, cx);
        }
    }

    // Count rows rides the catalog runner; its reply comes back through `take_replies`.
    fn count_rows(&mut self, conn: &str, rel: &RelationRef, cx: &mut Context<Self>) {
        let request = LoadRequest {
            conn: conn.to_owned(),
            generation: self.tree.generation(),
            query: CatalogQuery::Count { rel: rel.clone() },
        };
        self.send(request, cx);
    }

    fn toggle(&mut self, path: &FolderPath, cx: &mut Context<Self>) {
        if !self.collapsed.remove(path) {
            self.collapsed.insert(path.clone());
        }
        cx.emit(SidebarEvent::CollapseChanged);
        cx.notify();
    }

    fn open_relation(
        &mut self,
        conn: &str,
        rel: &RelationRef,
        kind: RelationKind,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self
            .workspace
            .connections()
            .iter()
            .position(|c| c.name() == conn)
        else {
            return;
        };
        // The owner selects the connection once the tab opens, which it may decline.
        cx.emit(SidebarEvent::OpenRelation {
            connection: ix,
            rel: rel.clone(),
            kind,
        });
        cx.notify();
    }

    // Enter opens a selected relation as its double click does and asks for a selected
    // object's definition; any other selection lets the key go on to whatever binds it above.
    fn open_selected(&mut self, _: &OpenSelected, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((connection, object)) = self.selected_object() {
            cx.emit(SidebarEvent::ViewDefinition { connection, object });
            return;
        }
        let Some(Selection::Node(NodeKey::Relation { conn, rel })) = self.selection.clone() else {
            cx.propagate();
            return;
        };
        let kind = self.tree.relations(&conn, rel.schema()).and_then(|l| {
            l.items()
                .iter()
                .find(|r| r.name() == rel.name())
                .map(|r| r.kind())
        });
        match kind {
            Some(kind) => self.open_relation(&conn, &rel, kind, cx),
            None => cx.propagate(),
        }
    }

    // The tree keys walk the rows a click can select, so columns and status rows are passed
    // over. Each one rebuilds first, so a key pressed before the next frame sees fresh rows.
    fn select_prev(&mut self, _: &TreeSelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.step(false, cx);
    }

    fn select_next(&mut self, _: &TreeSelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.step(true, cx);
    }

    fn select_first(&mut self, _: &TreeSelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        self.rebuild(cx);
        if let Some(ix) = (0..self.rows.len()).find(|&i| row_selection(&self.rows[i]).is_some()) {
            self.move_to(ix, cx);
        }
    }

    fn select_last(&mut self, _: &TreeSelectLast, _: &mut Window, cx: &mut Context<Self>) {
        self.rebuild(cx);
        if let Some(ix) = (0..self.rows.len()).rfind(|&i| row_selection(&self.rows[i]).is_some()) {
            self.move_to(ix, cx);
        }
    }

    // One selectable row down or up, stopping at the ends; with no visible selection the walk
    // starts at the first row going down and the last going up.
    fn step(&mut self, down: bool, cx: &mut Context<Self>) {
        self.rebuild(cx);
        let nav: Vec<usize> = (0..self.rows.len())
            .filter(|&i| row_selection(&self.rows[i]).is_some())
            .collect();
        let at = self
            .selected_row()
            .and_then(|ix| nav.iter().position(|&i| i == ix));
        let next = match (at, down) {
            (Some(p), true) => nav.get(p + 1),
            (Some(p), false) => p.checked_sub(1).and_then(|p| nav.get(p)),
            (None, true) => nav.first(),
            (None, false) => nav.last(),
        };
        if let Some(&ix) = next {
            self.move_to(ix, cx);
        }
    }

    // Right opens a closed row through the same path as its chevron, lazy loads included; on
    // an open row it moves to the first child, and on a leaf it does nothing.
    fn expand_selected(&mut self, _: &TreeExpand, _: &mut Window, cx: &mut Context<Self>) {
        self.rebuild(cx);
        let Some(ix) = self.selected_row() else {
            return;
        };
        match self.row_toggle(ix) {
            Some((false, toggle)) => self.apply_toggle(toggle, cx),
            Some((true, _)) if self.child_of(ix) => self.move_to(ix + 1, cx),
            _ => {}
        }
    }

    // Left closes an open row; on a closed row or a leaf it moves to the parent.
    fn collapse_selected(&mut self, _: &TreeCollapse, _: &mut Window, cx: &mut Context<Self>) {
        self.rebuild(cx);
        let Some(ix) = self.selected_row() else {
            return;
        };
        match self.row_toggle(ix) {
            Some((true, toggle)) => self.apply_toggle(toggle, cx),
            _ => {
                if let Some(parent) = self.parent_row(ix) {
                    self.move_to(parent, cx);
                }
            }
        }
    }

    // The visible row holding the selection, if any.
    fn selected_row(&self) -> Option<usize> {
        let selection = self.selection.as_ref()?;
        self.rows
            .iter()
            .position(|row| row_selection(row).as_ref() == Some(selection))
    }

    fn move_to(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(selection) = self.rows.get(ix).and_then(row_selection) else {
            return;
        };
        if self.selection.as_ref() != Some(&selection) {
            self.set_selection(selection, cx);
        }
        self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
    }

    // Whether row `ix` is open, and what toggles it; None for a row that cannot open.
    fn row_toggle(&self, ix: usize) -> Option<(bool, RowToggle)> {
        match self.rows.get(ix)? {
            SidebarRow::Explorer(r) => match &r.item {
                WorkspaceItem::Folder(path) => Some((r.expanded, RowToggle::Folder(path.clone()))),
                WorkspaceItem::Connection(name) => {
                    let key = NodeKey::Connection(name.clone());
                    Some((self.tree.is_expanded(&key), RowToggle::Node(key)))
                }
                _ => None,
            },
            // An object row is a leaf, though it carries its group's key.
            SidebarRow::Catalog(r, ..) if matches!(r.kind, RowKind::Object { .. }) => None,
            SidebarRow::Catalog(r, ..) => {
                row_selection(&self.rows[ix]).map(|_| (r.expanded, RowToggle::Node(r.key.clone())))
            }
        }
    }

    fn apply_toggle(&mut self, toggle: RowToggle, cx: &mut Context<Self>) {
        match toggle {
            RowToggle::Folder(path) => self.toggle(&path, cx),
            RowToggle::Node(key) => self.toggle_node(key, cx),
        }
    }

    // Whether the row after `ix` is a selectable child of it. Folder and connection rows are
    // matched by path, since deep folders share one capped indent.
    fn child_of(&self, ix: usize) -> bool {
        let Some(next) = self.rows.get(ix + 1).filter(|r| row_selection(r).is_some()) else {
            return false;
        };
        match (&self.rows[ix], next) {
            (SidebarRow::Explorer(r), SidebarRow::Explorer(n)) => match (&r.item, &n.item) {
                (WorkspaceItem::Folder(p), WorkspaceItem::Folder(q)) => {
                    q.parent().as_ref() == Some(p)
                }
                (WorkspaceItem::Folder(p), WorkspaceItem::Connection(c)) => {
                    self.workspace.connection(c).and_then(|c| c.folder()) == Some(p)
                }
                _ => false,
            },
            (SidebarRow::Explorer(r), SidebarRow::Catalog(..)) => {
                matches!(r.item, WorkspaceItem::Connection(_))
            }
            (SidebarRow::Catalog(r, ..), SidebarRow::Catalog(n, ..)) => n.depth > r.depth,
            _ => false,
        }
    }

    // The parent's visible row: a folder or connection's folder by path, a catalog row's
    // nearest shallower row above it.
    fn parent_row(&self, ix: usize) -> Option<usize> {
        let folder_row = |path: &FolderPath| {
            self.rows.iter().position(|r| {
                matches!(r, SidebarRow::Explorer(e)
                    if matches!(&e.item, WorkspaceItem::Folder(p) if p == path))
            })
        };
        match self.rows.get(ix)? {
            SidebarRow::Explorer(r) => match &r.item {
                WorkspaceItem::Folder(path) => folder_row(&path.parent()?),
                WorkspaceItem::Connection(name) => {
                    folder_row(self.workspace.connection(name)?.folder()?)
                }
                _ => None,
            },
            SidebarRow::Catalog(r, ..) => (0..ix).rev().find(|&i| match &self.rows[i] {
                SidebarRow::Explorer(_) => true,
                SidebarRow::Catalog(p, ..) => p.depth < r.depth,
            }),
        }
    }

    fn open(&mut self, ix: usize, cx: &mut Context<Self>) {
        // The row click already selected; the owner focuses or opens the connection's tab.
        cx.emit(SidebarEvent::Open(ix));
        cx.notify();
    }

    // Each connection row is followed by its catalog rows, one level deeper.
    fn rebuild(&mut self, cx: &Context<Self>) {
        let filter = self.filter.read(cx).value();
        let mut rows = Vec::new();
        let mut visible = 0;
        for row in explorer_rows(&self.workspace, &self.collapsed) {
            let conn = match &row.item {
                WorkspaceItem::Connection(name) => self.workspace.connection(name),
                _ => None,
            };
            // Deep folders stop indenting; catalog rows indent from the capped depth.
            let depth = shown_depth(row.depth);
            rows.push(SidebarRow::Explorer(ExplorerRow { depth, ..row }));
            let Some(conn) = conn else { continue };
            for c in self.tree.rows(conn.name(), depth + 1, &filter) {
                let owner = CatalogOwner {
                    name: conn.name().to_owned(),
                    folder: conn.folder().cloned(),
                };
                rows.push(SidebarRow::Catalog(Box::new(c), visible, owner));
                visible += 1;
            }
        }
        self.rows = Rc::new(rows);
    }

    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let rows = self.rows.clone();
        rows[range.start.min(rows.len())..range.end.min(rows.len())]
            .iter()
            .map(|row| match row {
                SidebarRow::Explorer(r) => self.explorer_row(r, cx),
                SidebarRow::Catalog(r, visible, owner) => self.catalog_row(r, *visible, owner, cx),
            })
            .collect()
    }

    // Every row reserves a 12 px lead slot (chevron or nothing) and the accent bar's 2 px,
    // so names line up per depth whether or not a row is tagged, expandable or selected.
    // Each ancestor depth draws a hairline guide centred under that ancestor's lead slot.
    pub(crate) fn row_base(depth: usize, guide: Hsla, height: Pixels) -> gpui_kit::Div {
        let guides = (0..depth).map(move |d| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(INSET + INDENT * d as f32 + LEAD / 2. - 0.5))
                .w(px(1.))
                .bg(guide)
        });
        div()
            .relative()
            .children(guides)
            .w_full()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .h(height)
            .pl(px(INSET + INDENT * depth as f32))
            .pr_3()
            .min_w_0()
            .border_l_2()
            .overflow_hidden()
            .whitespace_nowrap()
    }

    // A row that takes a dragged item into `target`, highlighted while one hovers it. A refused
    // drop is still taken by the row, so it never falls through to the list body.
    fn drop_target<E: StatefulInteractiveElement>(
        row: E,
        target: Option<FolderPath>,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> E {
        let theme = cx.theme();
        let wash = theme.drop_target;
        let bar = match drop_bar(selected) {
            DropBar::Accent => theme.primary,
            DropBar::Drop => theme.drag_border,
        };
        let allowed = target.clone();
        row.can_drop(move |d, _, _| {
            d.downcast_ref::<DraggedItem>()
                .is_some_and(|d| drop_allowed(&d.0, allowed.as_ref()))
        })
        .drag_over(move |style, _: &DraggedItem, _, _| style.bg(wash).border_color(bar))
        .on_drop(cx.listener(move |_, d: &DraggedItem, _, cx| {
            cx.emit(SidebarEvent::Move {
                item: d.0.clone(),
                to: target.clone(),
            });
        }))
    }

    // Folder and connection rows drag themselves, shown as a name chip under the pointer.
    fn draggable<E: StatefulInteractiveElement>(row: E, item: WorkspaceItem) -> E {
        row.on_drag(DraggedItem(item), |d, _, _, cx| {
            cx.new(|_| DragPreview(d.0.clone()))
        })
    }

    // A right-click records `item` for the context menu, which builds after the mouse down.
    fn menu_target<E: InteractiveElement>(row: E, item: ExplorerTarget, cx: &Context<Self>) -> E {
        row.on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, _, _, _| this.menu_target = Some(item.clone())),
        )
    }

    pub(crate) fn chevron(expanded: bool, color: Hsla) -> Icon {
        let name = if expanded {
            Icons::ChevronDown
        } else {
            Icons::ChevronRight
        };
        Icon::new(name).size_3().text_color(color)
    }

    // The 14 px node glyph after the lead slot; an empty slot keeps status text aligned.
    fn glyph(name: Option<IconName>, color: Hsla) -> gpui_kit::Div {
        let slot = div().size(px(GLYPH)).flex_none().flex().items_center();
        match name {
            Some(name) => slot.child(ui::icon(name, px(GLYPH), color)),
            None => slot,
        }
    }

    fn explorer_row(&mut self, row: &ExplorerRow, cx: &mut Context<Self>) -> AnyElement {
        let dim = theme::dim(cx);
        let theme = cx.theme();
        let (active, muted, accent, chrome, guide) = (
            theme.list_active,
            theme.muted_foreground,
            theme.primary,
            theme.sidebar,
            theme.border,
        );
        let ws = self.workspace.clone();
        let base = Self::row_base(row.depth, guide, settings::active_metrics(cx).tree_row)
            .cursor_pointer();
        let lead = || div().w(px(LEAD)).flex_none().flex().justify_center();
        match &row.item {
            WorkspaceItem::Folder(path) => {
                // A tagged folder marks itself with a dot and a stripe in its colour; no wash.
                let color = ws.folders()[row.index].effective_color();
                let selected = self.selection.as_ref() == Some(&Selection::Folder(path.clone()));
                let bg = if selected { active } else { chrome };
                // The row selects; only the chevron collapses, keeping its click to itself.
                let toggle = lead()
                    .id(part_id("folder-toggle", path.as_str()))
                    .test_support()
                    .child(Self::chevron(row.expanded, muted))
                    .on_click(cx.listener({
                        let path = path.clone();
                        move |this, e: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            if e.click_count() == 1 {
                                this.toggle(&path, cx);
                            }
                        }
                    }));
                let row_el = base
                    .id(folder_id(path))
                    .aria_label(path.as_str().to_owned())
                    .aria_selected(selected)
                    .test_support()
                    .bg(bg)
                    .border_color(if selected {
                        accent
                    } else {
                        color.map_or(chrome, tag_color)
                    })
                    .on_click(cx.listener({
                        let path = path.clone();
                        move |this, _, window, cx| {
                            window.focus(&this.focus, cx);
                            this.set_selection(Selection::Folder(path.clone()), cx)
                        }
                    }))
                    .child(toggle)
                    .child(Self::glyph(
                        Some(if row.expanded {
                            Icons::FolderOpen
                        } else {
                            Icons::Folder
                        }),
                        muted,
                    ))
                    // The dot's slot is always reserved, as on connection rows.
                    .child(
                        div()
                            .id(part_id("folder-dot", path.as_str()))
                            .aria_label(color.map_or("none", color_name))
                            .test_support()
                            .size(px(8.))
                            .flex_none()
                            .rounded_full()
                            .when_some(color, |dot, c| dot.bg(tag_color(c))),
                    )
                    .child(div().min_w_0().truncate().child(path.name().to_owned()));
                let target = ExplorerTarget::Workspace(row.item.clone());
                let row_el = Self::menu_target(row_el, target, cx);
                let row_el = Self::draggable(row_el, row.item.clone());
                Self::drop_target(row_el, Some(path.clone()), selected, cx).into_any_element()
            }
            WorkspaceItem::Connection(_) => {
                let (ix, c) = (row.index, &ws.connections()[row.index]);
                let key = NodeKey::Connection(c.name().to_owned());
                let expanded = self.tree.is_expanded(&key);
                // The chevron expands without opening a tab, so it keeps the click to itself.
                let toggle = lead()
                    .id(part_id("connection-toggle", c.name()))
                    .test_support()
                    .child(Self::chevron(expanded, muted))
                    .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        if e.click_count() == 1 {
                            this.toggle_node(key.clone(), cx);
                        }
                    }));
                // The dot's slot is always reserved, so tagged and untagged names line up.
                let dot = div().size(px(8.)).flex_none().rounded_full();
                let dot = match c.effective_color() {
                    Some(color) => dot
                        .id(part_id("connection-color", c.name()))
                        .aria_label(color_name(color))
                        .test_support()
                        .bg(tag_color(color))
                        .into_any_element(),
                    None => dot.into_any_element(),
                };
                let selected =
                    self.selection.as_ref() == Some(&Selection::Connection(c.name().to_owned()));
                let bg = if selected { active } else { chrome };
                let tip: SharedString = connection_tooltip(c).into();
                // The edit button shows only on the hovered or selected row, and keeps its click
                // to itself so it never opens a tab. Its kinds start outside `connection-`, so
                // the pilot's `kind-name` spelling never matches a connection named `edit-x`.
                let name = c.name().to_owned();
                let hovered = self.hovered.as_deref() == Some(c.name());
                let gear = theme::affordance(cx);
                let edit = (selected || hovered).then(|| {
                    Button::new(part_id("edit-connection", c.name()))
                        .ghost()
                        .xsmall()
                        .child(
                            div()
                                .id(part_id("edit-glyph", c.name()))
                                .test_support()
                                .size(px(GLYPH))
                                .child(ui::icon(Icons::Settings, px(GLYPH), gear)),
                        )
                        .tooltip("Edit connection")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.stop_propagation();
                            cx.emit(SidebarEvent::Edit(name.clone()));
                        }))
                });
                // The hovered row alone offers a blank query tab on its connection.
                let query_name = c.name().to_owned();
                let new_query = hovered.then(|| {
                    Button::new(part_id("connection-new-query", c.name()))
                        .ghost()
                        .xsmall()
                        .child(ui::icon(Icons::Plus, px(GLYPH), gear))
                        .tooltip("New query")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.stop_propagation();
                            cx.emit(SidebarEvent::NewQuery {
                                connection: query_name.clone(),
                                sql: String::new(),
                            });
                        }))
                });
                // Leaving a row clears the hover only if no other row has claimed it since.
                let hover_name = c.name().to_owned();
                let tip_key = NodeKey::Connection(c.name().to_owned());
                let click_key = tip_key.clone();
                let show_tip = self.tip_allowed(&tip_key, false);
                let row_el = base
                    .id(connection_id(c.name()))
                    .role(Role::TreeItem)
                    .accessibility_id(connection_a11y_id(c.name()))
                    .aria_label(c.name().to_owned())
                    .aria_selected(selected)
                    .test_support()
                    .bg(bg)
                    .border_color(if selected { accent } else { bg })
                    // Every click selects; only the second click of a double click opens, so
                    // a triple click opens once.
                    .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                        window.focus(&this.focus, cx);
                        this.quiet_tip_on_click(&click_key, false, cx);
                        this.select(ix, cx);
                        if e.click_count() == 2 {
                            this.open(ix, cx);
                        }
                    }))
                    .on_hover(cx.listener(move |this, over: &bool, _, cx| {
                        if *over {
                            this.hovered = Some(hover_name.clone());
                        } else if this.hovered.as_deref() == Some(&hover_name) {
                            this.hovered = None;
                        }
                        this.quiet_tip_on_hover(&tip_key, false, *over, cx);
                        cx.notify();
                    }))
                    .tooltip_show_delay(TREE_TOOLTIP_DELAY)
                    .when(show_tip, |row| {
                        row.tooltip(move |window, cx| theme::tooltip(tip.clone(), window, cx))
                    })
                    .child(toggle)
                    .child(Self::glyph(Some(engine_icon(c.engine())), muted))
                    .child(dot)
                    .child(div().min_w_0().truncate().child(c.name().to_owned()))
                    .when_some(
                        link_glyph(c.name(), &self.link(c.name()), cx),
                        |row, glyph| row.child(glyph),
                    )
                    .child(div().flex_1())
                    .when(self.shows_lock(c), |row| {
                        row.child(
                            div()
                                .id(part_id("connection-lock", c.name()))
                                .aria_label("read-only")
                                .test_support()
                                .flex_none()
                                .child(ui::icon(Icons::Lock, px(GLYPH), dim)),
                        )
                    })
                    .children(new_query)
                    .children(edit);
                let target = ExplorerTarget::Workspace(row.item.clone());
                let row_el = Self::menu_target(row_el, target, cx);
                let row_el = Self::draggable(row_el, row.item.clone());
                Self::drop_target(row_el, c.folder().cloned(), selected, cx).into_any_element()
            }
            // A tree item added to core later has no row until the explorer learns it.
            _ => Self::row_base(0, guide, settings::active_metrics(cx).tree_row).into_any_element(),
        }
    }

    // Schema, kind, relation and object group rows select on a click and expand only through
    // their chevron; a relation opens as a SELECT on the double click's second, an object asks
    // for its definition. Columns and status rows are inert, apart from a failed row's Retry.
    fn catalog_row(
        &mut self,
        row: &CatalogRow,
        visible: usize,
        owner: &CatalogOwner,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let dim = theme::dim(cx);
        let theme = cx.theme();
        let (muted, danger, chrome, guide, accent, active) = (
            theme.muted_foreground,
            theme.danger,
            theme.sidebar,
            theme.border,
            theme.primary,
            theme.list_active,
        );
        let (mono, mono_size) = (theme.mono_font_family.clone(), theme.mono_font_size);
        let mono_dim = |text: String| {
            div()
                .flex_none()
                .font_family(mono.clone())
                .text_size(mono_size)
                .text_color(dim)
                .child(text)
        };
        let expandable = matches!(
            row.kind,
            RowKind::Schema | RowKind::Kind(_) | RowKind::Relation(_) | RowKind::Group(_)
        );
        let lead = div().w(px(LEAD)).flex_none().flex().justify_center();
        // The chevron keeps its click to itself, so a double click on it toggles once.
        let lead = if expandable {
            let key = row.key.clone();
            lead.id(("tree-toggle", visible))
                .test_support()
                .child(Self::chevron(row.expanded, muted))
                .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    if e.click_count() == 1 {
                        this.toggle_node(key.clone(), cx);
                    }
                }))
                .into_any_element()
        } else {
            lead.into_any_element()
        };
        let name = |text: String| div().min_w_0().truncate().child(text);
        let icon = node_icon(&row.kind);
        let tint = if matches!(
            row.kind,
            RowKind::Column {
                primary_key: true,
                ..
            }
        ) {
            accent
        } else {
            muted
        };
        let selected = match &row.kind {
            RowKind::Object { object, .. } => {
                self.selection.as_ref()
                    == Some(&Selection::Object {
                        conn: owner.name.clone(),
                        object: object.clone(),
                    })
            }
            _ => expandable && self.selection.as_ref() == Some(&Selection::Node(row.key.clone())),
        };
        let bg = if selected { active } else { chrome };
        let base = Self::row_base(row.depth, guide, settings::active_metrics(cx).tree_row)
            .id(("tree-row", visible))
            .aria_label(row.aria.clone())
            .aria_selected(selected)
            .test_support()
            .bg(bg)
            .border_color(if selected { accent } else { bg })
            .child(lead)
            .child(Self::glyph(icon, tint));
        // A drop on a catalog row lands in its connection's folder; a right-click opens the
        // row's own menu, never the connection's.
        let base = Self::drop_target(base, owner.folder.clone(), selected, cx);
        let target = ExplorerTarget::Catalog(Box::new(CatalogTarget {
            connection: owner.name.clone(),
            node: row.key.clone(),
            kind: row.kind.clone(),
            label: row.label.clone(),
        }));
        let base = Self::menu_target(base, target, cx);
        let base = if expandable {
            let key = row.key.clone();
            let kind = match row.kind {
                RowKind::Relation(kind) => Some(kind),
                _ => None,
            };
            base.cursor_pointer()
                .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                    window.focus(&this.focus, cx);
                    this.quiet_tip_on_click(&key, false, cx);
                    this.set_selection(Selection::Node(key.clone()), cx);
                    if let (NodeKey::Relation { conn, rel }, Some(kind), 2) =
                        (&key, kind, e.click_count())
                    {
                        this.open_relation(conn, rel, kind, cx);
                    }
                }))
        } else if let RowKind::Object { object, .. } = &row.kind {
            let (conn, object) = (owner.name.clone(), object.clone());
            base.cursor_pointer()
                .on_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
                    window.focus(&this.focus, cx);
                    let selection = Selection::Object {
                        conn: conn.clone(),
                        object: object.clone(),
                    };
                    this.set_selection(selection, cx);
                    if e.click_count() == 2 {
                        cx.emit(SidebarEvent::ViewDefinition {
                            connection: conn.clone(),
                            object: object.clone(),
                        });
                    }
                }))
        } else {
            base
        };
        match &row.kind {
            RowKind::Schema => base.child(name(row.label.clone())),
            RowKind::Relation(_) => {
                let tip: Option<SharedString> = match &row.key {
                    NodeKey::Relation { rel, .. } if self.tip_allowed(&row.key, false) => {
                        Some(relation_tooltip(rel).into())
                    }
                    _ => None,
                };
                let key = row.key.clone();
                base.on_hover(cx.listener(move |this, over: &bool, _, cx| {
                    this.quiet_tip_on_hover(&key, false, *over, cx)
                }))
                .tooltip_show_delay(TREE_TOOLTIP_DELAY)
                .when_some(tip, |row, tip| {
                    row.tooltip(move |window, cx| theme::tooltip(tip.clone(), window, cx))
                })
                .child(name(row.label.clone()))
            }
            RowKind::Kind(kind) => {
                let count = match &row.key {
                    NodeKey::Kind { conn, schema, .. } => {
                        self.tree.relations(conn, schema).map_or(0, |l| {
                            l.items().iter().filter(|r| r.kind() == *kind).count()
                        })
                    }
                    _ => 0,
                };
                base.text_color(muted)
                    .child(name(group_title(*kind).to_owned()))
                    .child(div().flex_1())
                    .child(mono_dim(count.to_string()))
            }
            RowKind::Group(group) => base
                .text_color(muted)
                .child(name(object_group_title(*group).to_owned()))
                .child(div().flex_1())
                .child(mono_dim(self.tree.group_count(&row.key).to_string())),
            RowKind::Object { detail, .. } => base
                .child(name(row.label.clone()))
                .child(mono_dim(detail.clone()).min_w_0().truncate()),
            RowKind::Column { type_name, .. } => base
                // A long type shrinks before the column name, which keeps most of the row.
                .child(
                    div()
                        .flex_none()
                        .max_w(relative(0.7))
                        .truncate()
                        .child(row.label.clone()),
                )
                .child(
                    mono_dim(short_type(type_name))
                        .flex_initial()
                        .min_w_0()
                        .truncate(),
                ),
            RowKind::Failed { message } => {
                let key = row.key.clone();
                let message: SharedString = message.clone().into();
                let show_tip = self.tip_allowed(&key, true);
                let (click_key, hover_key) = (key.clone(), key.clone());
                base.text_color(danger)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.quiet_tip_on_click(&click_key, true, cx)
                    }))
                    .on_hover(cx.listener(move |this, over: &bool, _, cx| {
                        this.quiet_tip_on_hover(&hover_key, true, *over, cx)
                    }))
                    .tooltip_show_delay(TREE_TOOLTIP_DELAY)
                    .when(show_tip, |row| {
                        row.tooltip(move |window, cx| theme::tooltip(message.clone(), window, cx))
                    })
                    .child(name(row.label.clone()).flex_1())
                    .child(
                        Button::new(("tree-retry", visible))
                            .outline()
                            .xsmall()
                            .label("Retry")
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.retry(key.clone(), cx)),
                            ),
                    )
            }
            RowKind::Truncated => base.text_color(muted).child(name(row.label.clone())),
            _ => base.text_color(dim).child(name(row.label.clone())),
        }
        .into_any_element()
    }
}

// A connection's link as a 12 px mark keyed `connection-status`, its aria the state's word or,
// once failed, the full error (also its tooltip). Idle draws nothing.
fn link_glyph(conn: &str, link: &LinkState, cx: &App) -> Option<AnyElement> {
    let theme = cx.theme();
    let (label, mark) = match link {
        LinkState::Idle => return None,
        LinkState::Connecting => (
            link.word().to_owned(),
            Spinner::new()
                .with_size(gpui_kit::component::Size::Size(px(12.)))
                .color(theme.muted_foreground)
                .into_any_element(),
        ),
        LinkState::Connected => (
            link.word().to_owned(),
            div()
                .size(px(6.))
                .rounded_full()
                .bg(theme.success)
                .into_any_element(),
        ),
        LinkState::Failed(e) => (
            e.clone(),
            ui::icon(Icons::CircleX, px(12.), theme.danger).into_any_element(),
        ),
    };
    let failed = matches!(link, LinkState::Failed(_)).then(|| SharedString::from(label.clone()));
    let glyph = div()
        .id(part_id("connection-status", conn))
        .aria_label(label)
        .test_support()
        .size(px(12.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(mark)
        .when_some(failed, |g, tip| {
            g.tooltip(move |window, cx| theme::tooltip(tip.clone(), window, cx))
        });
    Some(glyph.into_any_element())
}

/// The dot color of a tag: one fixed hue per color, readable on both themes.
pub fn tag_color(color: Color) -> Hsla {
    let (hue, sat, light) = match color {
        Color::Red => (0., 0.80, 0.55),
        Color::Orange => (28., 0.90, 0.55),
        Color::Yellow => (48., 0.90, 0.50),
        Color::Green => (142., 0.65, 0.42),
        Color::Teal => (174., 0.70, 0.40),
        Color::Blue => (217., 0.85, 0.58),
        Color::Purple => (270., 0.65, 0.62),
        // A tag added to core later shows as gray until it gets its own hue.
        _ => (220., 0.08, 0.55),
    };
    hsla(hue / 360., sat, light, 1.)
}

/// The tag's name as written in `dbento.toml`.
pub fn color_name(color: Color) -> &'static str {
    match color {
        Color::Red => "red",
        Color::Orange => "orange",
        Color::Yellow => "yellow",
        Color::Green => "green",
        Color::Teal => "teal",
        Color::Blue => "blue",
        Color::Purple => "purple",
        _ => "gray",
    }
}

/// The glyph a catalog row leads with: Layers for a schema, Eye for views, Table2 for other
/// relation kinds, KeyRound or Columns3 for a column, an object group's glyph for the group
/// and its objects alike, and none for a status row.
pub fn node_icon(kind: &RowKind) -> Option<IconName> {
    let relation = |k: &RelationKind| match k {
        RelationKind::View | RelationKind::MaterializedView => Icons::Eye,
        _ => Icons::Table2,
    };
    match kind {
        RowKind::Schema => Some(Icons::Layers),
        RowKind::Kind(k) | RowKind::Relation(k) => Some(relation(k)),
        RowKind::Column {
            primary_key: true, ..
        } => Some(Icons::KeyRound),
        RowKind::Column { .. } => Some(Icons::Columns3),
        RowKind::Group(g) | RowKind::Object { group: g, .. } => Some(match g {
            ObjectGroup::Keys => Icons::Key,
            ObjectGroup::Indexes => Icons::Hash,
            ObjectGroup::Checks => Icons::ShieldCheck,
            ObjectGroup::Triggers => Icons::Zap,
            ObjectGroup::Types => Icons::Shapes,
            ObjectGroup::Routines => Icons::SquareFunction,
        }),
        _ => None,
    }
}

/// A connection's glyph by engine: generic shapes, never vendor logos.
pub fn engine_icon(engine: Engine) -> IconName {
    match engine {
        Engine::Postgres => Icons::Server,
        Engine::Sqlite => Icons::FileBox,
        Engine::Mysql => Icons::Database,
        Engine::Duckdb => Icons::FileBox,
        Engine::Mssql => Icons::Server,
        Engine::Surrealdb => Icons::Layers,
        Engine::Mongodb => Icons::FileCode,
        _ => Icons::Database,
    }
}

/// A connection row's tooltip: the engine, where it points and how to open it,
/// "postgres · host:5432/db · Double-click to open a query tab".
pub fn connection_tooltip(c: &ConnectionConfig) -> String {
    format!(
        "{} \u{b7} {} \u{b7} Double-click to open a query tab",
        engine_label(c.engine()),
        c.display_target()
    )
}

/// A relation row's tooltip: its qualified name and how to open it.
pub fn relation_tooltip(rel: &RelationRef) -> String {
    format!(
        "{}.{} \u{b7} Double-click or Enter to open",
        rel.schema(),
        rel.name()
    )
}

/// The driver name a connection's tooltip leads with.
pub fn engine_label(engine: Engine) -> &'static str {
    engine.name()
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.rebuild(cx);
        self.scroll_to_reveal();
        let theme = cx.theme();
        let (fg, chrome) = (theme.sidebar_foreground, theme.sidebar);
        let header = ui::panel_header("Explorer", cx)
            .child(
                Button::new("connection-new")
                    .ghost()
                    .with_size(px(24.))
                    .icon(Icons::Plus)
                    .tooltip_with_action("New connection", &NewConnection, None)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::NewConnection))),
            )
            .child(
                Button::new("schema-refresh")
                    .ghost()
                    .with_size(px(24.))
                    .icon(Icons::RotateCw)
                    .tooltip("Refresh")
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            );
        let filter = ui::search_input("schema-filter", &self.filter, cx);
        // Only the visible rows lay out, so a schema of 10,000 relations stays cheap.
        let list = uniform_list(
            "connection-list",
            self.rows.len(),
            cx.processor(|this, range: Range<usize>, _, cx| this.render_rows(range, cx)),
        )
        .track_scroll(&self.scroll)
        .test_support()
        .size_full();
        // Rows take a drag first, so the body only sees drops in gaps and below the rows.
        let body = div()
            .id("explorer-body")
            .test_support()
            .flex_1()
            .min_h_0()
            .on_drop(cx.listener(|_, d: &DraggedItem, _, cx| {
                cx.emit(SidebarEvent::Move {
                    item: d.0.clone(),
                    to: None,
                });
            }))
            // The menu does not stop propagation, so it is one menu on the body: capture clears
            // the target, a row's bubble handler sets it, and the deferred builder reads it.
            .capture_any_mouse_down(cx.listener(|this, _, _, _| this.menu_target = None))
            .child(list)
            .context_menu({
                let weak = cx.entity().downgrade();
                move |menu, _, cx| {
                    // The menu counts as open from its build until it is dismissed.
                    let _ = weak.update(cx, |s, cx| {
                        s.menu_open = true;
                        cx.notify();
                    });
                    let closed = weak.clone();
                    cx.subscribe_self(move |_, _: &DismissEvent, cx| {
                        let _ = closed.update(cx, |s, cx| {
                            s.menu_open = false;
                            cx.notify();
                        });
                    })
                    .detach();
                    explorer_menu(menu, &weak, cx)
                }
            });
        div()
            .id("schema-tree")
            .test_support()
            .track_focus(&self.focus)
            .key_context("Explorer")
            .on_action(cx.listener(Self::open_selected))
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::expand_selected))
            .on_action(cx.listener(Self::collapse_selected))
            .size_full()
            .flex()
            .flex_col()
            .bg(chrome)
            .text_color(fg)
            .child(header)
            .child(filter)
            .child(body)
    }
}

/// The chip under the pointer while a folder or connection is dragged, named like its row.
struct DragPreview(WorkspaceItem);

impl Render for DragPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = match &self.0 {
            WorkspaceItem::Folder(path) => path.name().to_owned(),
            WorkspaceItem::Connection(name) => name.clone(),
            _ => String::new(),
        };
        let theme = cx.theme();
        div()
            .id("drag-preview")
            .aria_label(name.clone())
            .test_support()
            .h(settings::active_metrics(cx).tree_row)
            .px_2()
            .flex()
            .items_center()
            .max_w(px(240.))
            .truncate()
            .text_size(px(12.))
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .border_1()
            .border_color(theme.drag_border)
            .rounded(theme.radius)
            .shadow_md()
            .opacity(0.9)
            .child(name)
    }
}

/// The glyph an explorer menu item leads with, by its id; items without one give `None`.
pub fn menu_icon(id: &str) -> Option<IconName> {
    Some(match id {
        "menu-new-connection" => Icons::Plus,
        "menu-new-folder" => Icons::FolderPlus,
        "menu-reload" => Icons::FolderSync,
        "menu-rename" => Icons::Pencil,
        "menu-edit" => Icons::Settings,
        "menu-new-query" => Icons::FileCode,
        "menu-refresh-schema" => Icons::RotateCw,
        "menu-disconnect" => Icons::CircleStop,
        "menu-duplicate" => Icons::CopyPlus,
        "menu-delete" => Icons::Trash,
        "tree-menu-open" => Icons::Table2,
        "tree-menu-new-query" | "tree-menu-generate-select" | "tree-menu-generate-insert" => {
            Icons::FileCode
        }
        "tree-menu-count" => Icons::Rows3,
        "tree-menu-copy-name" | "tree-menu-copy-qualified" | "tree-menu-copy-error" => Icons::Copy,
        "tree-menu-refresh" | "tree-menu-retry" => Icons::RotateCw,
        "tree-menu-view-definition" => Icons::FileText,
        _ => return None,
    })
}

// The explorer's context menu for the right-clicked row, or for the empty space below the rows.
fn explorer_menu(menu: PopupMenu, weak: &WeakEntity<Sidebar>, cx: &mut App) -> PopupMenu {
    let Some(sidebar) = weak.upgrade() else {
        return menu;
    };
    let (target, ws) = {
        let s = sidebar.read(cx);
        (s.menu_target.clone(), s.workspace.clone())
    };
    let emit = |event: SidebarEvent| {
        let weak = weak.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            let _ = weak.update(cx, |_, cx| cx.emit(event.clone()));
        }
    };
    if let Some(ExplorerTarget::Catalog(t)) = target {
        let Some(conn) = ws.connection(&t.connection) else {
            return menu;
        };
        return catalog_menu(menu, *t, conn, weak, cx);
    }
    // Every row leads with its glyph; only New connection and Reload carry a key chip.
    let label =
        |id: &'static str, text: &'static str| ui::menu_item(id, menu_icon(id), text, None, cx);
    let new_connection = ui::menu_item(
        "menu-new-connection",
        menu_icon("menu-new-connection"),
        "New connection",
        keys_for(&NewConnection, cx),
        cx,
    )
    .on_click(emit(SidebarEvent::NewConnection));
    let menu = menu.min_w(px(180.));
    // A connection's own actions lead its menu, so New connection moves to the end there.
    if let Some(ExplorerTarget::Workspace(WorkspaceItem::Connection(name))) = target {
        let Some(conn) = ws.connection(&name) else {
            return menu.item(new_connection);
        };
        let item = WorkspaceItem::Connection(name.clone());
        let parent = conn.folder().cloned();
        let new_query = SidebarEvent::NewQuery {
            connection: name.clone(),
            sql: String::new(),
        };
        let refresh = name.clone();
        return menu
            .item(label("menu-new-query", "New query").on_click(emit(new_query)))
            .item(
                label("menu-refresh-schema", "Refresh schema")
                    .on_click(on_sidebar(weak, move |s, cx| {
                        s.refresh_connection(&refresh, cx)
                    })),
            )
            .item(
                label("menu-disconnect", "Disconnect")
                    .on_click(emit(SidebarEvent::Disconnect(name.clone()))),
            )
            .separator()
            .item(
                label("menu-edit", "Edit connection")
                    .on_click(emit(SidebarEvent::Edit(name.clone()))),
            )
            .item(
                label("menu-duplicate", "Duplicate")
                    .on_click(emit(SidebarEvent::Duplicate(name.clone()))),
            )
            .item(label("menu-rename", "Rename").on_click(emit(SidebarEvent::Rename(item.clone()))))
            .item(swatches(item.clone(), conn.color(), weak.clone()))
            .separator()
            .item(label("menu-delete", "Delete").on_click(emit(SidebarEvent::Delete(item))))
            .separator()
            .item(new_connection)
            .item(
                label("menu-new-folder", "New folder")
                    .on_click(emit(SidebarEvent::NewFolder { parent })),
            );
    }
    let menu = menu.item(new_connection);
    match target {
        Some(ExplorerTarget::Workspace(WorkspaceItem::Folder(path))) => {
            let item = WorkspaceItem::Folder(path.clone());
            let own = ws
                .folders()
                .iter()
                .find(|f| f.path() == &path)
                .and_then(|f| f.color());
            menu.item(label("menu-new-folder", "New folder").on_click(emit(
                SidebarEvent::NewFolder {
                    parent: Some(path.clone()),
                },
            )))
            .separator()
            .item(label("menu-rename", "Rename").on_click(emit(SidebarEvent::Rename(item.clone()))))
            .item(swatches(item.clone(), own, weak.clone()))
            .separator()
            .item(label("menu-delete", "Delete").on_click(emit(SidebarEvent::Delete(item))))
        }
        _ => menu
            .item(
                label("menu-new-folder", "New folder")
                    .on_click(emit(SidebarEvent::NewFolder { parent: None })),
            )
            .separator()
            .item(
                ui::menu_item(
                    "menu-reload",
                    menu_icon("menu-reload"),
                    "Reload dbento.toml",
                    keys_for(&ReloadWorkspace, cx),
                    cx,
                )
                .on_click(emit(SidebarEvent::Reload)),
            ),
    }
}

/// What a right-click landed on: a workspace row, or a catalog row with what its menu needs.
#[derive(Debug, Clone)]
enum ExplorerTarget {
    Workspace(WorkspaceItem),
    Catalog(Box<CatalogTarget>),
}

// A catalog row as it was right-clicked, so a rebuild before the menu opens cannot retarget it.
#[derive(Debug, Clone)]
struct CatalogTarget {
    connection: String,
    node: NodeKey,
    kind: RowKind,
    label: String,
}

// A catalog row's menu: only the items `catalog_entries` lists for its kind, each wired to the
// sidebar or the clipboard. A column's relation is its node; its name is its label. `config`
// is the row's connection, for quoting and the select text.
fn catalog_menu(
    menu: PopupMenu,
    t: CatalogTarget,
    config: &ConnectionConfig,
    weak: &WeakEntity<Sidebar>,
    cx: &mut App,
) -> PopupMenu {
    let engine = config.engine();
    let rel = match &t.node {
        NodeKey::Relation { rel, .. } => Some(rel.clone()),
        _ => None,
    };
    let qualified = rel.as_ref().map(|rel| match t.kind {
        RowKind::Column { .. } => format!(
            "{}.{}",
            qualified_name(engine, rel),
            quote_ident(engine, &t.label)
        ),
        _ => qualified_name(engine, rel),
    });
    let copy = |text: String| {
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        }
    };
    let mut menu = menu.min_w(px(180.));
    for entry in catalog_entries(&t.kind, engine, cx) {
        let item = ui::menu_item(
            entry.id,
            menu_icon(entry.id),
            entry.label,
            entry.keys.clone(),
            cx,
        );
        let (conn, node) = (t.connection.clone(), t.node.clone());
        let item = match entry.id {
            "tree-menu-open" => {
                let (Some(rel), RowKind::Relation(kind)) = (rel.clone(), &t.kind) else {
                    continue;
                };
                let kind = *kind;
                item.on_click(on_sidebar(weak, move |s, cx| {
                    s.open_relation(&conn, &rel, kind, cx)
                }))
            }
            "tree-menu-new-query" => {
                let sql = rel
                    .as_ref()
                    .map_or_else(String::new, |rel| select_text(config, rel));
                item.on_click(on_sidebar(weak, move |_, cx| {
                    cx.emit(SidebarEvent::NewQuery {
                        connection: conn.clone(),
                        sql: sql.clone(),
                    })
                }))
            }
            "tree-menu-view-definition" => {
                let object = match (&t.kind, &rel) {
                    (RowKind::Object { object, .. }, _) => object.clone(),
                    (RowKind::Relation(_), Some(rel)) => ObjectRef::relation(rel.clone()),
                    _ => continue,
                };
                item.on_click(on_sidebar(weak, move |_, cx| {
                    cx.emit(SidebarEvent::ViewDefinition {
                        connection: conn.clone(),
                        object: object.clone(),
                    })
                }))
            }
            "tree-menu-copy-name" => item.on_click(copy(t.label.clone())),
            "tree-menu-copy-qualified" => match &qualified {
                Some(q) => item.on_click(copy(q.clone())),
                None => continue,
            },
            "tree-menu-generate-select" => match &rel {
                Some(rel) => item.on_click(copy(select_text(config, rel))),
                None => continue,
            },
            "tree-menu-generate-insert" => match rel.clone() {
                Some(rel) => item.on_click(on_sidebar(weak, move |s, cx| {
                    s.generate_insert(&conn, &rel, engine, cx)
                })),
                None => continue,
            },
            "tree-menu-count" => match rel.clone() {
                Some(rel) => {
                    item.on_click(on_sidebar(weak, move |s, cx| s.count_rows(&conn, &rel, cx)))
                }
                None => continue,
            },
            "tree-menu-refresh" => item.on_click(on_sidebar(weak, |s, cx| s.refresh(cx))),
            "tree-menu-retry" => {
                item.on_click(on_sidebar(weak, move |s, cx| s.retry(node.clone(), cx)))
            }
            "tree-menu-copy-error" => match &t.kind {
                RowKind::Failed { message } => item.on_click(copy(message.clone())),
                _ => continue,
            },
            _ => item,
        };
        menu = menu.item(item);
    }
    menu
}

// A menu click handler that runs `f` on the sidebar, if it still exists.
fn on_sidebar(
    weak: &WeakEntity<Sidebar>,
    f: impl Fn(&mut Sidebar, &mut Context<Sidebar>) + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let weak = weak.clone();
    move |_, _, cx| {
        let _ = weak.update(cx, |s, cx| f(s, cx));
    }
}

// One menu row of colour dots: "none" then each tag colour, with `own` ringed. A dot emits on
// its own click; the click then reaches the row, which has no handler and only dismisses.
fn swatches(item: WorkspaceItem, own: Option<Color>, weak: WeakEntity<Sidebar>) -> PopupMenuItem {
    PopupMenuItem::element(move |_, cx| {
        let theme = cx.theme();
        let (ring, muted) = (theme.foreground, theme.muted_foreground);
        let choices = std::iter::once(None).chain(MENU_COLORS.into_iter().map(Some));
        let dots = choices.enumerate().map(|(i, color)| {
            let (item, weak) = (item.clone(), weak.clone());
            let dot = div()
                .id(("menu-color", i))
                .aria_label(color.map_or("none", color_name))
                .test_support()
                .size(px(14.))
                .flex_none()
                .rounded_full()
                .border_2()
                .border_color(if color == own {
                    ring
                } else {
                    hsla(0., 0., 0., 0.)
                })
                .on_click(move |_, _, cx| {
                    let event = SidebarEvent::Recolor {
                        item: item.clone(),
                        color,
                    };
                    let _ = weak.update(cx, |_, cx| cx.emit(event));
                });
            match color {
                Some(c) => dot.bg(tag_color(c)),
                // "None" is a hollow dot with a muted edge.
                None => dot.child(
                    div()
                        .size_full()
                        .rounded_full()
                        .border_1()
                        .border_color(muted),
                ),
            }
        });
        h_flex().gap_1p5().py_0p5().children(dots)
    })
}
