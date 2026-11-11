//! The schema tree model: lazy levels, cache, errors, refresh, filter and the open-as-SELECT text.
use std::collections::HashSet;

use RelationKind::{Table, View};
use dbento_app::catalog::{CatalogData, CatalogQuery, CatalogReply};
use dbento_app::schema_tree::{
    CatalogTree, LoadRequest, NodeKey, ObjectGroup, RevealStep, RowKind, group_title, kind_label,
    object_group_label, object_group_title, select_sql, select_text, short_type,
};
use dbento_core::{
    ColumnMeta, ConstraintInfo, ConstraintKind, Engine, IndexInfo, ObjectRef, Relation,
    RelationKind, RelationList, RelationObjects, RelationRef, RoutineInfo, RoutineKind, SchemaInfo,
    SchemaObjects, TriggerInfo, TriggerState, TypeInfo, TypeKind,
};

fn conn() -> NodeKey {
    NodeKey::Connection("lite".into())
}
fn schema(s: &str) -> NodeKey {
    NodeKey::Schema {
        conn: "lite".into(),
        schema: s.into(),
    }
}
fn kind(s: &str, k: RelationKind) -> NodeKey {
    NodeKey::Kind {
        conn: "lite".into(),
        schema: s.into(),
        kind: k,
    }
}
fn rel(s: &str, r: &str) -> NodeKey {
    NodeKey::Relation {
        conn: "lite".into(),
        rel: RelationRef::new(s, r),
    }
}

fn answer(t: &mut CatalogTree, req: LoadRequest, result: Result<CatalogData, String>) {
    t.apply(
        &req.conn,
        CatalogReply {
            generation: req.generation,
            query: req.query,
            result,
        },
    );
}
fn schemas(names: &[&str]) -> CatalogData {
    CatalogData::Schemas(names.iter().map(|n| SchemaInfo::new(*n)).collect())
}
fn rels(items: &[(&str, RelationKind)], truncated: bool) -> CatalogData {
    CatalogData::Relations(RelationList::new(
        items.iter().map(|(n, k)| Relation::new(*n, *k)).collect(),
        truncated,
    ))
}
fn cols(items: &[(&str, &str, Option<u16>)]) -> CatalogData {
    CatalogData::Columns(
        items
            .iter()
            .map(|(n, t, pk)| ColumnMeta::new(*n, *t, pk.is_none(), *pk, None))
            .collect(),
    )
}
fn shape(t: &CatalogTree, filter: &str) -> Vec<String> {
    t.rows("lite", 1, filter)
        .iter()
        .map(|r| format!("{}{}", "  ".repeat(r.depth - 1), r.aria))
        .collect()
}
const FULL: [&str; 8] = [
    "lite.main",
    "  lite.main/tables",
    "    lite.main.Album",
    "      lite.main.Album.AlbumId",
    "      lite.main.Album.Title",
    "    lite.main.Artist",
    "  lite.main/views",
    "    lite.main.Recent",
];
/// lite, main and Album expanded, every load answered.
fn loaded() -> CatalogTree {
    let mut t = CatalogTree::new();
    let r = t.toggle(&conn()).unwrap();
    answer(&mut t, r, Ok(schemas(&["main"])));
    let r = t.toggle(&schema("main")).unwrap();
    answer(
        &mut t,
        r,
        Ok(rels(
            &[("Album", Table), ("Artist", Table), ("Recent", View)],
            false,
        )),
    );
    let r = t.toggle(&rel("main", "Album")).unwrap();
    answer(
        &mut t,
        r,
        Ok(cols(&[
            ("AlbumId", "INTEGER", Some(1)),
            ("Title", "NVARCHAR(160)", None),
        ])),
    );
    t
}

#[test]
fn each_level_loads_once_on_first_expand() {
    let mut t = CatalogTree::new();
    assert!(shape(&t, "").is_empty());
    let req = t.toggle(&conn()).unwrap();
    assert_eq!(
        req,
        LoadRequest {
            conn: "lite".into(),
            generation: 0,
            query: CatalogQuery::Schemas
        }
    );
    assert_eq!(shape(&t, ""), ["lite:loading"]);
    assert_eq!(t.rows("lite", 1, "")[0].label, "loading…");
    assert!(t.toggle(&conn()).is_none()); // collapse
    assert!(shape(&t, "").is_empty());
    assert!(t.toggle(&conn()).is_none()); // expand again: still loading, no second load
    answer(&mut t, req, Ok(schemas(&["main"])));
    assert_eq!(shape(&t, ""), ["lite.main"]);
    let t = loaded();
    assert_eq!(shape(&t, ""), FULL);
    let mut t = t;
    assert!(t.toggle(&rel("main", "Album")).is_none());
    assert!(
        t.toggle(&rel("main", "Album")).is_none(),
        "cached columns are not reloaded"
    );
    assert_eq!(shape(&t, ""), FULL);
}

#[test]
fn rows_carry_kinds_labels_and_column_types() {
    let t = loaded();
    let rows = t.rows("lite", 1, "");
    assert_eq!(rows[0].kind, RowKind::Schema);
    assert!(rows[0].expanded);
    assert_eq!(
        (rows[1].kind.clone(), rows[1].label.as_str()),
        (RowKind::Kind(Table), "tables")
    );
    assert_eq!(rows[2].kind, RowKind::Relation(Table));
    assert_eq!(rows[2].key, rel("main", "Album"));
    assert_eq!(
        rows[3].kind,
        RowKind::Column {
            type_name: "INTEGER".into(),
            primary_key: true,
            inferred: false
        }
    );
    assert_eq!(
        (rows[3].label.as_str(), rows[3].key.clone()),
        ("AlbumId", rel("main", "Album"))
    );
    assert_eq!(
        rows[4].kind,
        RowKind::Column {
            type_name: "NVARCHAR(160)".into(),
            primary_key: false,
            inferred: false
        }
    );
    assert!(!rows[5].expanded);
}

#[test]
fn a_failed_level_shows_its_error_and_retry_reloads() {
    let mut t = CatalogTree::new();
    let req = t.toggle(&conn()).unwrap();
    answer(&mut t, req, Err("boom".into()));
    let rows = t.rows("lite", 1, "");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].aria.as_str(), rows[0].key.clone()),
        ("lite:error", conn())
    );
    assert_eq!(
        rows[0].kind,
        RowKind::Failed {
            message: "boom".into()
        }
    );
    let again = t.retry(&conn()).unwrap();
    assert_eq!(again.query, CatalogQuery::Schemas);
    assert_eq!(shape(&t, ""), ["lite:loading"]);
    assert!(t.retry(&conn()).is_none(), "nothing to retry while loading");
    answer(&mut t, again, Err("boom".into()));
    // Collapsing and expanding a failed level reloads it too.
    assert!(t.toggle(&conn()).is_none());
    assert!(t.toggle(&conn()).is_some());
}

#[test]
fn empty_levels_say_so() {
    let mut t = CatalogTree::new();
    let r = t.toggle(&conn()).unwrap();
    answer(&mut t, r, Ok(schemas(&[])));
    assert_eq!(shape(&t, ""), ["lite:empty"]);
    assert_eq!(t.rows("lite", 1, "")[0].label, "no schemas");
    let mut t = CatalogTree::new();
    let r = t.toggle(&conn()).unwrap();
    answer(&mut t, r, Ok(schemas(&["main"])));
    let r = t.toggle(&schema("main")).unwrap();
    answer(&mut t, r, Ok(rels(&[("t", Table)], false)));
    let r = t.toggle(&rel("main", "t")).unwrap();
    answer(&mut t, r, Ok(cols(&[])));
    assert_eq!(
        shape(&t, ""),
        [
            "lite.main",
            "  lite.main/tables",
            "    lite.main.t",
            "      lite.main.t:empty"
        ]
    );
    assert_eq!(t.rows("lite", 1, "")[3].label, "no columns");
}

#[test]
fn truncated_relations_end_with_a_filter_hint() {
    let mut t = CatalogTree::new();
    let r = t.toggle(&conn()).unwrap();
    answer(&mut t, r, Ok(schemas(&["main"])));
    let r = t.toggle(&schema("main")).unwrap();
    answer(&mut t, r, Ok(rels(&[("a", Table), ("v", View)], true)));
    let rows = t.rows("lite", 1, "");
    let last = rows.last().unwrap();
    assert_eq!(
        (last.aria.as_str(), last.kind.clone(), last.depth),
        ("lite.main:truncated", RowKind::Truncated, 2)
    );
    assert_eq!(last.label, "first 10,000 shown; filter to narrow");
}

#[test]
fn stale_replies_after_refresh_are_dropped_and_expanded_levels_reload() {
    let mut t = loaded();
    let reqs = t.refresh();
    assert_eq!(t.generation(), 1);
    let got: HashSet<String> = reqs
        .iter()
        .map(|r| format!("{}:{}:{:?}", r.conn, r.generation, r.query))
        .collect();
    let want: HashSet<String> = [
        CatalogQuery::Schemas,
        CatalogQuery::Relations {
            schema: "main".into(),
        },
        CatalogQuery::Columns {
            rel: RelationRef::new("main", "Album"),
        },
    ]
    .iter()
    .map(|q| format!("lite:1:{q:?}"))
    .collect();
    assert_eq!(got, want);
    assert_eq!(shape(&t, ""), ["lite:loading"]);
    // A reply to the generation-0 request lands late and is ignored.
    t.apply(
        "lite",
        CatalogReply {
            generation: 0,
            query: CatalogQuery::Schemas,
            result: Ok(schemas(&["old"])),
        },
    );
    assert_eq!(shape(&t, ""), ["lite:loading"]);
    for req in reqs {
        let data = match &req.query {
            CatalogQuery::Schemas => schemas(&["main"]),
            CatalogQuery::Relations { .. } => rels(
                &[("Album", Table), ("Artist", Table), ("Recent", View)],
                false,
            ),
            CatalogQuery::Columns { .. }
            | CatalogQuery::Count { .. }
            | CatalogQuery::RelationObjects { .. }
            | CatalogQuery::SchemaObjects { .. }
            | CatalogQuery::Definition { .. }
            | CatalogQuery::Keys { .. }
            | CatalogQuery::Databases
            | CatalogQuery::CommandDocs => cols(&[
                ("AlbumId", "INTEGER", Some(1)),
                ("Title", "NVARCHAR(160)", None),
            ]),
        };
        answer(&mut t, req, Ok(data));
    }
    assert_eq!(shape(&t, ""), FULL);
}

#[test]
fn kinds_start_expanded_and_collapse_without_loading() {
    let mut t = loaded();
    assert!(t.is_expanded(&kind("main", Table)));
    assert!(t.toggle(&kind("main", Table)).is_none());
    assert_eq!(
        shape(&t, ""),
        [
            "lite.main",
            "  lite.main/tables",
            "  lite.main/views",
            "    lite.main.Recent"
        ]
    );
}

#[test]
fn the_filter_narrows_loaded_names_and_keeps_their_ancestors() {
    let t = loaded();
    assert_eq!(
        shape(&t, "art"),
        ["lite.main", "  lite.main/tables", "    lite.main.Artist"]
    );
    assert_eq!(
        shape(&t, "TITLE"),
        [
            "lite.main",
            "  lite.main/tables",
            "    lite.main.Album",
            "      lite.main.Album.Title"
        ]
    );
    assert_eq!(
        shape(&t, "album"),
        [
            "lite.main",
            "  lite.main/tables",
            "    lite.main.Album",
            "      lite.main.Album.AlbumId",
            "      lite.main.Album.Title"
        ]
    );
    assert_eq!(shape(&t, "main"), FULL);
    assert_eq!(shape(&t, "  "), FULL);
    assert!(shape(&t, "zzz").is_empty());
}

#[test]
fn other_connections_and_mismatched_replies() {
    let mut t = loaded();
    assert!(t.rows("other", 1, "").is_empty());
    assert!(t.schemas("other").is_none());
    assert_eq!(t.relations("lite", "main").unwrap().items().len(), 3);
    assert_eq!(
        t.columns("lite", &RelationRef::new("main", "Album"))
            .unwrap()
            .len(),
        2
    );
    let mut t2 = CatalogTree::new();
    let r = t2.toggle(&conn()).unwrap();
    // A reply whose data does not fit its query fails that level.
    answer(&mut t2, r, Ok(cols(&[])));
    assert!(matches!(
        t2.rows("lite", 1, "")[0].kind,
        RowKind::Failed { .. }
    ));
    t.toggle(&conn());
    assert!(t.rows("lite", 1, "").is_empty());
}

#[test]
fn select_sql_quotes_per_engine() {
    assert_eq!(
        select_sql(Engine::Postgres, &RelationRef::new("public", "artist")),
        "SELECT *\nFROM public.artist"
    );
    assert_eq!(
        select_sql(
            Engine::Postgres,
            &RelationRef::new("Ünï \"q\" schema", "select")
        ),
        "SELECT *\nFROM \"Ünï \"\"q\"\" schema\".\"select\""
    );
    assert_eq!(
        select_sql(Engine::Sqlite, &RelationRef::new("main", "Album")),
        "SELECT *\nFROM main.Album"
    );
    assert_eq!(
        kind_label(RelationKind::MaterializedView),
        "materialized views"
    );
    assert_eq!(kind_label(RelationKind::ForeignTable), "foreign tables");
    assert_eq!(kind_label(RelationKind::Other), "other");
}

#[test]
fn error_rows_show_under_a_filter_their_parent_misses() {
    let mut t = loaded();
    let r = t.toggle(&rel("main", "Artist")).unwrap();
    answer(&mut t, r, Err("boom".into()));
    assert_eq!(
        shape(&t, "album"),
        [
            "lite.main",
            "  lite.main/tables",
            "    lite.main.Album",
            "      lite.main.Album.AlbumId",
            "      lite.main.Album.Title",
            "    lite.main.Artist",
            "      lite.main.Artist:error"
        ]
    );
    // A schema whose relations failed keeps its error row and Retry under any filter.
    let mut t = CatalogTree::new();
    let r = t.toggle(&conn()).unwrap();
    answer(&mut t, r, Ok(schemas(&["aux", "main"])));
    let r = t.toggle(&schema("aux")).unwrap();
    answer(&mut t, r, Err("denied".into()));
    assert_eq!(shape(&t, "zzz"), ["lite.aux", "  lite.aux:error"]);
}

#[test]
fn the_filter_skips_what_a_collapsed_kind_group_hides() {
    let mut t = loaded();
    t.toggle(&kind("main", Table));
    // Like a collapsed schema or relation, a collapsed group does not surface matches.
    assert!(shape(&t, "art").is_empty());
    assert_eq!(
        shape(&t, "main"),
        [
            "lite.main",
            "  lite.main/tables",
            "  lite.main/views",
            "    lite.main.Recent"
        ]
    );
}

#[test]
fn group_titles_are_capitalised_and_arias_keep_the_lowercase_kind() {
    assert_eq!(group_title(RelationKind::Table), "Tables");
    assert_eq!(group_title(RelationKind::View), "Views");
    assert_eq!(
        group_title(RelationKind::MaterializedView),
        "Materialized views"
    );
    assert_eq!(group_title(RelationKind::ForeignTable), "Foreign tables");
    assert_eq!(group_title(RelationKind::Other), "Other");
    assert_eq!(kind_label(RelationKind::Table), "tables");
}
#[test]
fn column_types_read_as_the_grid_header_does() {
    for (long, short) in [
        ("integer", "int4"),
        ("smallint", "int2"),
        ("bigint", "int8"),
        ("real", "float4"),
        ("double precision", "float8"),
        ("boolean", "bool"),
        ("character varying(40)", "varchar(40)"),
        ("character varying", "varchar"),
        ("character(3)", "bpchar(3)"),
        ("bit varying(8)", "varbit(8)"),
        ("timestamp without time zone", "timestamp"),
        ("timestamp(3) with time zone", "timestamptz(3)"),
        ("time with time zone", "timetz"),
        ("time(0) without time zone", "time(0)"),
        ("integer[]", "int4[]"),
        ("character varying(40)[]", "varchar(40)[]"),
        ("numeric(10,2)", "numeric(10,2)"),
        ("public.citext", "public.citext"),
        ("NVARCHAR(160)", "NVARCHAR(160)"),
        ("INTEGER", "INTEGER"),
        ("", ""),
    ] {
        assert_eq!(short_type(long), short, "{long}");
    }
}

#[test]
fn expand_opens_and_never_collapses() {
    let mut t = CatalogTree::new();
    assert!(
        t.expand(&conn()).is_some(),
        "first open requests the schema load"
    );
    assert!(t.is_expanded(&conn()));
    assert!(
        t.expand(&conn()).is_none(),
        "an open node loads nothing more"
    );
    assert!(t.is_expanded(&conn()));
    t.toggle(&kind("main", Table));
    assert!(t.expand(&kind("main", Table)).is_none());
    assert!(t.is_expanded(&kind("main", Table)));
    t.expand(&kind("main", Table));
    assert!(t.is_expanded(&kind("main", Table)));
}

// Unwraps a waiting step to its one load.
fn waiting(step: RevealStep) -> LoadRequest {
    match step {
        RevealStep::Waiting(mut loads) if loads.len() == 1 => loads.remove(0),
        other @ (RevealStep::Waiting(_) | RevealStep::Shown | RevealStep::Missing | _) => {
            panic!("expected one load, got {other:?}")
        }
    }
}

#[test]
fn reveal_walks_down_the_lazy_levels_and_opens_the_kind_group() {
    let mut t = CatalogTree::new();
    let album = RelationRef::new("main", "Album");
    t.toggle(&kind("main", Table));
    let first = waiting(t.reveal("lite", &album));
    assert!(t.is_expanded(&conn()));
    assert_eq!(
        t.reveal("lite", &album),
        RevealStep::Waiting(vec![]),
        "still loading"
    );
    answer(&mut t, first, Ok(schemas(&["main", "other"])));
    let second = waiting(t.reveal("lite", &album));
    assert!(t.is_expanded(&schema("main")));
    answer(
        &mut t,
        second,
        Ok(rels(&[("Album", Table), ("Recent", View)], false)),
    );
    assert_eq!(t.reveal("lite", &album), RevealStep::Shown);
    assert!(t.is_expanded(&kind("main", Table)));
    let shown: Vec<_> = t.rows("lite", 0, "").into_iter().map(|r| r.key).collect();
    assert!(shown.contains(&rel("main", "Album")));
    assert!(!t.is_expanded(&schema("other")), "only the path opens");
}

#[test]
fn reveal_gives_up_on_a_gone_relation_or_a_failed_load() {
    let album = RelationRef::new("main", "Album");
    let mut t = CatalogTree::new();
    let load = waiting(t.reveal("lite", &album));
    answer(&mut t, load, Err("no such file".into()));
    assert_eq!(t.reveal("lite", &album), RevealStep::Missing);

    let mut t = CatalogTree::new();
    let load = waiting(t.reveal("lite", &album));
    answer(&mut t, load, Ok(schemas(&["temp"])));
    assert_eq!(
        t.reveal("lite", &album),
        RevealStep::Missing,
        "no such schema"
    );

    let mut t = CatalogTree::new();
    let load = waiting(t.reveal("lite", &album));
    answer(&mut t, load, Ok(schemas(&["main"])));
    let load = waiting(t.reveal("lite", &album));
    answer(&mut t, load, Ok(rels(&[("Recent", View)], false)));
    assert_eq!(
        t.reveal("lite", &album),
        RevealStep::Missing,
        "dropped table"
    );

    let mut t = CatalogTree::new();
    let load = waiting(t.reveal("lite", &album));
    answer(&mut t, load, Ok(schemas(&["main"])));
    let load = waiting(t.reveal("lite", &album));
    answer(&mut t, load, Err("locked".into()));
    assert_eq!(t.reveal("lite", &album), RevealStep::Missing);
}

fn album() -> RelationRef {
    RelationRef::new("main", "Album")
}
fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}
// fixture: the only callers of the core constructors.
fn idx(name: &str, cols: &[&str], unique: bool, method: Option<&str>) -> IndexInfo {
    IndexInfo::new(
        album(),
        name,
        strings(cols),
        unique,
        false,
        false,
        method.map(str::to_owned),
    )
}
// fixture
fn key(name: Option<&str>, kind: ConstraintKind, cols: &[&str]) -> ConstraintInfo {
    ConstraintInfo::new(album(), name.map(str::to_owned), kind, strings(cols), None)
}
// fixture
fn trig(name: &str, state: TriggerState) -> TriggerInfo {
    TriggerInfo::new(album(), name, state)
}
// fixture
fn album_objects() -> CatalogData {
    CatalogData::RelationObjects(RelationObjects::new(
        vec![idx("album_title", &["Title"], true, Some("btree"))],
        vec![
            key(None, ConstraintKind::PrimaryKey, &["AlbumId"]),
            key(Some("title_len"), ConstraintKind::Check, &["Title"]),
        ],
        vec![trig("album_touch", TriggerState::Disabled)],
    ))
}
// fixture
fn main_objects(truncated: bool) -> CatalogData {
    CatalogData::SchemaObjects(SchemaObjects::new(
        vec![TypeInfo::new("main", "mood", TypeKind::Enum)],
        vec![
            RoutineInfo::new(
                "main",
                "add",
                RoutineKind::Function,
                "a integer, b integer",
                Some("integer".to_owned()),
            ),
            RoutineInfo::new("main", "p", RoutineKind::Procedure, "", None),
        ],
        truncated,
    ))
}
// fixture
fn no_objects() -> (CatalogData, CatalogData) {
    (
        CatalogData::RelationObjects(RelationObjects::new(vec![], vec![], vec![])),
        CatalogData::SchemaObjects(SchemaObjects::new(vec![], vec![], false)),
    )
}
fn rgroup(group: ObjectGroup) -> NodeKey {
    NodeKey::RelationGroup {
        conn: "lite".into(),
        rel: album(),
        group,
    }
}
fn sgroup(group: ObjectGroup) -> NodeKey {
    NodeKey::SchemaGroup {
        conn: "lite".into(),
        schema: "main".into(),
        group,
    }
}
/// `loaded()` with Album's and main's objects asked and answered with `album` and `main`.
fn with_objects(
    album: Result<CatalogData, String>,
    main: Result<CatalogData, String>,
) -> CatalogTree {
    let mut t = loaded();
    for req in t.objects_to_load() {
        let result = match req.query {
            CatalogQuery::RelationObjects { .. } => album.clone(),
            CatalogQuery::Schemas
            | CatalogQuery::Relations { .. }
            | CatalogQuery::Columns { .. }
            | CatalogQuery::Count { .. }
            | CatalogQuery::SchemaObjects { .. }
            | CatalogQuery::Definition { .. }
            | CatalogQuery::Keys { .. }
            | CatalogQuery::Databases
            | CatalogQuery::CommandDocs => main.clone(),
        };
        answer(&mut t, req, result);
    }
    t
}
/// The data each tree level of `loaded()` answers with.
fn fixture_for(q: &CatalogQuery) -> CatalogData {
    match q {
        CatalogQuery::Schemas => schemas(&["main"]),
        CatalogQuery::Relations { .. } => rels(
            &[("Album", Table), ("Artist", Table), ("Recent", View)],
            false,
        ),
        CatalogQuery::Columns { .. } => cols(&[
            ("AlbumId", "INTEGER", Some(1)),
            ("Title", "NVARCHAR(160)", None),
        ]),
        CatalogQuery::RelationObjects { .. } => album_objects(),
        CatalogQuery::SchemaObjects { .. } => main_objects(false),
        other @ (CatalogQuery::Count { .. }
        | CatalogQuery::Definition { .. }
        | CatalogQuery::Keys { .. }
        | CatalogQuery::Databases
        | CatalogQuery::CommandDocs) => panic!("{other:?} is no tree level"),
    }
}
const ALL_GROUPS: [ObjectGroup; 6] = [
    ObjectGroup::Keys,
    ObjectGroup::Indexes,
    ObjectGroup::Checks,
    ObjectGroup::Triggers,
    ObjectGroup::Types,
    ObjectGroup::Routines,
];

#[test]
fn objects_load_only_for_expanded_relations_and_schemas() {
    let mut t = loaded();
    assert_eq!(shape(&t, ""), FULL, "absent objects add no rows");
    let mut asked: Vec<_> = t.objects_to_load().into_iter().map(|r| r.query).collect();
    asked.sort_by_key(|q| format!("{q:?}"));
    assert_eq!(
        asked,
        [
            CatalogQuery::RelationObjects { rel: album() },
            CatalogQuery::SchemaObjects {
                schema: "main".into()
            },
        ]
    );
    assert!(
        t.objects_to_load().is_empty(),
        "loading objects are not asked twice"
    );
    assert_eq!(shape(&t, ""), FULL, "loading objects add no rows");
    // A definition is no level of the tree: ensure starts nothing and apply stores nothing.
    let object = ObjectRef::relation(album());
    let q = CatalogQuery::Definition {
        object: object.clone(),
    };
    assert!(t.ensure("lite", &q).is_none());
    t.apply(
        "lite",
        CatalogReply {
            generation: 0,
            query: q,
            result: Err("x".into()),
        },
    );
    assert_eq!(shape(&t, ""), FULL);
}

#[test]
fn groups_toggle_locally_and_count_their_objects() {
    let mut t = with_objects(Ok(album_objects()), Ok(main_objects(false)));
    for g in &ALL_GROUPS[..4] {
        assert!(!t.is_expanded(&rgroup(*g)), "{g:?} starts collapsed");
        assert!(
            t.toggle(&rgroup(*g)).is_none(),
            "a group opens without a load"
        );
        assert!(t.is_expanded(&rgroup(*g)));
    }
    for g in &ALL_GROUPS[4..] {
        assert!(t.toggle(&sgroup(*g)).is_none());
    }
    let okey = NodeKey::Objects(Box::new(rel("main", "Album")));
    assert!(t.toggle(&okey).is_none());
    assert!(!t.is_expanded(&okey), "an objects key never opens");
    assert!(t.objects_to_load().is_empty());
    let counts = [
        t.group_count(&rgroup(ObjectGroup::Keys)),
        t.group_count(&rgroup(ObjectGroup::Indexes)),
        t.group_count(&rgroup(ObjectGroup::Checks)),
        t.group_count(&rgroup(ObjectGroup::Triggers)),
        t.group_count(&sgroup(ObjectGroup::Types)),
        t.group_count(&sgroup(ObjectGroup::Routines)),
    ];
    assert_eq!(counts, [1, 1, 1, 1, 1, 2]);
    assert_eq!(t.group_count(&rel("main", "Album")), 0, "only groups count");
    assert_eq!(
        t.relation_objects("lite", &album())
            .unwrap()
            .constraints()
            .len(),
        2
    );
    assert_eq!(
        t.schema_objects("lite", "main").unwrap().routines().len(),
        2
    );
    let artist = RelationRef::new("main", "Artist");
    assert!(
        t.relation_objects("lite", &artist).is_none(),
        "a collapsed relation loads none"
    );
}

#[test]
fn a_failed_objects_load_waits_for_retry() {
    let denied = "permission denied for table pg_proc";
    let mut t = with_objects(Err(denied.into()), Err("denied".into()));
    assert!(t.relation_objects("lite", &album()).is_none());
    assert!(t.objects_to_load().is_empty(), "a failure waits for Retry");
    let okey = NodeKey::Objects(Box::new(rel("main", "Album")));
    let r = t.retry(&okey).expect("a failed objects load retries");
    assert_eq!(r.query, CatalogQuery::RelationObjects { rel: album() });
    assert!(t.retry(&okey).is_none(), "a loading level is not retried");
    assert!(
        t.retry(&NodeKey::Objects(Box::new(schema("main"))))
            .is_some()
    );
    answer(&mut t, r, Ok(album_objects()));
    assert!(t.relation_objects("lite", &album()).is_some());
    assert!(t.retry(&okey).is_none(), "a loaded level is not retried");
}

#[test]
fn re_expanding_a_relation_restarts_its_failed_objects() {
    let mut t = with_objects(Err("boom".into()), Ok(main_objects(false)));
    assert!(t.toggle(&rel("main", "Album")).is_none());
    assert!(
        t.toggle(&rel("main", "Album")).is_none(),
        "the columns stay cached"
    );
    let again: Vec<_> = t.objects_to_load().into_iter().map(|r| r.query).collect();
    assert_eq!(again, [CatalogQuery::RelationObjects { rel: album() }]);
}

#[test]
fn refresh_and_forget_cover_the_objects() {
    let mut t = with_objects(Ok(album_objects()), Ok(main_objects(false)));
    t.toggle(&rgroup(ObjectGroup::Keys));
    let reqs = t.refresh();
    assert_eq!(t.generation(), 1);
    assert!(reqs.iter().all(|r| r.generation == 1));
    assert!(
        t.relation_objects("lite", &album()).is_none(),
        "refresh drops the objects"
    );
    let named = |reqs: &[LoadRequest], what: &str| {
        reqs.iter()
            .any(|r| format!("{:?}", r.query).starts_with(what))
    };
    assert!(named(&reqs, "RelationObjects") && named(&reqs, "SchemaObjects"));
    for r in reqs {
        let data = fixture_for(&r.query);
        answer(&mut t, r, Ok(data));
    }
    // A reply from before the refresh is ignored.
    t.apply(
        "lite",
        CatalogReply {
            generation: 0,
            query: CatalogQuery::RelationObjects { rel: album() },
            result: Ok(no_objects().0),
        },
    );
    let kept = t
        .relation_objects("lite", &album())
        .map(|o| o.constraints().len());
    assert_eq!(kept, Some(2), "a stale reply is ignored");
    assert!(
        t.is_expanded(&rgroup(ObjectGroup::Keys)),
        "refresh keeps group state"
    );
    let reqs = t.refresh_connection("lite");
    assert!(named(&reqs, "RelationObjects") && named(&reqs, "SchemaObjects"));
    assert!(
        t.schema_objects("lite", "main").is_none(),
        "the reload is in flight"
    );
    t.forget("lite");
    assert!(t.objects_to_load().is_empty());
    assert!(
        !t.is_expanded(&rgroup(ObjectGroup::Keys)),
        "forget drops group state"
    );
    assert!(t.relation_objects("lite", &album()).is_none());
}

#[test]
fn group_labels_and_titles() {
    assert_eq!(
        ALL_GROUPS.map(object_group_label),
        ["keys", "indexes", "checks", "triggers", "types", "routines"]
    );
    assert_eq!(
        ALL_GROUPS.map(object_group_title),
        ["Keys", "Indexes", "Checks", "Triggers", "Types", "Routines"]
    );
}

fn has(shape: &[String], row: &str) -> bool {
    shape.iter().any(|r| r == row)
}

#[test]
fn object_groups_follow_columns_and_kinds_collapsed() {
    let mut t = with_objects(Ok(album_objects()), Ok(main_objects(false)));
    assert_eq!(
        shape(&t, ""),
        [
            "lite.main",
            "  lite.main/tables",
            "    lite.main.Album",
            "      lite.main.Album.AlbumId",
            "      lite.main.Album.Title",
            "      lite.main.Album/keys",
            "      lite.main.Album/indexes",
            "      lite.main.Album/checks",
            "      lite.main.Album/triggers",
            "    lite.main.Artist",
            "  lite.main/views",
            "    lite.main.Recent",
            "  lite.main/types",
            "  lite.main/routines",
        ]
    );
    for g in &ALL_GROUPS[..4] {
        t.toggle(&rgroup(*g));
    }
    for g in &ALL_GROUPS[4..] {
        t.toggle(&sgroup(*g));
    }
    assert_eq!(
        shape(&t, ""),
        [
            "lite.main",
            "  lite.main/tables",
            "    lite.main.Album",
            "      lite.main.Album.AlbumId",
            "      lite.main.Album.Title",
            "      lite.main.Album/keys",
            "        lite.main.Album/keys.PRIMARY KEY (AlbumId)",
            "      lite.main.Album/indexes",
            "        lite.main.Album/indexes.album_title",
            "      lite.main.Album/checks",
            "        lite.main.Album/checks.title_len",
            "      lite.main.Album/triggers",
            "        lite.main.Album/triggers.album_touch",
            "    lite.main.Artist",
            "  lite.main/views",
            "    lite.main.Recent",
            "  lite.main/types",
            "    lite.main/types.mood",
            "  lite.main/routines",
            "    lite.main/routines.add(a integer, b integer)",
            "    lite.main/routines.p()",
        ]
    );
    let rows = t.rows("lite", 1, "");
    let pk = rows
        .iter()
        .find(|r| r.aria == "lite.main.Album/keys.PRIMARY KEY (AlbumId)")
        .unwrap();
    assert_eq!(
        pk.kind,
        RowKind::Object {
            group: ObjectGroup::Keys,
            object: key(None, ConstraintKind::PrimaryKey, &["AlbumId"]).object_ref(),
            detail: "primary key".into(),
        }
    );
    assert_eq!(pk.label, "PRIMARY KEY (AlbumId)");
    assert_eq!(pk.key, rgroup(ObjectGroup::Keys));
    let types = rows.iter().find(|r| r.aria == "lite.main/types").unwrap();
    assert_eq!(types.kind, RowKind::Group(ObjectGroup::Types));
    assert!(types.expanded);
    let details: Vec<(String, String)> = rows
        .iter()
        .filter_map(|r| match &r.kind {
            RowKind::Object { detail, .. } => Some((r.label.clone(), detail.clone())),
            RowKind::Schema
            | RowKind::Kind(_)
            | RowKind::Relation(_)
            | RowKind::Column { .. }
            | RowKind::Group(_)
            | RowKind::Loading
            | RowKind::Failed { .. }
            | RowKind::Truncated
            | RowKind::Empty
            | RowKind::KeyFolder { .. }
            | RowKind::Key(_)
            | RowKind::LoadMore => None,
        })
        .collect();
    let want: Vec<(String, String)> = [
        ("PRIMARY KEY (AlbumId)", "primary key"),
        ("album_title", "unique btree"),
        ("title_len", "check"),
        ("album_touch", "disabled"),
        ("mood", "enum"),
        ("add(a integer, b integer)", "integer"),
        ("p()", "procedure"),
    ]
    .iter()
    .map(|(l, d)| ((*l).to_owned(), (*d).to_owned()))
    .collect();
    assert_eq!(details, want);
}

#[test]
fn a_replica_only_trigger_says_so_like_a_disabled_one() {
    let objects = CatalogData::RelationObjects(RelationObjects::new(
        vec![],
        vec![],
        vec![
            trig("album_live", TriggerState::Enabled),
            trig("album_mirror", TriggerState::ReplicaOnly),
            trig("album_touch", TriggerState::Disabled),
        ],
    ));
    let mut t = with_objects(Ok(objects), Ok(main_objects(false)));
    t.toggle(&rgroup(ObjectGroup::Triggers));
    let details: Vec<(String, String)> = t
        .rows("lite", 1, "")
        .iter()
        .filter_map(|r| match &r.kind {
            RowKind::Object { detail, .. } => Some((r.label.clone(), detail.clone())),
            RowKind::Schema
            | RowKind::Kind(_)
            | RowKind::Relation(_)
            | RowKind::Column { .. }
            | RowKind::Group(_)
            | RowKind::Loading
            | RowKind::Failed { .. }
            | RowKind::Truncated
            | RowKind::Empty
            | RowKind::KeyFolder { .. }
            | RowKind::Key(_)
            | RowKind::LoadMore => None,
        })
        .collect();
    assert_eq!(
        details,
        [
            ("album_live".to_owned(), String::new()),
            ("album_mirror".to_owned(), "replica only".to_owned()),
            ("album_touch".to_owned(), "disabled".to_owned()),
        ]
    );
}

/// A tree whose Album objects answered that they cannot be read, with Album's columns
/// answered by `columns`.
fn unreadable_album(columns: Result<CatalogData, String>) -> CatalogTree {
    let mut t = CatalogTree::new();
    let r = t.toggle(&conn()).unwrap();
    answer(&mut t, r, Ok(schemas(&["main"])));
    let r = t.toggle(&schema("main")).unwrap();
    answer(&mut t, r, Ok(rels(&[("Album", Table)], false)));
    let r = t.toggle(&rel("main", "Album")).unwrap();
    answer(&mut t, r, columns);
    for req in t.objects_to_load() {
        let result = match req.query {
            CatalogQuery::RelationObjects { .. } => Ok(CatalogData::RelationObjects(
                RelationObjects::unreadable("no such module: nope"),
            )),
            CatalogQuery::Schemas
            | CatalogQuery::Relations { .. }
            | CatalogQuery::Columns { .. }
            | CatalogQuery::Count { .. }
            | CatalogQuery::SchemaObjects { .. }
            | CatalogQuery::Definition { .. }
            | CatalogQuery::Keys { .. }
            | CatalogQuery::Databases
            | CatalogQuery::CommandDocs => Ok(no_objects().1),
        };
        answer(&mut t, req, result);
    }
    t
}

#[test]
fn a_relation_whose_objects_cannot_be_read_says_why_without_an_error() {
    let why = "objects can't be read: no such module: nope";
    // The engine fails the columns of such a table the same way; no error row shows.
    for columns in [
        Err("no such module: nope".to_owned()),
        Ok(cols(&[("x", "", None)])),
    ] {
        let t = unreadable_album(columns.clone());
        let rows = t.rows("lite", 1, "");
        assert!(
            rows.iter()
                .all(|r| !matches!(r.kind, RowKind::Failed { .. })),
            "{columns:?}: {rows:?}"
        );
        let last = rows.last().unwrap();
        assert_eq!(
            (&last.kind, last.label.as_str(), last.aria.as_str()),
            (&RowKind::Empty, why, "lite.main.Album/objects:unreadable"),
            "{columns:?}"
        );
        assert!(
            !shape(&t, "zzz")
                .iter()
                .any(|r| r.ends_with("objects:unreadable")),
            "a status row hides under a filter"
        );
    }
}

#[test]
fn empty_groups_are_omitted() {
    let (r, s) = no_objects();
    assert_eq!(shape(&with_objects(Ok(r), Ok(s)), ""), FULL);
}

#[test]
fn a_capped_schema_listing_says_so_after_its_groups() {
    let t = with_objects(Ok(album_objects()), Ok(main_objects(true)));
    let shape = shape(&t, "");
    assert_eq!(
        &shape[shape.len() - 3..],
        [
            "  lite.main/types",
            "  lite.main/routines",
            "  lite.main/objects:truncated"
        ]
    );
    let last = t.rows("lite", 1, "").pop().unwrap();
    assert_eq!(last.kind, RowKind::Truncated);
    assert_eq!(last.key, NodeKey::Objects(Box::new(schema("main"))));
    assert_eq!(last.label, "first 10,000 of each shown; filter to narrow");
}

#[test]
fn the_filter_matches_object_names_in_open_groups_only() {
    let mut t = with_objects(Ok(album_objects()), Ok(main_objects(false)));
    assert_eq!(
        shape(&t, "title"),
        [
            "lite.main",
            "  lite.main/tables",
            "    lite.main.Album",
            "      lite.main.Album.Title",
        ],
        "collapsed groups surface nothing"
    );
    t.toggle(&rgroup(ObjectGroup::Indexes));
    t.toggle(&rgroup(ObjectGroup::Checks));
    t.toggle(&sgroup(ObjectGroup::Routines));
    assert_eq!(
        shape(&t, "TITLE"),
        [
            "lite.main",
            "  lite.main/tables",
            "    lite.main.Album",
            "      lite.main.Album.Title",
            "      lite.main.Album/indexes",
            "        lite.main.Album/indexes.album_title",
            "      lite.main.Album/checks",
            "        lite.main.Album/checks.title_len",
        ]
    );
    assert_eq!(
        shape(&t, "add"),
        [
            "lite.main",
            "  lite.main/routines",
            "    lite.main/routines.add(a integer, b integer)",
        ]
    );
    // A matching relation shows all of its groups, open ones with every object.
    assert_eq!(
        shape(&t, "album"),
        [
            "lite.main",
            "  lite.main/tables",
            "    lite.main.Album",
            "      lite.main.Album.AlbumId",
            "      lite.main.Album.Title",
            "      lite.main.Album/keys",
            "      lite.main.Album/indexes",
            "        lite.main.Album/indexes.album_title",
            "      lite.main.Album/checks",
            "        lite.main.Album/checks.title_len",
            "      lite.main.Album/triggers",
        ]
    );
}

#[test]
fn a_failed_objects_load_shows_one_error_row_with_its_retry_key() {
    let denied = "permission denied for table pg_proc";
    let mut t = with_objects(Err(denied.into()), Err("denied".into()));
    let all = shape(&t, "");
    assert!(
        has(&all, "      lite.main.Album.Title"),
        "loaded columns stay"
    );
    assert!(has(&all, "      lite.main.Album/objects:error"));
    assert_eq!(
        all.last().map(String::as_str),
        Some("  lite.main/objects:error")
    );
    let okey = NodeKey::Objects(Box::new(rel("main", "Album")));
    let rows = t.rows("lite", 1, "");
    let err = rows
        .iter()
        .find(|r| r.aria == "lite.main.Album/objects:error")
        .unwrap();
    assert_eq!(
        err.kind,
        RowKind::Failed {
            message: denied.into()
        }
    );
    assert_eq!(err.key, okey);
    // Error rows show under a filter their relation misses, as a column error does.
    assert_eq!(
        shape(&t, "zzz"),
        [
            "lite.main",
            "  lite.main/tables",
            "    lite.main.Album",
            "      lite.main.Album/objects:error",
            "  lite.main/objects:error",
        ]
    );
    let r = t.retry(&okey).expect("the error row's key retries");
    answer(&mut t, r, Ok(album_objects()));
    assert!(has(&shape(&t, ""), "      lite.main.Album/keys"));
}

#[test]
fn a_connection_refresh_drops_its_own_loads_in_flight_but_not_anothers() {
    let mut t = CatalogTree::new();
    let lite = t.toggle(&conn()).unwrap();
    let pg = t.toggle(&NodeKey::Connection("pg".into())).unwrap();
    let fresh = t.refresh_connection("lite");
    // Both loads were sent before the refresh; only lite's went stale.
    answer(&mut t, lite, Ok(schemas(&["stale"])));
    assert!(
        t.rows("lite", 1, "").iter().all(|r| r.aria != "lite.stale"),
        "a pre-refresh reply shows"
    );
    answer(&mut t, pg, Ok(schemas(&["public"])));
    assert!(t.rows("pg", 1, "").iter().any(|r| r.aria == "pg.public"));
    for r in fresh {
        answer(&mut t, r, Ok(schemas(&["main"])));
    }
    assert!(t.rows("lite", 1, "").iter().any(|r| r.aria == "lite.main"));
}

#[test]
fn re_expanding_a_schema_restarts_its_failed_objects() {
    let mut t = with_objects(Ok(album_objects()), Err("boom".into()));
    assert!(t.toggle(&schema("main")).is_none());
    assert!(
        t.toggle(&schema("main")).is_none(),
        "the relations stay cached"
    );
    let again: Vec<_> = t.objects_to_load().into_iter().map(|r| r.query).collect();
    assert_eq!(
        again,
        [CatalogQuery::SchemaObjects {
            schema: "main".into()
        }]
    );
}

#[test]
fn a_connection_refresh_keeps_another_connections_objects() {
    let mut t = with_objects(Ok(album_objects()), Ok(main_objects(false)));
    let pg = |k: NodeKey| match k {
        NodeKey::Schema { schema, .. } => NodeKey::Schema {
            conn: "pg".into(),
            schema,
        },
        NodeKey::Relation { rel, .. } => NodeKey::Relation {
            conn: "pg".into(),
            rel,
        },
        other @ (NodeKey::Connection(_)
        | NodeKey::Kind { .. }
        | NodeKey::RelationGroup { .. }
        | NodeKey::SchemaGroup { .. }
        | NodeKey::Objects(_)
        | NodeKey::KeyFolder { .. }) => other,
    };
    // pg mirrors lite: main and Album expanded, their objects loaded.
    for key in [
        NodeKey::Connection("pg".into()),
        pg(schema("main")),
        pg(rel("main", "Album")),
    ] {
        let r = t.toggle(&key).unwrap();
        let data = fixture_for(&r.query);
        answer(&mut t, r, Ok(data));
    }
    for r in t.objects_to_load() {
        let data = fixture_for(&r.query);
        answer(&mut t, r, Ok(data));
    }
    assert!(t.relation_objects("pg", &album()).is_some());
    let reqs = t.refresh_connection("lite");
    assert!(reqs.iter().all(|r| r.conn == "lite"), "only lite reloads");
    assert!(t.relation_objects("lite", &album()).is_none());
    assert!(t.schema_objects("lite", "main").is_none());
    let kept = (
        t.relation_objects("pg", &album())
            .map(|o| o.constraints().len()),
        t.schema_objects("pg", "main").map(|o| o.routines().len()),
    );
    assert_eq!(kept, (Some(2), Some(2)), "pg's objects stay");
    assert!(t.objects_to_load().is_empty(), "nor are pg's asked again");
}

#[test]
fn object_details_name_index_flags_and_constraint_kinds() {
    let index = |name: &str, unique, primary, partial, method: Option<&str>| {
        IndexInfo::new(
            album(),
            name,
            strings(&["AlbumId"]),
            unique,
            primary,
            partial,
            method.map(str::to_owned),
        )
    };
    let artist = RelationRef::new("main", "Artist");
    let objects = CatalogData::RelationObjects(RelationObjects::new(
        vec![
            index("album_pkey", true, true, false, Some("btree")),
            index("album_recent", false, false, true, None),
            index("album_once", true, false, true, Some("hash")),
        ],
        vec![
            ConstraintInfo::new(album(), None, ConstraintKind::Check, vec![], None),
            ConstraintInfo::new(album(), None, ConstraintKind::Unique, vec![], None),
            ConstraintInfo::new(
                album(),
                Some("album_artist".into()),
                ConstraintKind::ForeignKey,
                strings(&["ArtistId"]),
                Some(artist),
            ),
            key(None, ConstraintKind::Exclusion, &["AlbumId"]),
        ],
        vec![],
    ));
    let mut t = with_objects(Ok(objects), Ok(main_objects(false)));
    for g in [ObjectGroup::Keys, ObjectGroup::Indexes, ObjectGroup::Checks] {
        t.toggle(&rgroup(g));
    }
    let details: Vec<(String, String)> = t
        .rows("lite", 1, "")
        .iter()
        .filter_map(|r| match &r.kind {
            RowKind::Object { detail, .. } => Some((r.label.clone(), detail.clone())),
            RowKind::Schema
            | RowKind::Kind(_)
            | RowKind::Relation(_)
            | RowKind::Column { .. }
            | RowKind::Group(_)
            | RowKind::Loading
            | RowKind::Failed { .. }
            | RowKind::Truncated
            | RowKind::Empty
            | RowKind::KeyFolder { .. }
            | RowKind::Key(_)
            | RowKind::LoadMore => None,
        })
        .collect();
    let want: Vec<(String, String)> = [
        ("UNIQUE", "unique"),
        ("album_artist", "foreign key"),
        ("EXCLUDE (AlbumId)", "exclusion"),
        ("album_pkey", "primary btree"),
        ("album_recent", "partial"),
        ("album_once", "unique partial hash"),
        ("CHECK", "check"),
    ]
    .iter()
    .map(|(l, d)| ((*l).to_owned(), (*d).to_owned()))
    .collect();
    assert_eq!(details, want);
}

#[test]
fn the_objects_truncated_row_follows_the_schema_filter() {
    let t = with_objects(Ok(album_objects()), Ok(main_objects(true)));
    let capped = "  lite.main/objects:truncated";
    assert!(!has(&shape(&t, "zzz"), capped), "a missed schema hides it");
    assert!(has(&shape(&t, "main"), capped), "a matched schema shows it");
}

#[test]
fn schema_objects_follow_the_empty_relations_row() {
    let mut t = CatalogTree::new();
    let r = t.toggle(&conn()).unwrap();
    answer(&mut t, r, Ok(schemas(&["main"])));
    let r = t.toggle(&schema("main")).unwrap();
    answer(&mut t, r, Ok(rels(&[], false)));
    for r in t.objects_to_load() {
        answer(&mut t, r, Ok(main_objects(false)));
    }
    assert_eq!(
        shape(&t, ""),
        [
            "lite.main",
            "  lite.main:empty",
            "  lite.main/types",
            "  lite.main/routines",
        ]
    );
}

#[test]
fn surrealdb_selects_name_the_table_alone_and_switch_database_only_when_needed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        "dbento = 1\n[connections.sr]\nengine = \"surrealdb\"\nhost = \"h\"\n\
         namespace = \"app\"\ndatabase = \"main\"\n\n[connections.lite]\nengine = \"sqlite\"\n\
         path = \"l.db\"\n",
    )
    .unwrap();
    let ws = dbento_core::Workspace::open(dir.path()).unwrap();
    let sr = ws.declared("sr").unwrap();
    assert_eq!(
        select_sql(Engine::Surrealdb, &RelationRef::new("main", "t")),
        "SELECT * FROM `t`"
    );
    assert_eq!(
        select_text(sr, &RelationRef::new("main", "order lines")),
        "SELECT * FROM `order lines`"
    );
    assert_eq!(
        select_text(sr, &RelationRef::new("other", "t`x")),
        "USE DB `other`;\nSELECT * FROM `t\\`x`;"
    );
    assert_eq!(
        select_text(
            ws.declared("lite").unwrap(),
            &RelationRef::new("main", "Album")
        ),
        "SELECT *\nFROM main.Album"
    );
}

#[test]
fn mongodb_opens_a_collection_with_find_and_names_another_database() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        "dbento = 1\n[connections.mg]\nengine = \"mongodb\"\nuri = \"mongodb://h/shop\"\n",
    )
    .unwrap();
    let ws = dbento_core::Workspace::open(dir.path()).unwrap();
    let mg = ws.declared("mg").unwrap();
    assert_eq!(
        select_sql(Engine::Mongodb, &RelationRef::new("shop", "orders")),
        "db.getCollection(\"orders\").find()"
    );
    assert_eq!(
        select_text(mg, &RelationRef::new("shop", "order lines")),
        "db.getCollection(\"order lines\").find()"
    );
    assert_eq!(
        select_text(mg, &RelationRef::new("logs", "a\"b")),
        "db.getSiblingDB(\"logs\").getCollection(\"a\\\"b\").find()"
    );
}
