//! The schema tree model: which catalog levels are expanded, loaded, failed or stale, and the
//! flat rows the sidebar draws. Pure data with no GPUI types, so it is tested directly.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use dbento_core::{
    ColumnMeta, ConnectionConfig, ConstraintInfo, ConstraintKind, Engine, FormatOptions, IndexInfo,
    ObjectRef, QueryLanguage, RELATION_CAP, RelationKind, RelationList, RelationObjects,
    RelationRef, RoutineKind, SchemaInfo, SchemaObjects, TriggerState, TypeKind, format_sql,
    quote_ident,
};
use gpui_kit::App;

use crate::catalog::{CatalogData, CatalogQuery, CatalogReply};
use crate::keymap::{OpenSelected, keys_for};
use crate::keys::{KeyFilter, KeyItem, KeyToken, display_key, key_levels};
use crate::labels::{MenuEntry, group_digits};
use crate::link::LinkState;
use crate::redis_completion::RedisDoc;

/// A group of schema objects under a relation (the first four) or a schema (the last two).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectGroup {
    /// Primary, unique, foreign, exclusion and other keys.
    Keys,
    /// Indexes, constraint-backed ones included.
    Indexes,
    /// Check constraints.
    Checks,
    /// Triggers.
    Triggers,
    /// Enum, domain, composite and range types.
    Types,
    /// Functions, procedures and aggregates.
    Routines,
}

/// A node of the tree that can expand.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NodeKey {
    /// A connection row.
    Connection(String),
    /// A schema of a connection.
    Schema {
        /// The connection name.
        conn: String,
        /// The schema name.
        schema: String,
    },
    /// A group of one relation kind inside a schema.
    Kind {
        /// The connection name.
        conn: String,
        /// The schema name.
        schema: String,
        /// The kind the group lists.
        kind: RelationKind,
    },
    /// A table, view or other relation.
    Relation {
        /// The connection name.
        conn: String,
        /// The relation.
        rel: RelationRef,
    },
    /// An object group under a relation; starts collapsed.
    RelationGroup {
        /// The connection name.
        conn: String,
        /// The relation.
        rel: RelationRef,
        /// The group.
        group: ObjectGroup,
    },
    /// An object group under a schema; starts collapsed.
    SchemaGroup {
        /// The connection name.
        conn: String,
        /// The schema name.
        schema: String,
        /// The group.
        group: ObjectGroup,
    },
    /// The objects level of a `Relation` or `Schema` key: its error and truncated rows.
    Objects(Box<NodeKey>),
    /// A `:` folder of Redis keys; opens locally and starts collapsed.
    KeyFolder {
        /// The connection name.
        conn: String,
        /// The DB index.
        db: u32,
        /// The folder's full prefix, ending in `:`.
        prefix: Vec<u8>,
    },
}

/// What a row shows.
#[derive(Debug, Clone, PartialEq)]
pub enum RowKind {
    /// A schema.
    Schema,
    /// A kind group such as "tables".
    Kind(RelationKind),
    /// A relation of the given kind.
    Relation(RelationKind),
    /// A column.
    Column {
        /// The declared type, as the engine reports it.
        type_name: String,
        /// Whether the column is part of the primary key.
        primary_key: bool,
        /// Whether the column was inferred from sampled rows rather than declared.
        inferred: bool,
    },
    /// An object group such as "indexes".
    Group(ObjectGroup),
    /// One schema object.
    Object {
        /// The group it is listed in.
        group: ObjectGroup,
        /// What a definition load asks for.
        object: ObjectRef,
        /// The dim trailing text: index method, constraint kind, type kind, routine result.
        detail: String,
    },
    /// A level that is still loading.
    Loading,
    /// A level whose load failed.
    Failed {
        /// The error to show.
        message: String,
    },
    /// The relation list hit its cap.
    Truncated,
    /// A level that loaded with nothing in it.
    Empty,
    /// A `:` folder of Redis keys.
    KeyFolder {
        /// The folder's full prefix, ending in `:`.
        prefix: Vec<u8>,
        /// How many loaded keys sit below it.
        keys: usize,
    },
    /// One Redis key.
    Key(KeyItem),
    /// Asks for the next page of a key listing.
    LoadMore,
}

/// One visible row of the tree.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogRow {
    /// What the row shows.
    pub kind: RowKind,
    /// The node the row toggles; a column's or status row's is the node it sits under.
    pub key: NodeKey,
    /// Indent level, counted from the `depth` given to [`CatalogTree::rows`].
    pub depth: usize,
    /// Whether the row's node is expanded.
    pub expanded: bool,
    /// The text to draw.
    pub label: String,
    /// The dotted path; the row's aria label.
    pub aria: String,
}

/// A catalog load the caller must start.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadRequest {
    /// The connection to load from.
    pub conn: String,
    /// The generation to echo in the reply.
    pub generation: u64,
    /// The level to load.
    pub query: CatalogQuery,
}

/// How far [`CatalogTree::reveal`] got toward showing a relation's row.
#[derive(Debug, PartialEq)]
#[non_exhaustive]
pub enum RevealStep {
    /// A level on the path is still loading; the caller sends these loads and asks again.
    Waiting(Vec<LoadRequest>),
    /// Every ancestor is open, so the relation has a row unless the filter hides it.
    Shown,
    /// The relation, its schema, or a load on the way is gone or failed.
    Missing,
}

/// One cached level.
enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

/// One Redis DB's key listing: the pages loaded so far and the state of the next one.
struct KeyListing {
    items: Vec<KeyItem>,
    next: Option<KeyToken>,
    more: More,
}

/// The next page of a key listing.
enum More {
    Idle,
    Loading,
    Failed(String),
}

/// The DB index a key-browser schema row names, `db3` giving 3; None for any other name.
pub fn db_index(schema: &str) -> Option<u32> {
    let digits = schema.strip_prefix("db")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The name of the connection `key` belongs to.
pub(crate) fn node_connection(key: &NodeKey) -> &str {
    match key {
        NodeKey::Connection(conn)
        | NodeKey::Schema { conn, .. }
        | NodeKey::Kind { conn, .. }
        | NodeKey::Relation { conn, .. }
        | NodeKey::RelationGroup { conn, .. }
        | NodeKey::SchemaGroup { conn, .. }
        | NodeKey::KeyFolder { conn, .. } => conn,
        NodeKey::Objects(inner) => node_connection(inner),
    }
}

/// Expansion state, cached levels and the generation that guards against stale replies.
#[derive(Default)]
pub struct CatalogTree {
    expanded: HashSet<NodeKey>,
    collapsed_kinds: HashSet<NodeKey>,
    schemas: HashMap<String, Load<Vec<SchemaInfo>>>,
    // A parent connection's listed databases, its derived children.
    databases: HashMap<String, Load<Vec<String>>>,
    relations: HashMap<(String, String), Load<RelationList>>,
    columns: HashMap<(String, RelationRef), Load<Vec<ColumnMeta>>>,
    // Relations whose columns are the declared keys alone, with why sampling failed.
    sample_errors: HashMap<(String, RelationRef), String>,
    relation_objects: HashMap<(String, RelationRef), Load<RelationObjects>>,
    schema_objects: HashMap<(String, String), Load<SchemaObjects>>,
    // Connections whose DB rows list keys rather than relations, with each DB's filter and
    // listing; a refresh drops listings but keeps filters.
    key_browsers: HashSet<String>,
    key_filters: HashMap<(String, u32), KeyFilter>,
    keys: HashMap<(String, u32), Load<KeyListing>>,
    command_docs: HashMap<String, Load<Arc<[RedisDoc]>>>,
    generation: u64,
    // The oldest generation still current, for all connections and per refreshed one.
    floor: u64,
    floors: HashMap<String, u64>,
}

const KINDS: [RelationKind; 5] = [
    RelationKind::Table,
    RelationKind::View,
    RelationKind::MaterializedView,
    RelationKind::ForeignTable,
    RelationKind::Other,
];

/// One listed object: its label, its dim detail and what a definition load asks for.
type Entry = (String, String, ObjectRef);

/// One DB's loaded keys as [`CatalogTree::key_level`] walks them.
struct KeyWalk<'a> {
    w: &'a Walk<'a>,
    db: u32,
    items: &'a [KeyItem],
    names: &'a [&'a [u8]],
    /// The DB row's aria path, `conn.db0`.
    path: &'a str,
}

/// How a row fits the walk in [`CatalogTree::rows`].
struct Walk<'a> {
    conn: &'a str,
    /// The trimmed, lowercased filter; empty means no filtering.
    filter: &'a str,
}

impl Walk<'_> {
    fn matches(&self, name: &str) -> bool {
        self.filter.is_empty() || name.to_lowercase().contains(self.filter)
    }
}

impl CatalogTree {
    /// An empty tree at generation 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// The generation new requests carry; it moves on every refresh.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether a reply for `conn` at `generation` is from after that connection's last
    /// refresh; older replies are ignored.
    pub fn is_current(&self, conn: &str, generation: u64) -> bool {
        generation >= self.floor.max(self.floors.get(conn).copied().unwrap_or(0))
    }

    /// Whether `key` is expanded. Kind groups start expanded.
    pub fn is_expanded(&self, key: &NodeKey) -> bool {
        match key {
            NodeKey::Kind { .. } => !self.collapsed_kinds.contains(key),
            NodeKey::Connection(_)
            | NodeKey::Schema { .. }
            | NodeKey::Relation { .. }
            | NodeKey::RelationGroup { .. }
            | NodeKey::SchemaGroup { .. }
            | NodeKey::Objects(_)
            | NodeKey::KeyFolder { .. } => self.expanded.contains(key),
        }
    }

    /// Flips `key`; expanding a level that is absent or failed marks it loading and returns its load.
    pub fn toggle(&mut self, key: &NodeKey) -> Option<LoadRequest> {
        if matches!(key, NodeKey::Kind { .. }) {
            if !self.collapsed_kinds.remove(key) {
                self.collapsed_kinds.insert(key.clone());
            }
            return None;
        }
        // A group opens locally, its objects already loaded with its parent; an objects key
        // only carries status rows, so it never opens.
        match key {
            NodeKey::RelationGroup { .. }
            | NodeKey::SchemaGroup { .. }
            | NodeKey::KeyFolder { .. } => {
                if !self.expanded.remove(key) {
                    self.expanded.insert(key.clone());
                }
                return None;
            }
            NodeKey::Objects(_) => return None,
            NodeKey::Connection(_)
            | NodeKey::Schema { .. }
            | NodeKey::Kind { .. }
            | NodeKey::Relation { .. } => {}
        }
        if self.expanded.remove(key) {
            return None;
        }
        self.expanded.insert(key.clone());
        self.drop_failed_objects(key);
        let absent_or_failed = match key {
            NodeKey::Connection(c) => {
                !matches!(self.schemas.get(c), Some(Load::Loading | Load::Ready(_)))
            }
            NodeKey::Schema { conn, schema } => match self.key_db(conn, schema) {
                Some(db) => !matches!(
                    self.keys.get(&(conn.clone(), db)),
                    Some(Load::Loading | Load::Ready(_))
                ),
                None => !matches!(
                    self.relations.get(&(conn.clone(), schema.clone())),
                    Some(Load::Loading | Load::Ready(_))
                ),
            },
            NodeKey::Relation { conn, rel } => !matches!(
                self.columns.get(&(conn.clone(), rel.clone())),
                Some(Load::Loading | Load::Ready(_))
            ),
            NodeKey::Kind { .. }
            | NodeKey::RelationGroup { .. }
            | NodeKey::SchemaGroup { .. }
            | NodeKey::Objects(_)
            | NodeKey::KeyFolder { .. } => false,
        };
        if absent_or_failed {
            self.start(key)
        } else {
            None
        }
    }

    /// Opens `key` if it is collapsed, like [`Self::toggle`]; never collapses an open node.
    pub fn expand(&mut self, key: &NodeKey) -> Option<LoadRequest> {
        if self.is_expanded(key) {
            None
        } else {
            self.toggle(key)
        }
    }

    /// Opens the path down to `rel` as far as its loaded levels allow: the connection, the
    /// schema once the schemas are in, the relation's kind group once the relations are in.
    pub fn reveal(&mut self, conn: &str, rel: &RelationRef) -> RevealStep {
        let loads: Vec<_> = self
            .expand(&NodeKey::Connection(conn.to_owned()))
            .into_iter()
            .collect();
        let schemas = match self.schemas.get(conn) {
            Some(Load::Ready(s)) => s,
            Some(Load::Failed(_)) => return RevealStep::Missing,
            None | Some(Load::Loading) => return RevealStep::Waiting(loads),
        };
        if !schemas.iter().any(|s| s.name() == rel.schema()) {
            return RevealStep::Missing;
        }
        let skey = NodeKey::Schema {
            conn: conn.to_owned(),
            schema: rel.schema().to_owned(),
        };
        let loads: Vec<_> = self.expand(&skey).into_iter().collect();
        let list = match self
            .relations
            .get(&(conn.to_owned(), rel.schema().to_owned()))
        {
            Some(Load::Ready(l)) => l,
            Some(Load::Failed(_)) => return RevealStep::Missing,
            None | Some(Load::Loading) => return RevealStep::Waiting(loads),
        };
        let Some(found) = list.items().iter().find(|r| r.name() == rel.name()) else {
            return RevealStep::Missing;
        };
        let kind = NodeKey::Kind {
            conn: conn.to_owned(),
            schema: rel.schema().to_owned(),
            kind: found.kind(),
        };
        self.expand(&kind);
        RevealStep::Shown
    }

    /// Reloads a failed level under `key`; None unless it failed.
    pub fn retry(&mut self, key: &NodeKey) -> Option<LoadRequest> {
        // A key listing fails either its first page, which starts over, or a later one,
        // which is asked again from the cursor it kept.
        if let NodeKey::Schema { conn, schema } = key
            && let Some(db) = self.key_db(conn, schema)
        {
            return match self.keys.get(&(conn.clone(), db)) {
                Some(Load::Failed(_)) => self.start(key),
                Some(Load::Ready(l)) if matches!(l.more, More::Failed(_)) => {
                    self.load_more_keys(conn, db)
                }
                _ => None,
            };
        }
        let failed = match key {
            NodeKey::Connection(c) => matches!(self.schemas.get(c), Some(Load::Failed(_))),
            NodeKey::Schema { conn, schema } => matches!(
                self.relations.get(&(conn.clone(), schema.clone())),
                Some(Load::Failed(_))
            ),
            NodeKey::Relation { conn, rel } => {
                let k = (conn.clone(), rel.clone());
                matches!(self.columns.get(&k), Some(Load::Failed(_)))
                    || self.sample_errors.contains_key(&k)
            }
            NodeKey::Objects(inner) => {
                return match inner.as_ref() {
                    NodeKey::Relation { conn, rel }
                        if matches!(
                            self.relation_objects.get(&(conn.clone(), rel.clone())),
                            Some(Load::Failed(_))
                        ) =>
                    {
                        Some(self.begin(&conn.clone(), relation_objects_query(rel)))
                    }
                    NodeKey::Schema { conn, schema }
                        if matches!(
                            self.schema_objects.get(&(conn.clone(), schema.clone())),
                            Some(Load::Failed(_))
                        ) =>
                    {
                        Some(self.begin(&conn.clone(), schema_objects_query(schema)))
                    }
                    NodeKey::Connection(_)
                    | NodeKey::Schema { .. }
                    | NodeKey::Kind { .. }
                    | NodeKey::Relation { .. }
                    | NodeKey::RelationGroup { .. }
                    | NodeKey::SchemaGroup { .. }
                    | NodeKey::Objects(_)
                    | NodeKey::KeyFolder { .. } => None,
                };
            }
            NodeKey::Kind { .. }
            | NodeKey::RelationGroup { .. }
            | NodeKey::SchemaGroup { .. }
            | NodeKey::KeyFolder { .. } => false,
        };
        if failed { self.start(key) } else { None }
    }

    /// Forgets a failed objects level of the relation or schema `key`, so the next
    /// [`Self::objects_to_load`] restarts it; a loading or loaded level is kept.
    fn drop_failed_objects(&mut self, key: &NodeKey) {
        match key {
            NodeKey::Relation { conn, rel } => {
                let k = (conn.clone(), rel.clone());
                if matches!(self.relation_objects.get(&k), Some(Load::Failed(_))) {
                    self.relation_objects.remove(&k);
                }
            }
            NodeKey::Schema { conn, schema } => {
                let k = (conn.clone(), schema.clone());
                if matches!(self.schema_objects.get(&k), Some(Load::Failed(_))) {
                    self.schema_objects.remove(&k);
                }
            }
            NodeKey::Connection(_)
            | NodeKey::Kind { .. }
            | NodeKey::RelationGroup { .. }
            | NodeKey::SchemaGroup { .. }
            | NodeKey::Objects(_)
            | NodeKey::KeyFolder { .. } => {}
        }
    }

    /// Marks loading and returns the objects load of every expanded relation and schema
    /// whose objects are absent.
    pub fn objects_to_load(&mut self) -> Vec<LoadRequest> {
        let queries: Vec<(String, CatalogQuery)> = self
            .expanded
            .iter()
            .filter_map(|k| match k {
                NodeKey::Relation { conn, rel } => {
                    Some((conn.clone(), relation_objects_query(rel)))
                }
                // A Redis DB holds keys, never types or routines.
                NodeKey::Schema { conn, schema } if self.key_db(conn, schema).is_none() => {
                    Some((conn.clone(), schema_objects_query(schema)))
                }
                NodeKey::Connection(_)
                | NodeKey::Schema { .. }
                | NodeKey::Kind { .. }
                | NodeKey::RelationGroup { .. }
                | NodeKey::SchemaGroup { .. }
                | NodeKey::Objects(_)
                | NodeKey::KeyFolder { .. } => None,
            })
            .collect();
        queries
            .into_iter()
            .filter_map(|(conn, q)| self.ensure(&conn, &q))
            .collect()
    }

    /// The number of objects a group row lists; 0 for any other key.
    pub fn group_count(&self, key: &NodeKey) -> usize {
        match key {
            NodeKey::RelationGroup { conn, rel, group } => {
                let Some(o) = self.relation_objects(conn, rel) else {
                    return 0;
                };
                let checks = o
                    .constraints()
                    .iter()
                    .filter(|c| matches!(c.kind(), ConstraintKind::Check))
                    .count();
                match group {
                    ObjectGroup::Keys => o.constraints().len() - checks,
                    ObjectGroup::Indexes => o.indexes().len(),
                    ObjectGroup::Checks => checks,
                    ObjectGroup::Triggers => o.triggers().len(),
                    ObjectGroup::Types | ObjectGroup::Routines => 0,
                }
            }
            NodeKey::SchemaGroup {
                conn,
                schema,
                group,
            } => {
                let Some(o) = self.schema_objects(conn, schema) else {
                    return 0;
                };
                match group {
                    ObjectGroup::Types => o.types().len(),
                    ObjectGroup::Routines => o.routines().len(),
                    ObjectGroup::Keys
                    | ObjectGroup::Indexes
                    | ObjectGroup::Checks
                    | ObjectGroup::Triggers => 0,
                }
            }
            NodeKey::Connection(_)
            | NodeKey::Schema { .. }
            | NodeKey::Kind { .. }
            | NodeKey::Relation { .. }
            | NodeKey::Objects(_)
            | NodeKey::KeyFolder { .. } => 0,
        }
    }

    /// Marks the level under `key` loading and builds its request at the current generation.
    fn start(&mut self, key: &NodeKey) -> Option<LoadRequest> {
        let (conn, query) = match key {
            NodeKey::Connection(c) => (c, CatalogQuery::Schemas),
            NodeKey::Schema { conn, schema } => match self.key_db(conn, schema) {
                Some(db) => (
                    conn,
                    CatalogQuery::Keys {
                        db,
                        filter: self.key_filter(conn, db),
                        after: None,
                    },
                ),
                None => (
                    conn,
                    CatalogQuery::Relations {
                        schema: schema.clone(),
                    },
                ),
            },
            NodeKey::Relation { conn, rel } => (conn, CatalogQuery::Columns { rel: rel.clone() }),
            NodeKey::Kind { .. }
            | NodeKey::RelationGroup { .. }
            | NodeKey::SchemaGroup { .. }
            | NodeKey::Objects(_)
            | NodeKey::KeyFolder { .. } => return None,
        };
        Some(self.begin(&conn.clone(), query))
    }

    /// Marks `query`'s level of `conn` loading and builds its request at the current generation.
    fn begin(&mut self, conn: &str, query: CatalogQuery) -> LoadRequest {
        let c = conn.to_owned();
        match &query {
            CatalogQuery::Schemas => {
                self.schemas.insert(c, Load::Loading);
            }
            CatalogQuery::Databases => {
                self.databases.insert(c, Load::Loading);
            }
            CatalogQuery::Relations { schema } => {
                self.relations.insert((c, schema.clone()), Load::Loading);
            }
            CatalogQuery::Columns { rel } => {
                self.columns.insert((c, rel.clone()), Load::Loading);
            }
            CatalogQuery::RelationObjects { rel } => {
                self.relation_objects
                    .insert((c, rel.clone()), Load::Loading);
            }
            CatalogQuery::SchemaObjects { schema } => {
                self.schema_objects
                    .insert((c, schema.clone()), Load::Loading);
            }
            CatalogQuery::Keys {
                db, after: None, ..
            } => {
                self.keys.insert((c, *db), Load::Loading);
            }
            CatalogQuery::CommandDocs => {
                self.command_docs.insert(c, Load::Loading);
            }
            // Counts and definitions are no tree level, and a later key page is tracked by
            // its listing.
            CatalogQuery::Count { .. }
            | CatalogQuery::Definition { .. }
            | CatalogQuery::Keys { .. } => {}
        }
        LoadRequest {
            conn: conn.to_owned(),
            generation: self.generation,
            query,
        }
    }

    /// Marks `query`'s level loading and returns its request when it is absent; None when it
    /// is loading, loaded or failed. Never expands a node.
    pub fn ensure(&mut self, conn: &str, query: &CatalogQuery) -> Option<LoadRequest> {
        let c = conn.to_owned();
        let present = match query {
            CatalogQuery::Schemas => self.schemas.contains_key(&c),
            CatalogQuery::Databases => self.databases.contains_key(&c),
            CatalogQuery::CommandDocs => self.command_docs.contains_key(&c),
            CatalogQuery::Relations { schema } => self.relations.contains_key(&(c, schema.clone())),
            CatalogQuery::Columns { rel } => self.columns.contains_key(&(c, rel.clone())),
            CatalogQuery::RelationObjects { rel } => {
                self.relation_objects.contains_key(&(c, rel.clone()))
            }
            CatalogQuery::SchemaObjects { schema } => {
                self.schema_objects.contains_key(&(c, schema.clone()))
            }
            // A count or definition is no level of the tree, and keys load only from their
            // DB row, so there is nothing to ensure.
            CatalogQuery::Count { .. }
            | CatalogQuery::Definition { .. }
            | CatalogQuery::Keys { .. } => true,
        };
        (!present).then(|| self.begin(conn, query.clone()))
    }

    /// Stores a reply for `conn`; one from an older generation is ignored.
    pub fn apply(&mut self, conn: &str, reply: CatalogReply) {
        if !self.is_current(conn, reply.generation) {
            return;
        }
        let conn = conn.to_owned();
        let result = reply.result;
        // Data that does not fit its query fails that level rather than being dropped.
        match reply.query {
            CatalogQuery::Schemas => {
                let load = load_of(result, |d| match d {
                    CatalogData::Schemas(s) => Some(s),
                    CatalogData::Databases(_)
                    | CatalogData::Relations(_)
                    | CatalogData::Columns(_)
                    | CatalogData::Count(_)
                    | CatalogData::RelationObjects(_)
                    | CatalogData::SchemaObjects(_)
                    | CatalogData::Definition(_)
                    | CatalogData::Keys(_)
                    | CatalogData::CommandDocs(_)
                    | CatalogData::KeyColumns { .. } => None,
                });
                self.schemas.insert(conn, load);
            }
            CatalogQuery::Databases => {
                let load = load_of(result, |d| match d {
                    CatalogData::Databases(names) => Some(names),
                    CatalogData::Schemas(_)
                    | CatalogData::Relations(_)
                    | CatalogData::Columns(_)
                    | CatalogData::Count(_)
                    | CatalogData::RelationObjects(_)
                    | CatalogData::SchemaObjects(_)
                    | CatalogData::Definition(_)
                    | CatalogData::Keys(_)
                    | CatalogData::CommandDocs(_)
                    | CatalogData::KeyColumns { .. } => None,
                });
                self.databases.insert(conn, load);
            }
            CatalogQuery::Relations { schema } => {
                let load = load_of(result, |d| match d {
                    CatalogData::Relations(r) => Some(r),
                    CatalogData::Schemas(_)
                    | CatalogData::Databases(_)
                    | CatalogData::Columns(_)
                    | CatalogData::Count(_)
                    | CatalogData::RelationObjects(_)
                    | CatalogData::SchemaObjects(_)
                    | CatalogData::Definition(_)
                    | CatalogData::Keys(_)
                    | CatalogData::CommandDocs(_)
                    | CatalogData::KeyColumns { .. } => None,
                });
                self.relations.insert((conn, schema), load);
            }
            CatalogQuery::Columns { rel } => {
                // Key columns load as the level and keep why the rest are missing; a full
                // answer or a failure drops that reason.
                let mut reason = None;
                let load = load_of(result, |d| match d {
                    CatalogData::Columns(c) => Some(c),
                    CatalogData::KeyColumns { columns, reason: r } => {
                        reason = Some(r);
                        Some(columns)
                    }
                    CatalogData::Schemas(_)
                    | CatalogData::Databases(_)
                    | CatalogData::Relations(_)
                    | CatalogData::Count(_)
                    | CatalogData::RelationObjects(_)
                    | CatalogData::SchemaObjects(_)
                    | CatalogData::Definition(_)
                    | CatalogData::Keys(_)
                    | CatalogData::CommandDocs(_) => None,
                });
                let k = (conn, rel);
                match reason {
                    Some(r) => self.sample_errors.insert(k.clone(), r),
                    None => self.sample_errors.remove(&k),
                };
                self.columns.insert(k, load);
            }
            CatalogQuery::RelationObjects { rel } => {
                let load = load_of(result, |d| match d {
                    CatalogData::RelationObjects(o) => Some(o),
                    CatalogData::Schemas(_)
                    | CatalogData::Databases(_)
                    | CatalogData::Relations(_)
                    | CatalogData::Columns(_)
                    | CatalogData::Count(_)
                    | CatalogData::SchemaObjects(_)
                    | CatalogData::Definition(_)
                    | CatalogData::Keys(_)
                    | CatalogData::CommandDocs(_)
                    | CatalogData::KeyColumns { .. } => None,
                });
                self.relation_objects.insert((conn, rel), load);
            }
            CatalogQuery::SchemaObjects { schema } => {
                let load = load_of(result, |d| match d {
                    CatalogData::SchemaObjects(o) => Some(o),
                    CatalogData::Schemas(_)
                    | CatalogData::Databases(_)
                    | CatalogData::Relations(_)
                    | CatalogData::Columns(_)
                    | CatalogData::Count(_)
                    | CatalogData::RelationObjects(_)
                    | CatalogData::Definition(_)
                    | CatalogData::Keys(_)
                    | CatalogData::CommandDocs(_)
                    | CatalogData::KeyColumns { .. } => None,
                });
                self.schema_objects.insert((conn, schema), load);
            }
            CatalogQuery::Keys { db, filter, after } => {
                self.apply_keys((conn, db), &filter, after, result);
            }
            CatalogQuery::CommandDocs => {
                let load = load_of(result, |d| match d {
                    CatalogData::CommandDocs(d) => Some(d),
                    CatalogData::Schemas(_)
                    | CatalogData::Databases(_)
                    | CatalogData::Relations(_)
                    | CatalogData::Columns(_)
                    | CatalogData::Count(_)
                    | CatalogData::RelationObjects(_)
                    | CatalogData::SchemaObjects(_)
                    | CatalogData::Definition(_)
                    | CatalogData::Keys(_)
                    | CatalogData::KeyColumns { .. } => None,
                });
                self.command_docs.insert(conn, load);
            }
            // Counts and definitions are no tree level.
            CatalogQuery::Count { .. } | CatalogQuery::Definition { .. } => {}
        }
    }

    /// Stores a key page: a first page replaces the listing, a later one appends only while
    /// the listing still waits on that very cursor. A page for another filter than the DB's
    /// current one is stale and dropped.
    fn apply_keys(
        &mut self,
        k: (String, u32),
        filter: &KeyFilter,
        after: Option<KeyToken>,
        result: Result<CatalogData, String>,
    ) {
        if *filter != self.key_filter(&k.0, k.1) {
            return;
        }
        let Some(after) = after else {
            let load = load_of(result, |d| match d {
                CatalogData::Keys(b) => Some(KeyListing {
                    items: b.items,
                    next: b.next,
                    more: More::Idle,
                }),
                CatalogData::Schemas(_)
                | CatalogData::Databases(_)
                | CatalogData::Relations(_)
                | CatalogData::Columns(_)
                | CatalogData::Count(_)
                | CatalogData::RelationObjects(_)
                | CatalogData::SchemaObjects(_)
                | CatalogData::Definition(_)
                | CatalogData::CommandDocs(_)
                | CatalogData::KeyColumns { .. } => None,
            });
            self.keys.insert(k, load);
            return;
        };
        let Some(Load::Ready(l)) = self.keys.get_mut(&k) else {
            return;
        };
        if !matches!(l.more, More::Loading) || l.next.as_ref() != Some(&after) {
            return;
        }
        match result {
            Ok(CatalogData::Keys(b)) => {
                l.items.extend(b.items);
                l.next = b.next;
                l.more = More::Idle;
            }
            Ok(_) => l.more = More::Failed("unexpected catalog reply".into()),
            Err(m) => l.more = More::Failed(m),
        }
    }

    /// Marks `conn` as a key browser: its DB rows list keys instead of relations.
    pub fn set_key_browser(&mut self, conn: &str) {
        self.key_browsers.insert(conn.to_owned());
    }

    /// Unmarks every key browser, so a workspace install can mark its own afresh.
    pub fn clear_key_browsers(&mut self) {
        self.key_browsers.clear();
    }

    /// The DB index of `schema` when `conn` browses keys and the name is a DB row's.
    fn key_db(&self, conn: &str, schema: &str) -> Option<u32> {
        self.key_browsers.contains(conn).then(|| db_index(schema))?
    }

    /// The key filter of DB `db` of `conn`; the default lists every key.
    pub fn key_filter(&self, conn: &str, db: u32) -> KeyFilter {
        self.key_filters
            .get(&(conn.to_owned(), db))
            .cloned()
            .unwrap_or_default()
    }

    /// Sets DB `db`'s key filter and drops its listing; an open DB scans again from the
    /// first page and returns that load, a closed one keeps the filter for its next opening.
    pub fn filter_keys(&mut self, conn: &str, db: u32, filter: KeyFilter) -> Option<LoadRequest> {
        let k = (conn.to_owned(), db);
        self.key_filters.insert(k.clone(), filter);
        self.keys.remove(&k);
        let skey = NodeKey::Schema {
            conn: conn.to_owned(),
            schema: format!("db{db}"),
        };
        if self.key_browsers.contains(conn) && self.expanded.contains(&skey) {
            self.start(&skey)
        } else {
            None
        }
    }

    /// Asks for DB `db`'s next key page; None unless the listing is ready, has a next page
    /// and has none in flight, so a double click cannot scan one cursor twice.
    pub fn load_more_keys(&mut self, conn: &str, db: u32) -> Option<LoadRequest> {
        let filter = self.key_filter(conn, db);
        let generation = self.generation;
        let Some(Load::Ready(l)) = self.keys.get_mut(&(conn.to_owned(), db)) else {
            return None;
        };
        if matches!(l.more, More::Loading) {
            return None;
        }
        let after = l.next.clone()?;
        l.more = More::Loading;
        Some(LoadRequest {
            conn: conn.to_owned(),
            generation,
            query: CatalogQuery::Keys {
                db,
                filter,
                after: Some(after),
            },
        })
    }

    /// Drops every cache, bumps the generation and returns a load for each expanded level.
    pub fn refresh(&mut self) -> Vec<LoadRequest> {
        self.generation += 1;
        self.floor = self.generation;
        self.floors.clear();
        self.schemas.clear();
        self.databases.clear();
        self.relations.clear();
        self.columns.clear();
        self.sample_errors.clear();
        self.keys.clear();
        self.command_docs.clear();
        let rels = std::mem::take(&mut self.relation_objects)
            .into_keys()
            .collect();
        let schemas = std::mem::take(&mut self.schema_objects)
            .into_keys()
            .collect();
        let keys: Vec<NodeKey> = self.expanded.iter().cloned().collect();
        let mut loads: Vec<_> = keys.iter().filter_map(|k| self.start(k)).collect();
        loads.extend(self.restart_objects(rels, schemas));
        loads
    }

    /// Drops `conn`'s cached levels and returns a load for each of its expanded levels. Only
    /// `conn`'s loads in flight go stale; other connections' still land.
    pub fn refresh_connection(&mut self, conn: &str) -> Vec<LoadRequest> {
        self.generation += 1;
        self.floors.insert(conn.to_owned(), self.generation);
        self.schemas.remove(conn);
        self.databases.remove(conn);
        self.relations.retain(|(c, _), _| c != conn);
        self.columns.retain(|(c, _), _| c != conn);
        self.sample_errors.retain(|(c, _), _| c != conn);
        self.keys.retain(|(c, _), _| c != conn);
        self.command_docs.remove(conn);
        let rels = self
            .relation_objects
            .extract_if(|(c, _), _| c == conn)
            .map(|(k, _)| k)
            .collect();
        let schemas = self
            .schema_objects
            .extract_if(|(c, _), _| c == conn)
            .map(|(k, _)| k)
            .collect();
        let keys: Vec<NodeKey> = self
            .expanded
            .iter()
            .filter(|k| node_connection(k) == conn)
            .cloned()
            .collect();
        let mut loads: Vec<_> = keys.iter().filter_map(|k| self.start(k)).collect();
        loads.extend(self.restart_objects(rels, schemas));
        loads
    }

    /// Reloads the dropped objects levels whose relation or schema is still expanded; ones
    /// never asked stay absent for [`Self::objects_to_load`].
    fn restart_objects(
        &mut self,
        rels: Vec<(String, RelationRef)>,
        schemas: Vec<(String, String)>,
    ) -> Vec<LoadRequest> {
        let mut loads = Vec::new();
        for (conn, rel) in rels {
            let rkey = NodeKey::Relation {
                conn: conn.clone(),
                rel: rel.clone(),
            };
            if self.expanded.contains(&rkey) {
                loads.push(self.begin(&conn, relation_objects_query(&rel)));
            }
        }
        for (conn, schema) in schemas {
            let skey = NodeKey::Schema {
                conn: conn.clone(),
                schema: schema.clone(),
            };
            if self.expanded.contains(&skey) {
                loads.push(self.begin(&conn, schema_objects_query(&schema)));
            }
        }
        loads
    }

    /// Drops `conn`'s expansion and cached levels.
    pub fn forget(&mut self, conn: &str) {
        self.expanded.retain(|k| node_connection(k) != conn);
        self.collapsed_kinds.retain(|k| node_connection(k) != conn);
        self.schemas.remove(conn);
        self.databases.remove(conn);
        self.relations.retain(|(c, _), _| c != conn);
        self.columns.retain(|(c, _), _| c != conn);
        self.sample_errors.retain(|(c, _), _| c != conn);
        self.relation_objects.retain(|(c, _), _| c != conn);
        self.schema_objects.retain(|(c, _), _| c != conn);
        self.keys.retain(|(c, _), _| c != conn);
        self.key_filters.retain(|(c, _), _| c != conn);
        self.command_docs.remove(conn);
    }

    /// The schema level of `conn` as a link: absent Idle, loading Connecting, loaded
    /// Connected, failed Failed with its error.
    pub fn link(&self, conn: &str) -> LinkState {
        match self.schemas.get(conn) {
            None => LinkState::Idle,
            Some(Load::Loading) => LinkState::Connecting,
            Some(Load::Ready(_)) => LinkState::Connected,
            Some(Load::Failed(m)) => LinkState::Failed(m.clone()),
        }
    }

    /// Loaded COMMAND DOCS of Redis connection `conn`, for completion.
    pub fn command_docs(&self, conn: &str) -> Option<&[RedisDoc]> {
        match self.command_docs.get(conn) {
            Some(Load::Ready(d)) => Some(d),
            _ => None,
        }
    }

    /// Loaded database names of parent `conn`, in the server's order.
    pub fn databases(&self, conn: &str) -> Option<&[String]> {
        match self.databases.get(conn) {
            Some(Load::Ready(d)) => Some(d),
            _ => None,
        }
    }

    /// Loaded schemas of `conn`, for completion.
    pub fn schemas(&self, conn: &str) -> Option<&[SchemaInfo]> {
        match self.schemas.get(conn) {
            Some(Load::Ready(s)) => Some(s),
            _ => None,
        }
    }

    /// Loaded relations of `schema`, for completion.
    pub fn relations(&self, conn: &str, schema: &str) -> Option<&RelationList> {
        match self.relations.get(&(conn.to_owned(), schema.to_owned())) {
            Some(Load::Ready(r)) => Some(r),
            _ => None,
        }
    }

    /// Loaded columns of `rel`, for completion.
    pub fn columns(&self, conn: &str, rel: &RelationRef) -> Option<&[ColumnMeta]> {
        match self.columns.get(&(conn.to_owned(), rel.clone())) {
            Some(Load::Ready(c)) => Some(c),
            _ => None,
        }
    }

    /// Loaded objects of `rel`.
    pub fn relation_objects(&self, conn: &str, rel: &RelationRef) -> Option<&RelationObjects> {
        match self.relation_objects.get(&(conn.to_owned(), rel.clone())) {
            Some(Load::Ready(o)) => Some(o),
            _ => None,
        }
    }

    /// Loaded objects of `schema`.
    pub fn schema_objects(&self, conn: &str, schema: &str) -> Option<&SchemaObjects> {
        match self
            .schema_objects
            .get(&(conn.to_owned(), schema.to_owned()))
        {
            Some(Load::Ready(o)) => Some(o),
            _ => None,
        }
    }

    /// The rows under connection `conn` when it is expanded, schemas at `depth`.
    ///
    /// A non-empty `filter` (trimmed, case-insensitive) keeps rows whose name matches, the
    /// ancestors of visible matches and error rows, and everything below a match. Rows under a
    /// collapsed node are not searched.
    pub fn rows(&self, conn: &str, depth: usize, filter: &str) -> Vec<CatalogRow> {
        let ckey = NodeKey::Connection(conn.to_owned());
        if !self.expanded.contains(&ckey) {
            return Vec::new();
        }
        let filter = filter.trim().to_lowercase();
        let w = Walk {
            conn,
            filter: &filter,
        };
        let status = |load: Option<&Load<_>>| match load {
            None | Some(Load::Loading) => {
                Some((RowKind::Loading, "loading…".to_owned(), "loading"))
            }
            Some(Load::Failed(m)) => {
                Some((RowKind::Failed { message: m.clone() }, m.clone(), "error"))
            }
            Some(Load::Ready(_)) => None,
        };
        let mut out = Vec::new();
        if let Some((kind, label, tag)) = status(self.schemas.get(conn)) {
            out.push(row(
                kind,
                ckey,
                depth,
                false,
                label,
                format!("{conn}:{tag}"),
            ));
            return out;
        }
        let Some(Load::Ready(schemas)) = self.schemas.get(conn) else {
            return out;
        };
        if schemas.is_empty() {
            out.push(row(
                RowKind::Empty,
                ckey,
                depth,
                false,
                "no schemas".into(),
                format!("{conn}:empty"),
            ));
            return out;
        }
        for s in schemas {
            let skey = NodeKey::Schema {
                conn: conn.to_owned(),
                schema: s.name().to_owned(),
            };
            let matched = w.matches(s.name());
            let expanded = self.expanded.contains(&skey);
            let children = match self.key_db(conn, s.name()) {
                _ if !expanded => Vec::new(),
                Some(db) => self.key_rows(&w, db, s.name(), depth + 1, matched),
                None => self.schema_children(&w, s.name(), depth + 1, matched),
            };
            if matched || !children.is_empty() {
                let aria = format!("{conn}.{}", s.name());
                out.push(row(
                    RowKind::Schema,
                    skey,
                    depth,
                    expanded,
                    s.name().to_owned(),
                    aria,
                ));
                out.extend(children);
            }
        }
        out
    }

    /// Status rows, the `:` levels and the Load more row under one expanded Redis DB.
    fn key_rows(
        &self,
        w: &Walk,
        db: u32,
        schema: &str,
        depth: usize,
        all: bool,
    ) -> Vec<CatalogRow> {
        let skey = NodeKey::Schema {
            conn: w.conn.to_owned(),
            schema: schema.to_owned(),
        };
        let path = format!("{}.{schema}", w.conn);
        // As under a schema, only an error row shows whatever the filter, since Retry acts.
        let show_status = all || w.filter.is_empty();
        let status = |kind, label: String, tag: &str| {
            row(
                kind,
                skey.clone(),
                depth,
                false,
                label,
                format!("{path}:{tag}"),
            )
        };
        let failed =
            |m: &String| status(RowKind::Failed { message: m.clone() }, m.clone(), "error");
        let listing = match self.keys.get(&(w.conn.to_owned(), db)) {
            None | Some(Load::Loading) if show_status => {
                return vec![status(RowKind::Loading, "loading…".into(), "loading")];
            }
            None | Some(Load::Loading) => return Vec::new(),
            Some(Load::Failed(m)) => return vec![failed(m)],
            Some(Load::Ready(l)) => l,
        };
        let mut out = Vec::new();
        if listing.items.is_empty() && listing.next.is_none() && show_status {
            out.push(status(RowKind::Empty, "no keys".into(), "empty"));
        }
        let names: Vec<&[u8]> = listing.items.iter().map(|i| i.name.as_slice()).collect();
        let level = KeyWalk {
            w,
            db,
            items: &listing.items,
            names: &names,
            path: &path,
        };
        out.extend(self.key_level(&level, &skey, b"", depth, all));
        match &listing.more {
            More::Loading if show_status => {
                out.push(status(RowKind::Loading, "loading…".into(), "loading"));
            }
            More::Failed(m) => out.push(failed(m)),
            More::Idle if listing.next.is_some() && show_status => {
                out.push(status(RowKind::LoadMore, "Load more".into(), "more"));
            }
            More::Idle | More::Loading => {}
        }
        out
    }

    /// The folders, then the keys, directly under `prefix`, each open folder followed by its
    /// own level. A folder or key shows when it matches the filter or holds a match.
    fn key_level(
        &self,
        k: &KeyWalk,
        parent: &NodeKey,
        prefix: &[u8],
        depth: usize,
        all: bool,
    ) -> Vec<CatalogRow> {
        let level = key_levels(k.names, prefix);
        let mut out = Vec::new();
        for (folder, keys) in level.folders {
            let fkey = NodeKey::KeyFolder {
                conn: k.w.conn.to_owned(),
                db: k.db,
                prefix: folder.clone(),
            };
            let shown = display_key(&folder);
            let matched = all || k.w.matches(&shown);
            let expanded = self.expanded.contains(&fkey);
            let kids = if expanded {
                self.key_level(k, &fkey, &folder, depth + 1, matched)
            } else {
                Vec::new()
            };
            if matched || !kids.is_empty() {
                // "user:1:" under "user:" reads "1"; a bare ":" segment keeps its colon.
                let segment = &folder[prefix.len()..];
                let trimmed = &segment[..segment.len() - 1];
                let label = display_key(if trimmed.is_empty() { segment } else { trimmed });
                let kind = RowKind::KeyFolder {
                    prefix: folder,
                    keys,
                };
                let aria = format!("{}/{shown}", k.path);
                out.push(row(kind, fkey, depth, expanded, label, aria));
                out.extend(kids);
            }
        }
        for i in level.leaves {
            let item = &k.items[i];
            let full = display_key(&item.name);
            if !(all || k.w.matches(&full)) {
                continue;
            }
            let rest = &item.name[prefix.len()..];
            let label = if rest.is_empty() {
                full.clone()
            } else {
                display_key(rest)
            };
            let aria = format!("{}.{full}", k.path);
            out.push(row(
                RowKind::Key(item.clone()),
                parent.clone(),
                depth,
                false,
                label,
                aria,
            ));
        }
        out
    }

    /// Kind groups, relations and status rows under one expanded schema.
    fn schema_children(&self, w: &Walk, schema: &str, depth: usize, all: bool) -> Vec<CatalogRow> {
        let conn = w.conn;
        let skey = NodeKey::Schema {
            conn: conn.to_owned(),
            schema: schema.to_owned(),
        };
        let path = format!("{conn}.{schema}");
        // Loading, empty and truncated rows only show under a matching ancestor; an error row
        // always shows, since its Retry is actionable.
        let show_status = all || w.filter.is_empty();
        let list = match self.relations.get(&(conn.to_owned(), schema.to_owned())) {
            None | Some(Load::Loading) => {
                return if show_status {
                    vec![row(
                        RowKind::Loading,
                        skey,
                        depth,
                        false,
                        "loading…".into(),
                        format!("{path}:loading"),
                    )]
                } else {
                    Vec::new()
                };
            }
            Some(Load::Failed(m)) => {
                return vec![row(
                    RowKind::Failed { message: m.clone() },
                    skey,
                    depth,
                    false,
                    m.clone(),
                    format!("{path}:error"),
                )];
            }
            Some(Load::Ready(l)) => l,
        };
        let mut out = Vec::new();
        if list.items().is_empty() && show_status {
            out.push(row(
                RowKind::Empty,
                skey.clone(),
                depth,
                false,
                "no tables or views".into(),
                format!("{path}:empty"),
            ));
        }
        for k in KINDS {
            let items: Vec<_> = list.items().iter().filter(|r| r.kind() == k).collect();
            if items.is_empty() {
                continue;
            }
            let label = kind_label(k);
            let kkey = NodeKey::Kind {
                conn: conn.to_owned(),
                schema: schema.to_owned(),
                kind: k,
            };
            let expanded = self.is_expanded(&kkey);
            let mut kids = Vec::new();
            // The filter searches only what is visible, so a collapsed group surfaces nothing.
            for r in items.into_iter().filter(|_| expanded) {
                let rel = RelationRef::new(schema, r.name());
                let rkey = NodeKey::Relation {
                    conn: conn.to_owned(),
                    rel: rel.clone(),
                };
                let matched = all || w.matches(r.name());
                let rexp = self.expanded.contains(&rkey);
                let cols = if rexp {
                    self.column_rows(w, &rel, depth + 2, matched)
                } else {
                    Vec::new()
                };
                if matched || !cols.is_empty() {
                    let aria = format!("{path}.{}", r.name());
                    kids.push(row(
                        RowKind::Relation(k),
                        rkey,
                        depth + 1,
                        rexp,
                        r.name().to_owned(),
                        aria,
                    ));
                    kids.extend(cols);
                }
            }
            if all || !kids.is_empty() {
                out.push(row(
                    RowKind::Kind(k),
                    kkey,
                    depth,
                    expanded,
                    label.to_owned(),
                    format!("{path}/{label}"),
                ));
                out.extend(kids);
            }
        }
        if list.truncated() && show_status {
            out.push(row(
                RowKind::Truncated,
                skey,
                depth,
                false,
                "first 10,000 shown; filter to narrow".into(),
                format!("{path}:truncated"),
            ));
        }
        out.extend(self.schema_object_rows(w, schema, depth, all));
        out
    }

    /// A schema's types and routines groups and objects truncated row, or its error row.
    fn schema_object_rows(
        &self,
        w: &Walk,
        schema: &str,
        depth: usize,
        all: bool,
    ) -> Vec<CatalogRow> {
        let conn = w.conn;
        let path = format!("{conn}.{schema}");
        let okey = NodeKey::Objects(Box::new(NodeKey::Schema {
            conn: conn.to_owned(),
            schema: schema.to_owned(),
        }));
        let objects = match self
            .schema_objects
            .get(&(conn.to_owned(), schema.to_owned()))
        {
            None | Some(Load::Loading) => return Vec::new(),
            Some(Load::Failed(m)) => return vec![objects_error(okey, depth, m, &path)],
            Some(Load::Ready(o)) => o,
        };
        let mut out = Vec::new();
        for group in [ObjectGroup::Types, ObjectGroup::Routines] {
            let gkey = NodeKey::SchemaGroup {
                conn: conn.to_owned(),
                schema: schema.to_owned(),
                group,
            };
            out.extend(self.group_rows(w, gkey, &path, depth, all, || {
                schema_entries(objects, group)
            }));
        }
        if objects.truncated() && (all || w.filter.is_empty()) {
            let label = format!(
                "first {} of each shown; filter to narrow",
                group_digits(RELATION_CAP as u64)
            );
            out.push(row(
                RowKind::Truncated,
                okey,
                depth,
                false,
                label,
                format!("{path}/objects:truncated"),
            ));
        }
        out
    }

    /// Whether `rel`'s objects answered that the engine cannot read them.
    fn unreadable(&self, conn: &str, rel: &RelationRef) -> bool {
        matches!(
            self.relation_objects.get(&(conn.to_owned(), rel.clone())),
            Some(Load::Ready(o)) if o.unreadable_reason().is_some()
        )
    }

    /// The keys, indexes, checks and triggers groups of one relation, or its objects error row.
    fn relation_object_rows(
        &self,
        w: &Walk,
        rel: &RelationRef,
        depth: usize,
        all: bool,
    ) -> Vec<CatalogRow> {
        let conn = w.conn;
        let path = format!("{conn}.{}.{}", rel.schema(), rel.name());
        let okey = NodeKey::Objects(Box::new(NodeKey::Relation {
            conn: conn.to_owned(),
            rel: rel.clone(),
        }));
        let objects = match self.relation_objects.get(&(conn.to_owned(), rel.clone())) {
            None | Some(Load::Loading) => return Vec::new(),
            Some(Load::Failed(m)) => return vec![objects_error(okey, depth, m, &path)],
            Some(Load::Ready(o)) => o,
        };
        // An unreadable relation is no failure to retry: one status row says why.
        if let Some(why) = objects.unreadable_reason() {
            if !(all || w.filter.is_empty()) {
                return Vec::new();
            }
            let label = format!("objects can't be read: {why}");
            let aria = format!("{path}/objects:unreadable");
            return vec![row(RowKind::Empty, okey, depth, false, label, aria)];
        }
        let groups = [
            ObjectGroup::Keys,
            ObjectGroup::Indexes,
            ObjectGroup::Checks,
            ObjectGroup::Triggers,
        ];
        groups
            .into_iter()
            .flat_map(|group| {
                let gkey = NodeKey::RelationGroup {
                    conn: conn.to_owned(),
                    rel: rel.clone(),
                    group,
                };
                self.group_rows(w, gkey, &path, depth, all, || {
                    relation_entries(objects, group)
                })
            })
            .collect()
    }

    /// A non-empty group's row and, when it is open, its objects one level deeper. Like a
    /// kind group, a collapsed group surfaces nothing under a filter; `entries` is built only
    /// for an open group, so a closed one with thousands of routines costs nothing.
    fn group_rows(
        &self,
        w: &Walk,
        gkey: NodeKey,
        path: &str,
        depth: usize,
        all: bool,
        entries: impl FnOnce() -> Vec<Entry>,
    ) -> Vec<CatalogRow> {
        let (NodeKey::RelationGroup { group, .. } | NodeKey::SchemaGroup { group, .. }) = gkey
        else {
            return Vec::new();
        };
        if self.group_count(&gkey) == 0 {
            return Vec::new();
        }
        let expanded = self.expanded.contains(&gkey);
        let label = object_group_label(group);
        let aria = format!("{path}/{label}");
        let kids: Vec<_> = if expanded { entries() } else { Vec::new() }
            .into_iter()
            .filter(|(name, ..)| all || w.matches(name))
            .map(|(name, detail, object)| {
                let kind = RowKind::Object {
                    group,
                    object,
                    detail,
                };
                let aria = format!("{aria}.{name}");
                row(kind, gkey.clone(), depth + 1, false, name, aria)
            })
            .collect();
        if !all && kids.is_empty() {
            return Vec::new();
        }
        let mut out = vec![row(
            RowKind::Group(group),
            gkey,
            depth,
            expanded,
            label.to_owned(),
            aria,
        )];
        out.extend(kids);
        out
    }

    /// Column and status rows under one expanded relation.
    fn column_rows(&self, w: &Walk, rel: &RelationRef, depth: usize, all: bool) -> Vec<CatalogRow> {
        let conn = w.conn;
        let rkey = NodeKey::Relation {
            conn: conn.to_owned(),
            rel: rel.clone(),
        };
        let path = format!("{conn}.{}.{}", rel.schema(), rel.name());
        let show_status = all || w.filter.is_empty();
        let status = |kind, label: String, tag: &str| {
            if show_status || matches!(kind, RowKind::Failed { .. }) {
                vec![row(
                    kind,
                    rkey.clone(),
                    depth,
                    false,
                    label,
                    format!("{path}:{tag}"),
                )]
            } else {
                Vec::new()
            }
        };
        let columns = self.columns.get(&(conn.to_owned(), rel.clone()));
        let mut out = match columns {
            None | Some(Load::Loading) => status(RowKind::Loading, "loading…".into(), "loading"),
            // Such a relation fails its columns the same way; its objects row says why.
            Some(Load::Failed(_)) if self.unreadable(conn, rel) => {
                return self.relation_object_rows(w, rel, depth, all);
            }
            Some(Load::Failed(m)) => {
                status(RowKind::Failed { message: m.clone() }, m.clone(), "error")
            }
            Some(Load::Ready(c)) if c.is_empty() => {
                status(RowKind::Empty, "no columns".into(), "empty")
            }
            Some(Load::Ready(cols)) => cols
                .iter()
                .filter(|c| all || w.matches(c.name()))
                .map(|c| {
                    let kind = RowKind::Column {
                        type_name: c.type_name().to_owned(),
                        primary_key: c.primary_key().is_some(),
                        inferred: c.inferred(),
                    };
                    row(
                        kind,
                        rkey.clone(),
                        depth,
                        false,
                        c.name().to_owned(),
                        format!("{path}.{}", c.name()),
                    )
                })
                .collect(),
        };
        if let Some(reason) = self.sample_errors.get(&(conn.to_owned(), rel.clone())) {
            let message = format!("attributes not sampled: {reason}");
            out.extend(status(
                RowKind::Failed {
                    message: message.clone(),
                },
                message,
                "error",
            ));
        }
        if matches!(columns, Some(Load::Ready(_))) {
            out.extend(self.relation_object_rows(w, rel, depth, all));
        }
        out
    }
}

/// The error row of a failed objects level; it shows under any filter, since Retry acts.
fn objects_error(okey: NodeKey, depth: usize, message: &str, path: &str) -> CatalogRow {
    row(
        RowKind::Failed {
            message: message.to_owned(),
        },
        okey,
        depth,
        false,
        message.to_owned(),
        format!("{path}/objects:error"),
    )
}

/// The objects a relation group lists, in the order core gives them.
fn relation_entries(o: &RelationObjects, group: ObjectGroup) -> Vec<Entry> {
    let is_check = |c: &&ConstraintInfo| matches!(c.kind(), ConstraintKind::Check);
    match group {
        ObjectGroup::Keys => o
            .constraints()
            .iter()
            .filter(|c| !is_check(c))
            .map(constraint_entry)
            .collect(),
        ObjectGroup::Checks => o
            .constraints()
            .iter()
            .filter(is_check)
            .map(constraint_entry)
            .collect(),
        ObjectGroup::Indexes => o.indexes().iter().map(index_entry).collect(),
        ObjectGroup::Triggers => o
            .triggers()
            .iter()
            .map(|t| {
                let detail = match t.state() {
                    TriggerState::Disabled => "disabled",
                    TriggerState::ReplicaOnly => "replica only",
                    TriggerState::Enabled | _ => "",
                };
                (t.name().to_owned(), detail.to_owned(), t.object_ref())
            })
            .collect(),
        ObjectGroup::Types | ObjectGroup::Routines => Vec::new(),
    }
}

/// The objects a schema group lists, in the order core gives them.
fn schema_entries(o: &SchemaObjects, group: ObjectGroup) -> Vec<Entry> {
    match group {
        ObjectGroup::Types => o
            .types()
            .iter()
            .map(|t| {
                let detail = match t.kind() {
                    TypeKind::Enum => "enum",
                    TypeKind::Domain => "domain",
                    TypeKind::Composite => "composite",
                    TypeKind::Range => "range",
                    TypeKind::Other | _ => "type",
                };
                (t.name().to_owned(), detail.to_owned(), t.object_ref())
            })
            .collect(),
        ObjectGroup::Routines => o
            .routines()
            .iter()
            .map(|r| {
                let detail = match r.kind() {
                    RoutineKind::Procedure => "procedure",
                    RoutineKind::Aggregate => "aggregate",
                    RoutineKind::Function | _ => r.returns().unwrap_or_default(),
                };
                let label = format!("{}({})", r.name(), r.arguments());
                (label, detail.to_owned(), r.object_ref())
            })
            .collect(),
        ObjectGroup::Keys | ObjectGroup::Indexes | ObjectGroup::Checks | ObjectGroup::Triggers => {
            Vec::new()
        }
    }
}

/// An index by name, its detail the words among primary or unique, partial and the method.
fn index_entry(i: &IndexInfo) -> Entry {
    let key = if i.primary() {
        Some("primary")
    } else {
        i.unique().then_some("unique")
    };
    let words: Vec<&str> = key
        .into_iter()
        .chain(i.partial().then_some("partial"))
        .chain(i.method())
        .collect();
    (i.name().to_owned(), words.join(" "), i.object_ref())
}

/// A constraint by name or, nameless, by its words and columns; its detail is its kind.
fn constraint_entry(c: &ConstraintInfo) -> Entry {
    let words = constraint_words(c.kind());
    let label = match c.name() {
        Some(name) => name.to_owned(),
        None if c.columns().is_empty() => words.to_owned(),
        None => format!("{words} ({})", c.columns().join(", ")),
    };
    let detail = match c.kind() {
        ConstraintKind::Exclusion => "exclusion".to_owned(),
        ConstraintKind::PrimaryKey
        | ConstraintKind::Unique
        | ConstraintKind::ForeignKey
        | ConstraintKind::Check => words.to_lowercase(),
        ConstraintKind::Other | _ => "constraint".to_owned(),
    };
    (label, detail, c.object_ref())
}

/// "PRIMARY KEY", "UNIQUE", "FOREIGN KEY", "CHECK", "EXCLUDE", else "CONSTRAINT".
pub fn constraint_words(kind: ConstraintKind) -> &'static str {
    match kind {
        ConstraintKind::PrimaryKey => "PRIMARY KEY",
        ConstraintKind::Unique => "UNIQUE",
        ConstraintKind::ForeignKey => "FOREIGN KEY",
        ConstraintKind::Check => "CHECK",
        ConstraintKind::Exclusion => "EXCLUDE",
        ConstraintKind::Other | _ => "CONSTRAINT",
    }
}

fn row(
    kind: RowKind,
    key: NodeKey,
    depth: usize,
    expanded: bool,
    label: String,
    aria: String,
) -> CatalogRow {
    CatalogRow {
        kind,
        key,
        depth,
        expanded,
        label,
        aria,
    }
}

/// The one schema's name when `schemas` holds exactly one, else None.
pub fn lone_schema(schemas: &[SchemaInfo]) -> Option<&str> {
    match schemas {
        [only] => Some(only.name()),
        _ => None,
    }
}

/// "tables", "views", "materialized views", "foreign tables", "other".
pub fn kind_label(kind: RelationKind) -> &'static str {
    match kind {
        RelationKind::Table => "tables",
        RelationKind::View => "views",
        RelationKind::MaterializedView => "materialized views",
        RelationKind::ForeignTable => "foreign tables",
        RelationKind::Other | _ => "other",
    }
}

/// "keys", "indexes", "checks", "triggers", "types", "routines".
pub fn object_group_label(group: ObjectGroup) -> &'static str {
    match group {
        ObjectGroup::Keys => "keys",
        ObjectGroup::Indexes => "indexes",
        ObjectGroup::Checks => "checks",
        ObjectGroup::Triggers => "triggers",
        ObjectGroup::Types => "types",
        ObjectGroup::Routines => "routines",
    }
}

/// "Keys", "Indexes", "Checks", "Triggers", "Types", "Routines".
pub fn object_group_title(group: ObjectGroup) -> &'static str {
    match group {
        ObjectGroup::Keys => "Keys",
        ObjectGroup::Indexes => "Indexes",
        ObjectGroup::Checks => "Checks",
        ObjectGroup::Triggers => "Triggers",
        ObjectGroup::Types => "Types",
        ObjectGroup::Routines => "Routines",
    }
}

/// The explorer's group heading for a kind: "Tables", "Views", "Materialized views",
/// "Foreign tables", "Other".
pub fn group_title(kind: RelationKind) -> &'static str {
    match kind {
        RelationKind::Table => "Tables",
        RelationKind::View => "Views",
        RelationKind::MaterializedView => "Materialized views",
        RelationKind::ForeignTable => "Foreign tables",
        RelationKind::Other | _ => "Other",
    }
}

/// A column row's dim detail: its short type, marked when it was inferred from samples.
pub fn column_detail(type_name: &str, inferred: bool) -> String {
    let short = short_type(type_name);
    if inferred {
        format!("{short} · inferred")
    } else {
        short
    }
}

/// The tooltip on an inferred column's detail.
pub const INFERRED_COLUMN_TOOLTIP: &str =
    "Inferred from up to 100 sampled items; sampling costs read capacity.";

/// A Postgres `format_type` spelling in the grid header's short name ("character varying(40)[]"
/// becomes "varchar(40)[]"), keeping the modifier and array brackets. Other text is unchanged.
pub fn short_type(type_name: &str) -> String {
    // Peel the array brackets, then lift the first modifier out of the words it sits among
    // ("timestamp(3) with time zone"), so the remaining words match exactly.
    let mut base = type_name;
    while let Some(rest) = base.strip_suffix("[]") {
        base = rest;
    }
    let brackets = &type_name[base.len()..];
    let (words, modifier) = match (base.find('('), base.find(')')) {
        (Some(open), Some(close)) if open < close => (
            format!("{}{}", &base[..open], &base[close + 1..]),
            &base[open..=close],
        ),
        _ => (base.to_owned(), ""),
    };
    let short = match words.as_str() {
        "integer" => "int4",
        "smallint" => "int2",
        "bigint" => "int8",
        "real" => "float4",
        "double precision" => "float8",
        "boolean" => "bool",
        "character varying" => "varchar",
        "character" => "bpchar",
        "bit varying" => "varbit",
        "timestamp without time zone" => "timestamp",
        "timestamp with time zone" => "timestamptz",
        "time without time zone" => "time",
        "time with time zone" => "timetz",
        _ => return type_name.to_owned(),
    };
    format!("{short}{modifier}{brackets}")
}

/// `<schema>.<name>`, each part through `quote_ident` (`"main.Album"` on SQLite).
pub fn qualified_name(engine: Engine, rel: &RelationRef) -> String {
    format!(
        "{}.{}",
        quote_ident(engine, rel.schema()),
        quote_ident(engine, rel.name())
    )
}

/// The context menu items for a catalog row of `kind` on `engine`, in menu order, ids
/// `tree-menu-*`. Loading, empty and truncated rows have none, so their menu never opens.
pub fn catalog_entries(kind: &RowKind, engine: Engine, cx: &App) -> Vec<MenuEntry> {
    let entry = |name, label| MenuEntry {
        id: name,
        label,
        enabled: true,
        keys: None,
    };
    let new_query = |label| entry("tree-menu-new-query", label);
    let copy_name = entry("tree-menu-copy-name", "Copy name");
    let copy_qualified = entry("tree-menu-copy-qualified", "Copy qualified name");
    let refresh = entry("tree-menu-refresh", "Refresh");
    let view_definition = entry("tree-menu-view-definition", "View definition");
    // Counting, INSERT templates and qualified names are SQL text; a SurrealQL engine gets none.
    let language = engine.capabilities().language;
    let sql = language == QueryLanguage::Sql;
    // A MongoDB collection opens with a find and keeps no definition to view.
    let mongo = language == QueryLanguage::MongoShell;
    let generate_select = if mongo {
        "Generate find"
    } else {
        "Generate SELECT"
    };
    let copy_qualified = sql.then_some(copy_qualified);
    match kind {
        RowKind::Relation(kind) => vec![
            MenuEntry {
                keys: keys_for(&OpenSelected, cx),
                ..entry("tree-menu-open", "Open data")
            },
            new_query(if *kind == RelationKind::Table {
                "New query on table"
            } else {
                "New query on view"
            }),
            copy_name,
        ]
        .into_iter()
        .chain(copy_qualified)
        .chain(sql.then(|| entry("tree-menu-count", "Count rows")))
        .chain([entry("tree-menu-generate-select", generate_select)])
        .chain(
            (sql && *kind == RelationKind::Table)
                .then(|| entry("tree-menu-generate-insert", "Generate INSERT")),
        )
        // A table offers its definition only where the engine keeps one (SQLite, MySQL, DuckDB).
        .chain(
            match kind {
                _ if mongo => false,
                RelationKind::View | RelationKind::MaterializedView => true,
                RelationKind::Table => engine.capabilities().table_definition,
                RelationKind::ForeignTable | RelationKind::Other | _ => false,
            }
            .then_some(view_definition),
        )
        .chain([refresh])
        .collect(),
        // A Redis DB lists keys, so its filter is a key pattern rather than the name filter.
        RowKind::Schema if engine.capabilities().key_browser => vec![
            new_query("New query"),
            copy_name,
            entry("tree-menu-filter-keys", "Filter keys…"),
            refresh,
        ],
        RowKind::Schema => vec![new_query("New query"), copy_name, refresh],
        RowKind::Kind(_) => vec![new_query("New query"), refresh],
        RowKind::Column { .. } => [copy_name].into_iter().chain(copy_qualified).collect(),
        RowKind::Failed { .. } => vec![
            entry("tree-menu-retry", "Retry"),
            entry("tree-menu-copy-error", "Copy error"),
        ],
        RowKind::Group(_) => vec![refresh],
        // A DynamoDB index keeps no definition but reads as its own PartiQL target.
        RowKind::Object {
            group: ObjectGroup::Indexes,
            ..
        } if engine == Engine::Dynamodb => vec![
            MenuEntry {
                keys: keys_for(&OpenSelected, cx),
                ..entry("tree-menu-open", "Open data")
            },
            copy_name,
        ],
        RowKind::Object { .. } => vec![
            MenuEntry {
                keys: keys_for(&OpenSelected, cx),
                ..view_definition
            },
            copy_name,
        ],
        RowKind::Loading | RowKind::Empty | RowKind::Truncated => Vec::new(),
        RowKind::Key(_) => vec![
            MenuEntry {
                keys: keys_for(&OpenSelected, cx),
                ..entry("tree-menu-open-key", "Open value")
            },
            copy_name,
        ],
        RowKind::KeyFolder { .. } => vec![copy_name],
        RowKind::LoadMore => Vec::new(),
    }
}

/// `SELECT * FROM <schema>.<name>`, each part through `quote_ident`, laid out by `format_sql`
/// with default options (`"SELECT *\nFROM main.Album"` on SQLite). SurrealDB names the table
/// alone, `` SELECT * FROM `t` ``, since a dotted name reads as a field path there, and so
/// does DynamoDB, whose schema is the region. MongoDB gets `db.getCollection("<name>").find()`,
/// the name JSON-quoted.
pub fn select_sql(engine: Engine, rel: &RelationRef) -> String {
    if engine == Engine::Mongodb {
        return format!("db.getCollection({}).find()", json_string(rel.name()));
    }
    if matches!(engine, Engine::Surrealdb | Engine::Dynamodb) {
        return format!("SELECT * FROM {}", quote_ident(engine, rel.name()));
    }
    let raw = format!(
        "SELECT * FROM {}.{}",
        quote_ident(engine, rel.schema()),
        quote_ident(engine, rel.name())
    );
    format_sql(engine, &raw, &FormatOptions::default())
}

/// The text that opens `rel` on `conn`: `select_sql`, and when the relation's database is not
/// the connection's, a `USE DB` first on SurrealDB or `db.getSiblingDB(..)` on MongoDB.
pub fn select_text(conn: &ConnectionConfig, rel: &RelationRef) -> String {
    let engine = conn.engine();
    let select = select_sql(engine, rel);
    if conn.database() == Some(rel.schema()) {
        return select;
    }
    match engine {
        Engine::Surrealdb => format!("USE DB {};\n{select};", quote_ident(engine, rel.schema())),
        Engine::Mongodb => select.replacen(
            "db.",
            &format!("db.getSiblingDB({}).", json_string(rel.schema())),
            1,
        ),
        Engine::Postgres
        | Engine::Sqlite
        | Engine::Mysql
        | Engine::Mssql
        | Engine::Duckdb
        | Engine::Redis
        | Engine::Dynamodb
        | _ => select,
    }
}

/// `SELECT * FROM <table>.<index>`, both through `quote_ident`: how PartiQL reads a DynamoDB
/// secondary index (`SELECT * FROM "orders"."by_status"`).
pub fn index_select_sql(engine: Engine, rel: &RelationRef, index: &str) -> String {
    format!(
        "SELECT * FROM {}.{}",
        quote_ident(engine, rel.name()),
        quote_ident(engine, index)
    )
}

fn json_string(s: &str) -> String {
    serde_json::Value::from(s).to_string()
}

/// `INSERT INTO <q> (<cols>)\nVALUES (NULL, ...)` with one NULL per column, every name
/// through `quote_ident`.
pub fn insert_sql(engine: Engine, rel: &RelationRef, columns: &[&str]) -> String {
    let names: Vec<_> = columns.iter().map(|c| quote_ident(engine, c)).collect();
    format!(
        "INSERT INTO {} ({})\nVALUES ({})",
        qualified_name(engine, rel),
        names.join(", "),
        vec!["NULL"; columns.len()].join(", ")
    )
}

/// A count's notice text, `"main.Album: 12,345 rows"` (`"1 row"` for one).
pub fn count_text(rel: &RelationRef, n: u64) -> String {
    let noun = if n == 1 { "row" } else { "rows" };
    format!(
        "{}.{}: {} {noun}",
        rel.schema(),
        rel.name(),
        group_digits(n)
    )
}

fn relation_objects_query(rel: &RelationRef) -> CatalogQuery {
    CatalogQuery::RelationObjects { rel: rel.clone() }
}

fn schema_objects_query(schema: &str) -> CatalogQuery {
    CatalogQuery::SchemaObjects {
        schema: schema.to_owned(),
    }
}

fn load_of<T>(
    result: Result<CatalogData, String>,
    pick: impl FnOnce(CatalogData) -> Option<T>,
) -> Load<T> {
    match result {
        Ok(d) => pick(d).map_or_else(
            || Load::Failed("unexpected catalog reply".into()),
            Load::Ready,
        ),
        Err(e) => Load::Failed(e),
    }
}
