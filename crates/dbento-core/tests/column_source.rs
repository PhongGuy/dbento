//! Each result column knows the table column it reads, and a session maps those back to a
//! relation's column names: an alias resolves, a computed or foreign column does not.
mod support;
use dbento_core::{ColumnInfo, RelationRef, Session, SessionOptions, named_source_columns};
use mysql_async::prelude::*;
use support::{my_fixtures, pg_fixture, sqlite_fixture};

const SQLITE_SETUP: &str = "CREATE TABLE t(a INTEGER, b TEXT); CREATE TABLE u(c INTEGER);
INSERT INTO t VALUES (1, 'x'); INSERT INTO u VALUES (5);";
const PG_SETUP: &str = "CREATE TABLE t(a int, b text); CREATE TABLE u(c int);
INSERT INTO t VALUES (1, 'x'); INSERT INTO u VALUES (5);";
const MY_SETUP: &str =
    "CREATE TABLE t(a int, b text); CREATE TABLE u(c int); CREATE TABLE T(a int);
INSERT INTO t VALUES (1, 'x'); INSERT INTO u VALUES (5); INSERT INTO T VALUES (2);";

/// The columns `sql` reports, read and dropped before the session is reused.
async fn columns(s: &mut Session, sql: &str) -> Vec<ColumnInfo> {
    let mut script = s.script(sql);
    let mut stream = script
        .next()
        .await
        .unwrap()
        .unwrap()
        .execute(Default::default())
        .await
        .unwrap();
    let cols = stream.columns().to_vec();
    while stream.next_batch().await.unwrap().is_some() {}
    cols
}

fn names(v: &[Option<&str>]) -> Vec<Option<String>> {
    v.iter().map(|n| n.map(str::to_owned)).collect()
}

async fn check(s: &mut Session, schema: &str) {
    let t = RelationRef::new(schema, "t");
    let cases: [(&str, &[Option<&str>]); 4] = [
        ("SELECT a AS n, b FROM t", &[Some("a"), Some("b")]),
        ("SELECT a + 1 AS b, b AS total FROM t", &[None, Some("b")]),
        ("SELECT 1 AS a, 'x' AS b", &[None, None]),
        ("SELECT t.a, u.c FROM t, u", &[Some("a"), None]),
    ];
    for (sql, want) in cases {
        let cols = columns(s, sql).await;
        let got = s.source_columns(&t, &cols).await.unwrap();
        assert_eq!(got, names(want), "{sql}");
    }
    let star = columns(s, "SELECT * FROM t").await;
    assert_eq!(
        s.source_columns(&t, &star).await.unwrap(),
        names(&[Some("a"), Some("b")])
    );
    let other = RelationRef::new(schema, "u");
    assert_eq!(
        s.source_columns(&other, &star).await.unwrap(),
        names(&[None, None]),
        "t's columns are not u's"
    );
}

#[tokio::test]
async fn sqlite_columns_map_to_their_source_table_column() {
    let fx = sqlite_fixture(SQLITE_SETUP, "");
    let mut s = fx.session(SessionOptions::default()).await;
    let cols = columns(&mut s, "SELECT a AS n FROM t").await;
    assert!(cols[0].source().is_some(), "{cols:?}");
    assert!(columns(&mut s, "SELECT 1").await[0].source().is_none());
    let t = RelationRef::new("main", "T");
    assert_eq!(
        named_source_columns(&t, &cols),
        Some(names(&[Some("a")])),
        "SQLite names need no session, and match without case"
    );
    check(&mut s, "main").await;
    s.close().await.unwrap();
}

#[tokio::test]
async fn postgres_columns_map_to_their_source_table_column() {
    let fx = pg_fixture("").await;
    fx.db.admin().await.batch_execute(PG_SETUP).await.unwrap();
    let mut s = fx.session(SessionOptions::default()).await;
    let cols = columns(&mut s, "SELECT a AS n FROM t").await;
    assert!(cols[0].source().is_some(), "{cols:?}");
    assert!(columns(&mut s, "SELECT 1").await[0].source().is_none());
    let t = RelationRef::new("public", "t");
    assert_eq!(
        named_source_columns(&t, &cols),
        None,
        "an OID needs the catalog"
    );
    check(&mut s, "public").await;
    s.close().await.unwrap();
}

#[tokio::test]
async fn mysql_columns_map_to_their_source_table_column() {
    for (label, fx) in my_fixtures("").await {
        let mut c = fx.db.admin().await;
        c.query_drop(MY_SETUP).await.unwrap();
        c.disconnect().await.unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        check(&mut s, fx.db.database()).await;
        let upper = columns(&mut s, "SELECT a FROM T").await;
        let t = RelationRef::new(fx.db.database(), "t");
        assert_eq!(
            s.source_columns(&t, &upper).await.unwrap(),
            names(&[None]),
            "{label}: T is not t"
        );
    }
}
