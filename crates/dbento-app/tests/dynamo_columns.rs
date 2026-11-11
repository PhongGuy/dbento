//! DynamoDB columns in the explorer: keys alone when sampling fails, and the inferred marker.
use dbento_app::catalog::{CatalogData, CatalogQuery, CatalogReply, columns_answer};
use dbento_app::schema_tree::{
    CatalogTree, INFERRED_COLUMN_TOOLTIP, LoadRequest, NodeKey, RowKind, column_detail, short_type,
};
use dbento_core::{
    ColumnMeta, IntrospectError, Relation, RelationKind, RelationList, RelationRef, SchemaInfo,
};

const R: &str = "eu-west-1";

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
fn orders() -> NodeKey {
    NodeKey::Relation {
        conn: "dy".into(),
        rel: RelationRef::new(R, "orders"),
    }
}
fn keys() -> Vec<ColumnMeta> {
    vec![
        ColumnMeta::new("pk", "S", false, Some(1), None),
        ColumnMeta::new("sk", "N", false, Some(2), None),
    ]
}
fn err(message: &str) -> IntrospectError {
    IntrospectError::Unexpected {
        message: message.into(),
    }
}
/// dy, its region and orders expanded; orders' columns still loading.
fn expanded() -> (CatalogTree, LoadRequest) {
    let mut t = CatalogTree::new();
    let r = t.toggle(&NodeKey::Connection("dy".into())).unwrap();
    answer(
        &mut t,
        r,
        Ok(CatalogData::Schemas(vec![SchemaInfo::new(R)])),
    );
    let r = t
        .toggle(&NodeKey::Schema {
            conn: "dy".into(),
            schema: R.into(),
        })
        .unwrap();
    let list = RelationList::new(vec![Relation::new("orders", RelationKind::Table)], false);
    answer(&mut t, r, Ok(CatalogData::Relations(list)));
    let r = t.toggle(&orders()).unwrap();
    (t, r)
}
fn arias(t: &CatalogTree) -> Vec<String> {
    t.rows("dy", 1, "").iter().map(|r| r.aria.clone()).collect()
}

#[test]
fn loaded_columns_are_the_answer() {
    match columns_answer(Ok(keys()), None) {
        Ok(CatalogData::Columns(c)) => assert_eq!(c, keys()),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_failed_sample_with_keys_answers_the_keys_and_why() {
    let want = err("no Scan").to_string();
    match columns_answer(Err(err("no Scan")), Some(Ok(keys()))) {
        Ok(CatalogData::KeyColumns { columns, reason }) => {
            assert_eq!((columns, reason), (keys(), want));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_original_error_wins_without_keys() {
    let want = err("no Scan").to_string();
    for keys in [
        None,
        Some(Ok(Vec::new())),
        Some(Err(err("no DescribeTable"))),
    ] {
        match columns_answer(Err(err("no Scan")), keys) {
            Err(e) => assert_eq!(e.to_string(), want),
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn a_failed_sample_shows_the_keys_then_why_and_retry_reloads() {
    let (mut t, r) = expanded();
    let reason = "AccessDeniedException: no Scan".to_owned();
    answer(
        &mut t,
        r,
        Ok(CatalogData::KeyColumns {
            columns: keys(),
            reason,
        }),
    );
    let got = arias(&t);
    let at = |a: &str| {
        got.iter()
            .position(|x| x == a)
            .unwrap_or_else(|| panic!("{a}: {got:?}"))
    };
    assert!(at("dy.eu-west-1.orders.pk") < at("dy.eu-west-1.orders.sk"));
    assert!(at("dy.eu-west-1.orders.sk") < at("dy.eu-west-1.orders:error"));
    let error = t
        .rows("dy", 1, "")
        .into_iter()
        .find(|r| r.aria == "dy.eu-west-1.orders:error")
        .unwrap();
    assert_eq!(
        error.kind,
        RowKind::Failed {
            message: "attributes not sampled: AccessDeniedException: no Scan".into()
        }
    );
    let again = t
        .retry(&orders())
        .expect("a key-only listing can be retried");
    assert_eq!(
        again.query,
        CatalogQuery::Columns {
            rel: RelationRef::new(R, "orders")
        }
    );
    let mut full = keys();
    full.push(ColumnMeta::new("status", "S", true, None, None));
    answer(&mut t, again, Ok(CatalogData::Columns(full)));
    let got = arias(&t);
    assert!(
        got.contains(&"dy.eu-west-1.orders.status".to_owned()),
        "{got:?}"
    );
    assert!(
        !got.contains(&"dy.eu-west-1.orders:error".to_owned()),
        "{got:?}"
    );
}

#[test]
fn a_failed_key_lookup_keeps_the_columns_error() {
    let (mut t, r) = expanded();
    answer(
        &mut t,
        r,
        Err("AccessDeniedException: no DescribeTable".into()),
    );
    let error = t
        .rows("dy", 1, "")
        .into_iter()
        .find(|r| r.aria == "dy.eu-west-1.orders:error")
        .unwrap();
    assert_eq!(
        error.kind,
        RowKind::Failed {
            message: "AccessDeniedException: no DescribeTable".into()
        }
    );
}

#[test]
fn inferred_columns_say_so_and_declared_ones_do_not() {
    let (mut t, r) = expanded();
    let mut cols = keys();
    cols.push(ColumnMeta::new("status", "S", true, None, None).mark_inferred());
    answer(&mut t, r, Ok(CatalogData::Columns(cols)));
    let rows = t.rows("dy", 1, "");
    let kind = |aria: &str| rows.iter().find(|r| r.aria == aria).map(|r| r.kind.clone());
    assert_eq!(
        kind("dy.eu-west-1.orders.status"),
        Some(RowKind::Column {
            type_name: "S".into(),
            primary_key: false,
            inferred: true
        })
    );
    assert_eq!(
        kind("dy.eu-west-1.orders.pk"),
        Some(RowKind::Column {
            type_name: "S".into(),
            primary_key: true,
            inferred: false
        })
    );
    assert_eq!(
        column_detail("S | N", true),
        format!("{} · inferred", short_type("S | N"))
    );
    assert_eq!(column_detail("INTEGER", false), short_type("INTEGER"));
    assert!(INFERRED_COLUMN_TOOLTIP.contains("read capacity"));
}
