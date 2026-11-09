//! SQLite introspection on Chinook and torture names, attached schemas, the cap, and quoting.

use crate::support::{run, sqlite_fixture};
use RelationKind::{Table, View};
use dbento_core::{
    ColumnMeta, Engine, Limits, RELATION_CAP, RelationKind, RelationList, RelationRef,
    SessionOptions, quote_ident,
};

const TORTURE: &str = r#"
CREATE TABLE "we""ird" (id INTEGER PRIMARY KEY AUTOINCREMENT, v TEXT DEFAULT 'x');
CREATE TABLE [select] ("Ünï col" NUMERIC NOT NULL, b, PRIMARY KEY (b, "Ünï col"));
CREATE TABLE gen (a INT, twice INT GENERATED ALWAYS AS (a * 2) VIRTUAL);
CREATE VIEW "Ünï view" AS SELECT 1 AS one;
CREATE INDEX idx ON gen (a);
"#;

fn read_only() -> SessionOptions {
    let mut opts = SessionOptions::default();
    opts.force_read_only = true;
    opts
}

fn rels(l: &RelationList) -> Vec<(String, RelationKind)> {
    l.items()
        .iter()
        .map(|r| (r.name().to_owned(), r.kind()))
        .collect()
}

fn want(items: &[(&str, RelationKind)]) -> Vec<(String, RelationKind)> {
    items.iter().map(|(n, k)| ((*n).to_owned(), *k)).collect()
}

type Col = (String, String, bool, Option<u16>, Option<String>);

fn cols(c: &[ColumnMeta]) -> Vec<Col> {
    c.iter()
        .map(|c| {
            (
                c.name().into(),
                c.type_name().into(),
                c.nullable(),
                c.primary_key(),
                c.default().map(str::to_owned),
            )
        })
        .collect()
}

fn col(name: &str, ty: &str, nullable: bool, pk: Option<u16>, default: Option<&str>) -> Col {
    (
        name.into(),
        ty.into(),
        nullable,
        pk,
        default.map(str::to_owned),
    )
}

fn schema_names(s: &[dbento_core::SchemaInfo]) -> Vec<String> {
    s.iter().map(|x| x.name().to_owned()).collect()
}

#[tokio::test]
async fn chinook_lists_main_its_tables_and_their_columns() {
    let fx = sqlite_fixture(dbento_testkit::chinook_sqlite_sql(), "");
    let mut s = fx.session(read_only()).await;
    assert_eq!(schema_names(&s.schemas().await.unwrap()), ["main"]);
    let list = s.relations("main").await.unwrap();
    assert!(!list.truncated());
    assert_eq!(
        rels(&list),
        want(&[
            ("Album", Table),
            ("Artist", Table),
            ("Customer", Table),
            ("Employee", Table),
            ("Genre", Table),
            ("Invoice", Table),
            ("InvoiceLine", Table),
            ("MediaType", Table),
            ("Playlist", Table),
            ("PlaylistTrack", Table),
            ("Track", Table),
        ])
    );
    let album = s.columns(&RelationRef::new("main", "Album")).await.unwrap();
    assert_eq!(
        cols(&album),
        [
            col("AlbumId", "INTEGER", false, Some(1), None),
            col("Title", "NVARCHAR(160)", false, None, None),
            col("ArtistId", "INTEGER", false, None, None),
        ]
    );
    s.close().await.unwrap();
}

#[tokio::test]
async fn torture_names_internal_tables_and_generated_columns() {
    let fx = sqlite_fixture(TORTURE, "");
    let mut s = fx.session(read_only()).await;
    // sqlite_sequence (from AUTOINCREMENT) and the index stay out; BINARY name order.
    assert_eq!(
        rels(&s.relations("main").await.unwrap()),
        want(&[
            ("gen", Table),
            ("select", Table),
            ("we\"ird", Table),
            ("Ünï view", View)
        ])
    );
    assert_eq!(
        cols(
            &s.columns(&RelationRef::new("main", "we\"ird"))
                .await
                .unwrap()
        ),
        [
            col("id", "INTEGER", true, Some(1), None),
            col("v", "TEXT", true, None, Some("'x'"))
        ]
    );
    assert_eq!(
        cols(
            &s.columns(&RelationRef::new("main", "select"))
                .await
                .unwrap()
        ),
        [
            col("Ünï col", "NUMERIC", false, Some(2), None),
            col("b", "", true, Some(1), None)
        ]
    );
    assert_eq!(
        cols(&s.columns(&RelationRef::new("main", "gen")).await.unwrap()),
        [
            col("a", "INT", true, None, None),
            col("twice", "INT", true, None, None)
        ]
    );
    // Every listed relation selects through quote_ident.
    for r in s.relations("main").await.unwrap().items().to_vec() {
        let sql = format!(
            "SELECT * FROM {}.{}",
            quote_ident(Engine::Sqlite, "main"),
            quote_ident(Engine::Sqlite, r.name())
        );
        for ran in run(&mut s, &sql, Limits::default()).await {
            ran.unwrap_or_else(|e| panic!("{sql}: {e}"));
        }
    }
    s.close().await.unwrap();
}

#[tokio::test]
async fn attached_schemas_list_and_temp_stays_out() {
    let fx = sqlite_fixture("CREATE TABLE t(x)", "");
    let aux = fx.dir.path().join("data/aux.db");
    rusqlite::Connection::open(&aux)
        .unwrap()
        .execute_batch("CREATE TABLE inner_t(y TEXT NOT NULL)")
        .unwrap();
    let mut s = fx.session(SessionOptions::default()).await;
    let attach = format!(
        "ATTACH '{}' AS \"aux schema\"; CREATE TEMP TABLE tmp(z)",
        aux.display()
    );
    for r in run(&mut s, &attach, Limits::default()).await {
        r.unwrap();
    }
    assert_eq!(
        schema_names(&s.schemas().await.unwrap()),
        ["main", "aux schema"]
    );
    assert_eq!(
        rels(&s.relations("aux schema").await.unwrap()),
        want(&[("inner_t", Table)])
    );
    assert_eq!(
        cols(
            &s.columns(&RelationRef::new("aux schema", "inner_t"))
                .await
                .unwrap()
        ),
        [col("y", "TEXT", false, None, None)]
    );
    s.close().await.unwrap();
}

#[tokio::test]
async fn unknown_names_read_as_empty() {
    let fx = sqlite_fixture("CREATE TABLE t(x)", "");
    let mut s = fx.session(read_only()).await;
    let list = s.relations("nope").await.unwrap();
    assert!(list.items().is_empty() && !list.truncated());
    assert!(
        s.columns(&RelationRef::new("main", "nope"))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        s.columns(&RelationRef::new("nope", "t"))
            .await
            .unwrap()
            .is_empty()
    );
    s.close().await.unwrap();
}

fn many_tables(n: usize) -> String {
    let mut sql = String::from("BEGIN;");
    for i in 0..n {
        sql.push_str(&format!("CREATE TABLE t{i:05}(x);"));
    }
    sql.push_str("COMMIT;");
    sql
}

#[tokio::test]
async fn more_than_the_cap_is_truncated_and_exactly_the_cap_is_not() {
    let fx = sqlite_fixture(&many_tables(RELATION_CAP + 1), "");
    let mut s = fx.session(read_only()).await;
    let list = s.relations("main").await.unwrap();
    assert_eq!((list.items().len(), list.truncated()), (RELATION_CAP, true));
    assert_eq!(list.items()[0].name(), "t00000");
    assert_eq!(list.items()[RELATION_CAP - 1].name(), "t09999");
    s.close().await.unwrap();
    let fx = sqlite_fixture(&many_tables(RELATION_CAP), "");
    let mut s = fx.session(read_only()).await;
    let list = s.relations("main").await.unwrap();
    assert_eq!(
        (list.items().len(), list.truncated()),
        (RELATION_CAP, false)
    );
    s.close().await.unwrap();
}

#[test]
fn postgres_quotes_only_what_needs_it() {
    let q = |s: &str| quote_ident(Engine::Postgres, s);
    assert_eq!(q("artist"), "artist");
    assert_eq!(q("_x1"), "_x1");
    assert_eq!(q("name"), "name"); // an unreserved keyword
    assert_eq!(q("Artist"), "\"Artist\"");
    assert_eq!(q("select"), "\"select\"");
    assert_eq!(q("user"), "\"user\"");
    assert_eq!(q("between"), "\"between\""); // a column-name keyword
    assert_eq!(q("we\"ird"), "\"we\"\"ird\"");
    assert_eq!(q("Ünï"), "\"Ünï\"");
    assert_eq!(q("1abc"), "\"1abc\"");
    assert_eq!(q("a b"), "\"a b\"");
    assert_eq!(q("a$b"), "\"a$b\"");
    assert_eq!(q(""), "\"\"");
}

#[test]
fn sqlite_quotes_only_what_needs_it() {
    let q = |s: &str| quote_ident(Engine::Sqlite, s);
    assert_eq!(q("Artist"), "Artist");
    assert_eq!(q("main"), "main");
    assert_eq!(q("_x1"), "_x1");
    assert_eq!(q("a"), "a"); // too short for any keyword
    assert_eq!(q("order"), "\"order\"");
    assert_eq!(q("Select"), "\"Select\"");
    assert_eq!(q("temp"), "\"temp\"");
    assert_eq!(q("we\"ird"), "\"we\"\"ird\"");
    assert_eq!(q("aux schema"), "\"aux schema\"");
    assert_eq!(q("Ünï"), "\"Ünï\"");
    assert_eq!(q("1abc"), "\"1abc\"");
    assert_eq!(q(""), "\"\"");
}
