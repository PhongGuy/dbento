//! PartiQL completion: quoted tables without the region, attributes, keywords and functions.
use dbento_app::catalog::{CatalogData, CatalogQuery, CatalogReply};
use dbento_app::completion::{Completion, complete};
use dbento_app::schema_tree::CatalogTree;
use dbento_core::{
    ColumnMeta, Engine, Relation, RelationKind, RelationList, RelationRef, SchemaInfo,
};

fn put(t: &mut CatalogTree, query: CatalogQuery, data: CatalogData) {
    let generation = t.generation();
    t.apply(
        "dy",
        CatalogReply {
            generation,
            query,
            result: Ok(data),
        },
    );
}
/// `dy`: region eu-west-1 holds orders, "Order Items" and events; orders has pk, sk, status.
fn dy_tree() -> CatalogTree {
    let mut t = CatalogTree::new();
    put(
        &mut t,
        CatalogQuery::Schemas,
        CatalogData::Schemas(vec![SchemaInfo::new("eu-west-1")]),
    );
    let rels = ["orders", "Order Items", "events"]
        .map(|n| Relation::new(n, RelationKind::Table))
        .to_vec();
    put(
        &mut t,
        CatalogQuery::Relations {
            schema: "eu-west-1".into(),
        },
        CatalogData::Relations(RelationList::new(rels, false)),
    );
    let cols = vec![
        ColumnMeta::new("pk", "S", false, Some(1), None),
        ColumnMeta::new("sk", "N", false, Some(2), None),
        ColumnMeta::new("status", "S", true, None, None),
    ];
    put(
        &mut t,
        CatalogQuery::Columns {
            rel: RelationRef::new("eu-west-1", "orders"),
        },
        CatalogData::Columns(cols),
    );
    t
}
fn at_end(sql: &str, default_schema: Option<&str>) -> Completion {
    complete(
        Engine::Dynamodb,
        sql,
        sql.len(),
        &dy_tree(),
        "dy",
        default_schema,
    )
}
fn pairs(c: &Completion) -> Vec<(&str, &str)> {
    c.items
        .iter()
        .map(|i| (i.label.as_str(), i.insert.as_str()))
        .collect()
}
fn labels(c: &Completion) -> Vec<&str> {
    c.items.iter().map(|i| i.label.as_str()).collect()
}

#[test]
fn tables_insert_quoted_and_the_region_is_never_offered() {
    let c = at_end("SELECT * FROM Ord", Some("eu-west-1"));
    assert_eq!(
        pairs(&c),
        [("Order Items", "\"Order Items\""), ("orders", "\"orders\"")]
    );
    let c = at_end("SELECT * FROM e", Some("eu-west-1"));
    assert_eq!(
        labels(&c),
        ["events"],
        "the region eu-west-1 is no candidate"
    );
}

#[test]
fn a_region_from_the_environment_still_completes_tables() {
    let c = at_end("SELECT * FROM ev", None);
    assert_eq!(labels(&c), ["events"]);
}

#[test]
fn where_offers_the_tables_attributes_and_partiql_words() {
    let c = at_end("SELECT * FROM \"orders\" WHERE st", Some("eu-west-1"));
    assert_eq!(labels(&c).first(), Some(&"status"));
    let c = at_end("SELECT * FROM \"orders\" WHERE begins", Some("eu-west-1"));
    assert!(labels(&c).contains(&"begins_with"), "{:?}", labels(&c));
    let c = at_end(
        "SELECT * FROM \"orders\" WHERE ATTRIBUTE_N",
        Some("eu-west-1"),
    );
    assert!(
        labels(&c).contains(&"attribute_not_exists"),
        "{:?}",
        labels(&c)
    );
    let c = at_end(
        "SELECT * FROM \"orders\" WHERE pk IS MISS",
        Some("eu-west-1"),
    );
    assert!(labels(&c).contains(&"MISSING"), "{:?}", labels(&c));
}

#[test]
fn a_caret_inside_a_bracket_list_still_completes() {
    // Read as a quoted identifier, the open `[` would swallow the caret and offer nothing.
    let c = at_end(
        "SELECT * FROM \"orders\" WHERE status IN ['a', st",
        Some("eu-west-1"),
    );
    assert_eq!(labels(&c).first(), Some(&"status"));
}
