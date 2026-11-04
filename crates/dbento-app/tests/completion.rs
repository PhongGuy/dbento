use dbento_app::catalog::{CatalogData, CatalogQuery, CatalogReply};
use dbento_app::completion::{CandidateKind, Completion, MAX_ITEMS, complete};
use dbento_app::schema_tree::{CatalogTree, LoadRequest};
use dbento_core::{
    ColumnMeta, Engine, Relation, RelationKind, RelationList, RelationRef, SchemaInfo,
};

fn put(t: &mut CatalogTree, conn: &str, query: CatalogQuery, data: CatalogData) {
    let generation = t.generation();
    t.apply(
        conn,
        CatalogReply {
            generation,
            query,
            result: Ok(data),
        },
    );
}
fn col(name: &str, ty: &str) -> ColumnMeta {
    ColumnMeta::new(name, ty, true, None, None)
}
fn rels(items: &[(&str, RelationKind)]) -> CatalogData {
    let items = items.iter().map(|(n, k)| Relation::new(*n, *k)).collect();
    CatalogData::Relations(RelationList::new(items, false))
}
fn relations_of(schema: &str) -> CatalogQuery {
    CatalogQuery::Relations {
        schema: schema.into(),
    }
}
/// `lite`: schemas main and aux; main holds Album, Artist and the view "Recent Tracks".
fn lite() -> CatalogTree {
    let mut t = CatalogTree::new();
    let schemas = vec![SchemaInfo::new("main"), SchemaInfo::new("aux")];
    put(
        &mut t,
        "lite",
        CatalogQuery::Schemas,
        CatalogData::Schemas(schemas),
    );
    put(
        &mut t,
        "lite",
        relations_of("main"),
        rels(&[
            ("Album", RelationKind::Table),
            ("Artist", RelationKind::Table),
            ("Recent Tracks", RelationKind::View),
        ]),
    );
    put(
        &mut t,
        "lite",
        relations_of("aux"),
        rels(&[("Notes", RelationKind::Table)]),
    );
    let album = vec![
        col("AlbumId", "INTEGER"),
        col("Title", "NVARCHAR(160)"),
        col("ArtistId", "INTEGER"),
    ];
    put(
        &mut t,
        "lite",
        CatalogQuery::Columns {
            rel: RelationRef::new("main", "Album"),
        },
        CatalogData::Columns(album),
    );
    let artist = vec![col("ArtistId", "INTEGER"), col("Name", "TEXT")];
    put(
        &mut t,
        "lite",
        CatalogQuery::Columns {
            rel: RelationRef::new("main", "Artist"),
        },
        CatalogData::Columns(artist),
    );
    t
}
fn at_end(tree: &CatalogTree, sql: &str) -> Completion {
    complete(Engine::Sqlite, sql, sql.len(), tree, "lite", Some("main"))
}
fn labels(c: &Completion) -> Vec<&str> {
    c.items.iter().map(|i| i.label.as_str()).collect()
}

#[test]
fn a_where_prefix_offers_the_from_relations_columns() {
    let c = at_end(&lite(), "SELECT * FROM Album WHERE Ti");
    assert_eq!(labels(&c), ["Title"]);
    assert_eq!(
        (c.items[0].kind, c.items[0].detail.as_str()),
        (CandidateKind::Column, "NVARCHAR(160)")
    );
    assert_eq!(c.replace, 26..28);
}

#[test]
fn after_from_only_relations_and_schemas_are_offered() {
    let c = at_end(&lite(), "SELECT * FROM A");
    assert_eq!(labels(&c), ["Album", "Artist", "aux"]);
    let kinds: Vec<_> = c.items.iter().map(|i| i.kind).collect();
    assert_eq!(
        kinds,
        [
            CandidateKind::Relation,
            CandidateKind::Relation,
            CandidateKind::Schema
        ]
    );
    assert_eq!(
        (c.items[0].detail.as_str(), c.items[2].detail.as_str()),
        ("table", "schema")
    );
}

#[test]
fn keywords_follow_the_case_of_the_prefix() {
    assert_eq!(labels(&at_end(&lite(), "sel")), ["select"]);
    assert_eq!(labels(&at_end(&lite(), "SEL")), ["SELECT"]);
    assert_eq!(labels(&at_end(&lite(), "Sel")), ["SELECT"]);
    let c = at_end(&lite(), "sel");
    let i = &c.items[0];
    assert_eq!(
        (i.kind, i.insert.as_str(), i.detail.as_str()),
        (CandidateKind::Keyword, "select", "keyword")
    );
}

#[test]
fn columns_come_from_every_from_and_join_relation_once() {
    let sql = "SELECT A FROM Album JOIN Artist USING (ArtistId)";
    let c = complete(Engine::Sqlite, sql, 8, &lite(), "lite", Some("main"));
    let l = labels(&c);
    assert_eq!(&l[..5], ["AlbumId", "ArtistId", "Album", "Artist", "aux"]);
    assert_eq!(l.iter().filter(|x| **x == "ArtistId").count(), 1);
    assert!(
        l[5..]
            .iter()
            .all(|k| k.chars().all(|ch| ch.is_ascii_uppercase())),
        "then keywords: {l:?}"
    );
    assert_eq!(c.replace, 7..8);
}

#[test]
fn a_schema_qualifier_lists_its_relations() {
    let c = at_end(&lite(), "SELECT * FROM aux.");
    assert_eq!(labels(&c), ["Notes"]);
    assert_eq!(c.replace, 18..18);
    assert_eq!(labels(&at_end(&lite(), "SELECT * FROM AUX.n")), ["Notes"]);
}

#[test]
fn a_relation_qualifier_lists_its_columns() {
    let c = complete(
        Engine::Sqlite,
        "SELECT Album. FROM Album",
        13,
        &lite(),
        "lite",
        Some("main"),
    );
    assert_eq!(labels(&c), ["AlbumId", "ArtistId", "Title"]);
}

#[test]
fn unloaded_levels_are_wanted_while_known_items_return() {
    let c = at_end(&CatalogTree::new(), "SELECT * FROM Album WHERE Al");
    assert_eq!(labels(&c), ["ALL", "ALTER"]);
    assert!(c.wants.contains(&CatalogQuery::Schemas), "{:?}", c.wants);
    assert!(c.wants.contains(&relations_of("main")), "{:?}", c.wants);
    let mut t = CatalogTree::new();
    put(
        &mut t,
        "lite",
        relations_of("main"),
        rels(&[("Album", RelationKind::Table)]),
    );
    let c = at_end(&t, "SELECT * FROM album WHERE Al");
    let album = CatalogQuery::Columns {
        rel: RelationRef::new("main", "Album"),
    };
    assert!(c.wants.contains(&album), "{:?}", c.wants);
}

#[test]
fn postgres_defaults_to_public_and_matches_names_without_case() {
    let mut t = CatalogTree::new();
    put(
        &mut t,
        "pg",
        relations_of("public"),
        rels(&[("album", RelationKind::Table)]),
    );
    let sql = "SELECT * FROM Album WHERE ti";
    let columns = CatalogQuery::Columns {
        rel: RelationRef::new("public", "album"),
    };
    let c = complete(Engine::Postgres, sql, sql.len(), &t, "pg", Some("public"));
    assert!(c.wants.contains(&columns), "{:?}", c.wants);
    put(
        &mut t,
        "pg",
        columns,
        CatalogData::Columns(vec![col("title", "text")]),
    );
    let c = complete(Engine::Postgres, sql, sql.len(), &t, "pg", Some("public"));
    assert_eq!(labels(&c), ["title"]);
}

#[test]
fn nothing_completes_inside_strings_comments_or_quoted_names() {
    for (engine, sql) in [
        (Engine::Sqlite, "SELECT 'Al"),
        (Engine::Sqlite, "SELECT 1 -- Al"),
        (Engine::Sqlite, "SELECT \"Al"),
        (Engine::Sqlite, "/* Al"),
        (Engine::Postgres, "SELECT $$Al"),
    ] {
        let default = Some(if engine == Engine::Postgres {
            "public"
        } else {
            "main"
        });
        let c = complete(engine, sql, sql.len(), &lite(), "lite", default);
        assert!(c.items.is_empty(), "{sql}: {:?}", labels(&c));
    }
    assert!(
        at_end(&lite(), "SELECT 1 ").items.is_empty(),
        "no prefix opens nothing"
    );
    assert!(
        at_end(&lite(), "SELECT 1").items.is_empty(),
        "a number is no prefix"
    );
}

#[test]
fn only_the_current_statement_names_relations() {
    let sql = "SELECT Na FROM Album; SELECT 1 FROM Artist";
    assert!(
        complete(Engine::Sqlite, sql, 9, &lite(), "lite", Some("main"))
            .items
            .is_empty()
    );
    let sql = "SELECT 1 FROM Album; SELECT Na FROM Artist";
    let at = sql.find("Na").unwrap() + 2;
    assert_eq!(
        labels(&complete(
            Engine::Sqlite,
            sql,
            at,
            &lite(),
            "lite",
            Some("main")
        )),
        ["Name"]
    );
}

#[test]
fn names_that_need_quotes_insert_quoted() {
    let c = at_end(&lite(), "SELECT * FROM Re");
    assert_eq!(labels(&c), ["Recent Tracks"]);
    assert_eq!(
        (c.items[0].insert.as_str(), c.items[0].detail.as_str()),
        ("\"Recent Tracks\"", "view")
    );
    assert_eq!(
        at_end(&lite(), "SELECT * FROM Alb").items[0].insert,
        "Album"
    );
}

#[test]
fn at_most_max_items_return() {
    let mut t = CatalogTree::new();
    let items = (0..150)
        .map(|i| Relation::new(format!("t{i:03}"), RelationKind::Table))
        .collect();
    put(
        &mut t,
        "lite",
        relations_of("main"),
        CatalogData::Relations(RelationList::new(items, false)),
    );
    let c = at_end(&t, "SELECT * FROM t");
    assert_eq!(
        (c.items.len(), c.items[0].label.as_str()),
        (MAX_ITEMS, "t000")
    );
}

#[test]
fn ensure_loads_a_level_once_without_expanding_it() {
    let mut t = CatalogTree::new();
    let want = LoadRequest {
        conn: "lite".into(),
        generation: 0,
        query: CatalogQuery::Schemas,
    };
    assert_eq!(t.ensure("lite", &CatalogQuery::Schemas), Some(want));
    assert_eq!(
        t.ensure("lite", &CatalogQuery::Schemas),
        None,
        "already loading"
    );
    assert!(t.rows("lite", 0, "").is_empty(), "nothing expanded");
    let failed = CatalogReply {
        generation: 0,
        query: CatalogQuery::Schemas,
        result: Err("down".into()),
    };
    t.apply("lite", failed);
    assert_eq!(
        t.ensure("lite", &CatalogQuery::Schemas),
        None,
        "Retry or Refresh reloads a failure"
    );
    t.refresh();
    assert_eq!(
        t.ensure("lite", &CatalogQuery::Schemas)
            .map(|r| r.generation),
        Some(1)
    );
}

/// `shop`: Postgres schema public with orders(id, status) and customers(id, name).
fn shop() -> CatalogTree {
    let mut t = CatalogTree::new();
    put(
        &mut t,
        "pg",
        CatalogQuery::Schemas,
        CatalogData::Schemas(vec![SchemaInfo::new("public")]),
    );
    put(
        &mut t,
        "pg",
        relations_of("public"),
        rels(&[
            ("orders", RelationKind::Table),
            ("customers", RelationKind::Table),
        ]),
    );
    for (rel, cols) in [
        ("orders", vec![col("id", "int4"), col("status", "text")]),
        ("customers", vec![col("id", "int4"), col("name", "text")]),
    ] {
        put(
            &mut t,
            "pg",
            CatalogQuery::Columns {
                rel: RelationRef::new("public", rel),
            },
            CatalogData::Columns(cols),
        );
    }
    t
}
fn pg_at(sql: &str, caret: &str) -> Vec<String> {
    let off = sql.find(caret).expect("caret marker") + caret.len();
    let c = complete(Engine::Postgres, sql, off, &shop(), "pg", Some("public"));
    c.items.into_iter().map(|i| i.label).collect()
}

#[test]
fn an_alias_qualifies_completion_to_its_tables_columns() {
    let sql = "SELECT * FROM orders o WHERE o.st";
    assert_eq!(pg_at(sql, "o.st"), ["status"]);
    let sql = "SELECT * FROM orders AS o WHERE o.st";
    assert_eq!(pg_at(sql, "o.st"), ["status"]);
    let sql = "SELECT * FROM customers c JOIN orders AS o ON o.id = c.id WHERE o.";
    assert_eq!(pg_at(sql, "WHERE o."), ["id", "status"]);
}

#[test]
fn an_alias_of_a_schema_qualified_table_resolves() {
    let sql = "SELECT * FROM public.orders o WHERE o.st";
    assert_eq!(pg_at(sql, "o.st"), ["status"]);
}

#[test]
fn several_aliases_complete_the_one_named() {
    let sql = "SELECT c.n, o.s FROM orders o, customers c";
    assert_eq!(pg_at(sql, "c.n"), ["name"]);
    assert_eq!(pg_at(sql, "o.s"), ["status"]);
    let sql = "SELECT c. FROM orders o JOIN customers AS c ON true";
    assert_eq!(pg_at(sql, "c."), ["id", "name"]);
}

#[test]
fn an_alias_declared_later_in_the_text_resolves() {
    let sql = "SELECT o. FROM orders o";
    assert_eq!(pg_at(sql, "o."), ["id", "status"]);
}

#[test]
fn a_table_name_qualifier_still_completes_beside_aliases() {
    let sql = "SELECT orders.st FROM orders JOIN customers c ON true";
    assert_eq!(pg_at(sql, "orders.st"), ["status"]);
}

#[test]
fn a_quoted_alias_resolves() {
    let sql = r#"SELECT "O".st FROM orders "O""#;
    assert_eq!(pg_at(sql, r#""O".st"#), ["status"]);
    let sql = r#"SELECT "my o".st FROM orders AS "my o""#;
    assert_eq!(pg_at(sql, r#""my o".st"#), ["status"]);
}

#[test]
fn an_alias_does_not_disturb_keyword_or_table_completion() {
    let sql = "SELECT * FROM orders o WH";
    assert_eq!(pg_at(sql, "o WH"), ["WHEN", "WHERE"]);
    let sql = "SELECT * FROM orders o JOIN cu";
    assert_eq!(pg_at(sql, "JOIN cu"), ["customers"]);
    let sql = "SELECT * FROM orders o WHERE st";
    assert_eq!(pg_at(sql, "WHERE st"), ["status"]);
}

/// `my`: schemas shop and other; shop holds orders and customers.
fn shop_my() -> CatalogTree {
    let mut t = CatalogTree::new();
    let schemas = vec![SchemaInfo::new("shop"), SchemaInfo::new("other")];
    put(
        &mut t,
        "my",
        CatalogQuery::Schemas,
        CatalogData::Schemas(schemas),
    );
    put(
        &mut t,
        "my",
        relations_of("shop"),
        rels(&[
            ("orders", RelationKind::Table),
            ("customers", RelationKind::Table),
        ]),
    );
    t
}
fn my(sql: &str, default: Option<&str>) -> Vec<String> {
    let c = complete(Engine::Mysql, sql, sql.len(), &shop_my(), "my", default);
    labels(&c).into_iter().map(str::to_owned).collect()
}
fn offers(got: &[String], label: &str) -> bool {
    got.iter().any(|l| l == label)
}

#[test]
fn mysql_completes_unqualified_names_from_the_connections_database() {
    assert!(offers(&my("SELECT * FROM ord", Some("shop")), "orders"));
    let none = my("SELECT * FROM ord", None);
    assert!(!offers(&none, "orders"), "{none:?}");
    // Without a database there is no default schema to load relations from.
    let sql = "SELECT * FROM ord";
    let c = complete(Engine::Mysql, sql, sql.len(), &shop_my(), "my", None);
    assert!(c.wants.is_empty(), "{:?}", c.wants);
    assert!(offers(&my("SELECT * FROM sh", None), "shop"));
    assert!(offers(&my("SELECT * FROM `shop`.ord", None), "orders"));
}

#[test]
fn mysql_comments_and_strings_lex_as_the_server_reads_them() {
    let shop = Some("shop");
    for inside in [
        "SELECT 1 # FROM ord",
        "SELECT 1 -- FROM ord",
        "SELECT 1 --\tFROM ord",
    ] {
        assert!(my(inside, shop).is_empty(), "{inside}");
    }
    for after in [
        "SELECT 2 --FROM ord",
        "SELECT 'it\\'s' FROM ord",
        "SELECT \"a\\\"b\" FROM ord",
        "/* a /* b */ SELECT * FROM ord",
        "SELECT a[ FROM ord",
    ] {
        assert!(
            offers(&my(after, shop), "orders"),
            "{after}: {:?}",
            my(after, shop)
        );
    }
}

/// `duck`: schema main holding Album, as DuckDB's catalog lists it.
fn duck_tree() -> CatalogTree {
    let mut t = CatalogTree::new();
    put(
        &mut t,
        "duck",
        CatalogQuery::Schemas,
        CatalogData::Schemas(vec![SchemaInfo::new("main")]),
    );
    put(
        &mut t,
        "duck",
        relations_of("main"),
        rels(&[("Album", RelationKind::Table)]),
    );
    t
}
fn duck(sql: &str) -> Vec<String> {
    let c = complete(
        Engine::Duckdb,
        sql,
        sql.len(),
        &duck_tree(),
        "duck",
        Some("main"),
    );
    labels(&c).into_iter().map(str::to_owned).collect()
}

#[test]
fn duckdb_reads_strings_and_comments_as_postgres_does() {
    for inside in [
        "SELECT 'Al",
        "SELECT 1 -- Al",
        "SELECT \"Al",
        "SELECT $$Al",
        "SELECT $q$ x $q$ || $q$Al",
        "/* a /* b */ Al",
    ] {
        let got = duck(inside);
        assert!(got.is_empty(), "{inside}: {got:?}");
    }
    // A bracket opens a DuckDB list, not a quoted name, so the word after it completes.
    let after_bracket = duck("SELECT [Tr");
    assert!(offers(&after_bracket, "TRUE"), "{after_bracket:?}");
    assert!(offers(&duck("SELECT * FROM Al"), "Album"));
}
