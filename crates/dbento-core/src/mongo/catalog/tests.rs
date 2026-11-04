use bson::{Bson, doc};

use super::*;
use crate::introspect::RelationKind;

#[test]
fn hidden_databases_show_only_when_the_uri_names_them() {
    let names = || {
        ["local", "shop", "admin", "config"]
            .map(str::to_owned)
            .to_vec()
    };
    let shown = |db| {
        visible_schemas(names(), db)
            .iter()
            .map(|s| s.name().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(shown("shop"), ["shop"]);
    assert_eq!(shown("admin"), ["admin", "shop"]);
}

#[test]
fn relations_map_their_type_and_hide_system_collections() {
    let entries = [
        doc! { "name": "v", "type": "view" },
        doc! { "name": "ts", "type": "timeseries" },
        doc! { "name": "system.views", "type": "collection" },
        doc! { "name": "a", "type": "collection" },
    ];
    let list = relation_list(&entries);
    let got: Vec<_> = list.items().iter().map(|r| (r.name(), r.kind())).collect();
    assert_eq!(
        got,
        [
            ("a", RelationKind::Table),
            ("ts", RelationKind::Table),
            ("v", RelationKind::View)
        ]
    );
}

#[test]
fn columns_are_inferred_by_frequency_with_id_first() {
    let documents = [
        doc! { "b": "x", "_id": 1, "a": 1 },
        doc! { "_id": 2, "a": "y", "c": 1 },
        doc! { "_id": 3, "a": Bson::Null, "c": 2 },
    ];
    let columns = inferred_columns(&documents);
    let got: Vec<_> = columns
        .iter()
        .map(|c| (c.name(), c.type_name(), c.nullable(), c.primary_key()))
        .collect();
    assert_eq!(
        got,
        [
            ("_id", "int", false, Some(1)),
            ("a", "int | null | string", true, None),
            ("c", "int", true, None),
            ("b", "string", true, None),
        ]
    );
    assert!(inferred_columns(&[]).is_empty());
}

#[test]
fn an_index_reads_its_keys_uniqueness_and_method() {
    let rel = RelationRef::new("app", "t");
    let index = |entry| index_info(&rel, &entry).unwrap();
    let id = index(doc! { "v": 2, "key": { "_id": 1 }, "name": "_id_" });
    assert!(id.primary() && id.unique() && !id.partial() && id.method().is_none());
    let compound = index(doc! { "key": { "n.x": -1, "y": 1 }, "name": "c", "unique": true });
    assert_eq!(compound.columns(), ["n.x", "y"]);
    assert!(compound.unique() && !compound.primary());
    let partial = index(
        doc! { "key": { "z": 1 }, "name": "p", "partialFilterExpression": { "z": { "$gt": 1 } } },
    );
    assert!(partial.partial());
    assert_eq!(
        index(doc! { "key": { "h": "hashed" }, "name": "h" }).method(),
        Some("hashed")
    );
    assert_eq!(
        index(doc! { "key": { "p": "2dsphere" }, "name": "g" }).method(),
        Some("2dsphere")
    );
    let text = index(
        doc! { "key": { "_fts": "text", "_ftsx": 1 }, "name": "s", "weights": { "s": 1, "u": 1 } },
    );
    assert_eq!(text.columns(), ["s", "u"]);
    assert_eq!(text.method(), Some("text"));
    assert!(index_info(&rel, &doc! { "name": "no key" }).is_none());
}

#[test]
fn a_compound_text_index_keeps_its_plain_keys() {
    let rel = RelationRef::new("app", "t");
    let entry =
        doc! { "key": { "a": 1, "_fts": "text", "_ftsx": 1 }, "name": "c", "weights": { "s": 1 } };
    assert_eq!(index_info(&rel, &entry).unwrap().columns(), ["a", "s"]);
}

#[test]
fn sampled_columns_are_marked_inferred() {
    let columns = inferred_columns(&[doc! { "_id": 1, "a": 2 }]);
    assert!(
        !columns.is_empty() && columns.iter().all(ColumnMeta::inferred),
        "{columns:?}"
    );
    assert!(!ColumnMeta::new("x", "int", true, None, None).inferred());
}
