//! Foreign keys in both directions on SQLite, and the filter a reference link browses with.
use crate::support::{Fixture, sqlite_fixture};
use dbento_core::{
    Browse, EditValue, Engine, FkDirection, ForeignKey, RelationRef, SessionOptions, browse_sql,
    key_filter,
};

const SETUP: &str = "
CREATE TABLE artist(id INTEGER PRIMARY KEY, name TEXT);
CREATE TABLE \"Album\"(\"AlbumId\" INTEGER PRIMARY KEY, title TEXT,
  artist_id INTEGER REFERENCES ARTIST(id));
CREATE TABLE pair(a INTEGER, b TEXT, PRIMARY KEY (a, b));
CREATE TABLE child(id INTEGER PRIMARY KEY, pb TEXT, pa INTEGER,
  parent_id INTEGER REFERENCES child, FOREIGN KEY (pa, pb) REFERENCES pair(a, b));
CREATE TABLE implicit(id INTEGER PRIMARY KEY, art INTEGER REFERENCES artist);
CREATE TABLE dangling(x REFERENCES gone, y REFERENCES gone(id));
CREATE TABLE lonely(x);
INSERT INTO artist VALUES (1, 'a'), (2, 'b');
INSERT INTO \"Album\" VALUES (10, 'x', 1), (11, 'y', 1), (12, 'z', 2);
";

fn rel(name: &str) -> RelationRef {
    RelationRef::new("main", name)
}
fn cols(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}
fn fk(table: &str, c: &[&str], referenced: &str, rc: &[&str], d: FkDirection) -> ForeignKey {
    ForeignKey::new(None, rel(table), cols(c), rel(referenced), cols(rc), d)
}
async fn keys(fx: &Fixture, name: &str) -> Vec<ForeignKey> {
    let mut s = fx.session(SessionOptions::default()).await;
    let k = s.foreign_keys(&rel(name)).await.unwrap();
    s.close().await.unwrap();
    k
}

#[tokio::test]
async fn outgoing_keys_resolve_the_referenced_table_and_its_primary_key() {
    let fx = sqlite_fixture(SETUP, "");
    let album = keys(&fx, "Album").await;
    assert_eq!(
        album,
        vec![fk(
            "Album",
            &["artist_id"],
            "artist",
            &["id"],
            FkDirection::Outgoing
        )]
    );
    assert_eq!(album[0].other(), &rel("artist"));
    assert_eq!(album[0].name(), None);
    assert_eq!(
        keys(&fx, "implicit").await,
        vec![fk(
            "implicit",
            &["art"],
            "artist",
            &["id"],
            FkDirection::Outgoing
        )]
    );
}

#[tokio::test]
async fn incoming_keys_are_found_across_the_schema() {
    let fx = sqlite_fixture(SETUP, "");
    let artist = keys(&fx, "artist").await;
    assert_eq!(
        artist,
        vec![
            fk(
                "Album",
                &["artist_id"],
                "artist",
                &["id"],
                FkDirection::Incoming
            ),
            fk(
                "implicit",
                &["art"],
                "artist",
                &["id"],
                FkDirection::Incoming
            ),
        ]
    );
    assert_eq!(artist[0].other(), &rel("Album"));
}

#[tokio::test]
async fn composite_and_self_references_keep_key_order_and_list_both_ways() {
    let fx = sqlite_fixture(SETUP, "");
    assert_eq!(
        keys(&fx, "child").await,
        vec![
            fk(
                "child",
                &["parent_id"],
                "child",
                &["id"],
                FkDirection::Outgoing
            ),
            fk(
                "child",
                &["pa", "pb"],
                "pair",
                &["a", "b"],
                FkDirection::Outgoing
            ),
            fk(
                "child",
                &["parent_id"],
                "child",
                &["id"],
                FkDirection::Incoming
            ),
        ]
    );
    assert_eq!(
        keys(&fx, "pair").await,
        vec![fk(
            "child",
            &["pa", "pb"],
            "pair",
            &["a", "b"],
            FkDirection::Incoming
        )]
    );
}

#[tokio::test]
async fn a_missing_target_keeps_its_written_name_or_is_dropped_without_columns() {
    let fx = sqlite_fixture(SETUP, "");
    assert_eq!(
        keys(&fx, "dangling").await,
        vec![fk(
            "dangling",
            &["y"],
            "gone",
            &["id"],
            FkDirection::Outgoing
        )]
    );
}

#[tokio::test]
async fn no_keys_for_a_plain_table_or_an_unknown_relation() {
    let fx = sqlite_fixture(SETUP, "");
    assert!(keys(&fx, "lonely").await.is_empty());
    assert!(keys(&fx, "missing").await.is_empty());
    let mut s = fx.session(SessionOptions::default()).await;
    let none = s
        .foreign_keys(&RelationRef::new("nosuch", "artist"))
        .await
        .unwrap();
    assert!(none.is_empty());
    s.close().await.unwrap();
}

#[test]
fn key_filter_quotes_columns_and_writes_untyped_literals() {
    let t = |s: &str| EditValue::Text(s.into());
    let sqlite = vec![
        ("AlbumId".to_owned(), t("it's")),
        ("b".to_owned(), EditValue::Bytes(vec![1, 0xab])),
    ];
    assert_eq!(
        key_filter(Engine::Sqlite, &sqlite).as_deref(),
        Some("AlbumId = 'it''s' AND b = X'01ab'")
    );
    let pg = vec![
        ("AlbumId".to_owned(), t("a\\b")),
        ("n".to_owned(), EditValue::Bytes(vec![1])),
    ];
    assert_eq!(
        key_filter(Engine::Postgres, &pg).as_deref(),
        Some("\"AlbumId\" = E'a\\\\b' AND n = E'\\\\x01'")
    );
    let my = vec![
        ("AlbumId".to_owned(), t("a\\b")),
        ("b".to_owned(), EditValue::Bytes(vec![1, 0xab])),
    ];
    assert_eq!(
        key_filter(Engine::Mysql, &my).as_deref(),
        Some("`AlbumId` = CONVERT(X'615c62' USING utf8mb4) AND `b` = X'01ab'")
    );
    assert_eq!(
        key_filter(Engine::Postgres, &[("id".to_owned(), t("5"))]).as_deref(),
        Some("id = '5'")
    );
    assert_eq!(key_filter(Engine::Sqlite, &[]), None);
    assert_eq!(
        key_filter(Engine::Sqlite, &[("id".to_owned(), EditValue::Null)]),
        None
    );
    assert_eq!(
        key_filter(Engine::Sqlite, &[("id".to_owned(), EditValue::Default)]),
        None
    );
}

#[tokio::test]
async fn a_key_filter_browses_the_matching_rows() {
    let fx = sqlite_fixture(SETUP, "");
    let target = fx.target("Album").await;
    let filter = key_filter(
        Engine::Sqlite,
        &[("artist_id".to_owned(), EditValue::Text("1".into()))],
    );
    let sql = browse_sql(&target, &Browse::new(filter, None)).unwrap();
    assert_eq!(fx.rows(&sql).await.len(), 2);
}
