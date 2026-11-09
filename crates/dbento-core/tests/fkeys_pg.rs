//! Foreign keys in both directions on Postgres, partitions included.
use crate::support::{PgFixture, pg_fixture, text};
use dbento_core::{
    Browse, EditValue, Engine, FkDirection, ForeignKey, RelationRef, SessionOptions, browse_sql,
    key_filter,
};

const SETUP: &str = r#"
CREATE SCHEMA "Other";
CREATE TABLE artist(id int PRIMARY KEY, name text);
CREATE TABLE "Other"."Album"("AlbumId" int PRIMARY KEY,
  artist_id int CONSTRAINT album_artist REFERENCES artist(id), title text);
CREATE TABLE pair(a int, b text, PRIMARY KEY (a, b));
CREATE TABLE child(id int PRIMARY KEY, pb text, pa int,
  parent_id int CONSTRAINT child_parent REFERENCES child(id),
  CONSTRAINT child_pair FOREIGN KEY (pa, pb) REFERENCES pair(a, b));
CREATE TABLE parts(id int PRIMARY KEY, artist_id int CONSTRAINT parts_artist REFERENCES artist(id))
  PARTITION BY RANGE (id);
CREATE TABLE parts_1 PARTITION OF parts FOR VALUES FROM (0) TO (100);
CREATE TABLE ref_parts(id int PRIMARY KEY, p_id int, CONSTRAINT rp FOREIGN KEY (p_id) REFERENCES parts(id));
CREATE TABLE blobs(k bytea PRIMARY KEY, s text);
INSERT INTO artist VALUES (1, 'a'), (2, 'b');
INSERT INTO "Other"."Album" VALUES (7, 1, 'x'), (8, 2, 'y');
INSERT INTO blobs VALUES ('\x01ab', 'a\b'), ('\x02', 'c'), ('\x03', 'it''s\');
"#;

fn r(schema: &str, name: &str) -> RelationRef {
    RelationRef::new(schema, name)
}
fn cols(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}
fn fk(
    name: &str,
    table: RelationRef,
    c: &[&str],
    referenced: RelationRef,
    rc: &[&str],
    d: FkDirection,
) -> ForeignKey {
    ForeignKey::new(
        Some(name.to_owned()),
        table,
        cols(c),
        referenced,
        cols(rc),
        d,
    )
}
async fn setup() -> PgFixture {
    let fx = pg_fixture("").await;
    fx.db.admin().await.batch_execute(SETUP).await.unwrap();
    fx
}
async fn keys(fx: &PgFixture, rel: RelationRef) -> Vec<ForeignKey> {
    let mut s = fx.session(SessionOptions::default()).await;
    let k = s.foreign_keys(&rel).await.unwrap();
    s.close().await.unwrap();
    k
}

#[tokio::test]
async fn keys_cross_schemas_in_both_directions() {
    let fx = setup().await;
    let album = r("Other", "Album");
    assert_eq!(
        keys(&fx, album.clone()).await,
        vec![fk(
            "album_artist",
            album.clone(),
            &["artist_id"],
            r("public", "artist"),
            &["id"],
            FkDirection::Outgoing
        )]
    );
    assert_eq!(
        keys(&fx, r("public", "artist")).await,
        vec![
            fk(
                "album_artist",
                album,
                &["artist_id"],
                r("public", "artist"),
                &["id"],
                FkDirection::Incoming
            ),
            fk(
                "parts_artist",
                r("public", "parts"),
                &["artist_id"],
                r("public", "artist"),
                &["id"],
                FkDirection::Incoming
            ),
        ]
    );
}

#[tokio::test]
async fn composite_and_self_references_keep_constraint_column_order() {
    let fx = setup().await;
    let child = r("public", "child");
    assert_eq!(
        keys(&fx, child.clone()).await,
        vec![
            fk(
                "child_parent",
                child.clone(),
                &["parent_id"],
                child.clone(),
                &["id"],
                FkDirection::Outgoing
            ),
            fk(
                "child_pair",
                child.clone(),
                &["pa", "pb"],
                r("public", "pair"),
                &["a", "b"],
                FkDirection::Outgoing
            ),
            fk(
                "child_parent",
                child.clone(),
                &["parent_id"],
                child,
                &["id"],
                FkDirection::Incoming
            ),
        ]
    );
}

#[tokio::test]
async fn partitioned_tables_list_each_key_once() {
    let fx = setup().await;
    let (parts, artist) = (r("public", "parts"), r("public", "artist"));
    assert_eq!(
        keys(&fx, r("public", "ref_parts")).await,
        vec![fk(
            "rp",
            r("public", "ref_parts"),
            &["p_id"],
            parts.clone(),
            &["id"],
            FkDirection::Outgoing
        )]
    );
    assert_eq!(
        keys(&fx, parts.clone()).await,
        vec![
            fk(
                "parts_artist",
                parts.clone(),
                &["artist_id"],
                artist.clone(),
                &["id"],
                FkDirection::Outgoing
            ),
            fk(
                "rp",
                r("public", "ref_parts"),
                &["p_id"],
                parts,
                &["id"],
                FkDirection::Incoming
            ),
        ]
    );
    let p1 = keys(&fx, r("public", "parts_1")).await;
    assert_eq!(p1.len(), 2, "{p1:?}");
    assert_eq!(p1[0].direction(), FkDirection::Outgoing);
    assert_eq!(p1[0].referenced(), &artist);
    assert_eq!(p1[1].direction(), FkDirection::Incoming);
    assert_eq!(p1[1].table(), &r("public", "ref_parts"));
}

#[tokio::test]
async fn an_unknown_relation_has_no_keys() {
    let fx = setup().await;
    assert!(keys(&fx, r("public", "missing")).await.is_empty());
    assert!(keys(&fx, r("nosuch", "artist")).await.is_empty());
}

#[tokio::test]
async fn key_filters_match_quoted_columns_bytes_and_backslashes() {
    let fx = setup().await;
    let count = |sql: String| format!("SELECT count(*)::text FROM ({sql}) AS s");
    let album = fx.target("Other", "Album").await;
    let f = key_filter(
        Engine::Postgres,
        &[("AlbumId".to_owned(), EditValue::Text("7".into()))],
    );
    let sql = browse_sql(&album, &Browse::new(f, None)).unwrap();
    assert_eq!(fx.row(&count(sql)).await[0], text("1"));
    let blobs = fx.target("public", "blobs").await;
    let pairs = [
        ("k".to_owned(), EditValue::Bytes(vec![1, 0xab])),
        ("s".to_owned(), EditValue::Text("a\\b".into())),
    ];
    let sql = browse_sql(
        &blobs,
        &Browse::new(key_filter(Engine::Postgres, &pairs), None),
    )
    .unwrap();
    assert_eq!(fx.row(&count(sql)).await[0], text("1"));
}

#[tokio::test]
async fn key_filters_match_a_quote_beside_a_backslash() {
    let fx = setup().await;
    let blobs = fx.target("public", "blobs").await;
    let f = key_filter(
        Engine::Postgres,
        &[("s".to_owned(), EditValue::Text("it's\\".into()))],
    );
    let sql = browse_sql(&blobs, &Browse::new(f, None)).unwrap();
    let sql = format!("SELECT encode(k, 'hex') FROM ({sql}) AS s");
    assert_eq!(fx.row(&sql).await[0], text("03"));
}
