//! Postgres introspection: names, kinds and columns from the catalogs, read-only, and
//! without touching the user's own transaction.
mod support;

use std::future::Future;
use std::time::{Duration, Instant};

use RelationKind::{MaterializedView, Table, View};
use dbento_core::{
    ColumnMeta, Engine, ExecuteError, IntrospectError, Limits, RelationKind, RelationList,
    RelationRef, SessionOptions, quote_ident,
};
use support::{pg_fixture, run};

const STATE: &str = "SELECT state FROM pg_stat_activity WHERE application_name = 'dbento' AND datname = current_database()";

/// Quotes, Unicode, a reserved word and every kind the tree groups, plus a sequence and an
/// index that must stay out of the list.
const TORTURE: &str = r#"
CREATE SCHEMA "Ünï ""q"" schema";
SET search_path = "Ünï ""q"" schema";
CREATE TABLE "select" (id int PRIMARY KEY, "Mixed Col" text NOT NULL DEFAULT 'x', gone int);
ALTER TABLE "select" DROP COLUMN gone;
CREATE TABLE pair (b int, a int, note text, PRIMARY KEY (a, b));
CREATE VIEW "we""ird view" AS SELECT 1 AS one;
CREATE MATERIALIZED VIEW mv AS SELECT 1 AS one;
CREATE TABLE parted (k int) PARTITION BY RANGE (k);
CREATE SEQUENCE seq;
CREATE INDEX ON "select" ("Mixed Col");
"#;
const SCHEMA: &str = "Ünï \"q\" schema";

fn read_only() -> SessionOptions {
    let mut opts = SessionOptions::default();
    opts.force_read_only = true;
    opts
}

async fn bounded(test: impl Future<Output = ()>) {
    tokio::time::timeout(Duration::from_secs(120), test)
        .await
        .expect("test timed out");
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
                c.name().to_owned(),
                c.type_name().to_owned(),
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

#[tokio::test]
async fn chinook_lists_public_its_tables_and_their_columns() {
    bounded(async {
        let fx = pg_fixture("").await;
        fx.db
            .admin()
            .await
            .batch_execute(dbento_testkit::chinook_pg_sql())
            .await
            .unwrap();
        let mut s = fx.session(read_only()).await;
        let schemas: Vec<String> = s
            .schemas()
            .await
            .unwrap()
            .iter()
            .map(|x| x.name().to_owned())
            .collect();
        assert_eq!(schemas, ["public"]);
        let list = s.relations("public").await.unwrap();
        assert!(!list.truncated());
        assert_eq!(
            rels(&list),
            want(&[
                ("album", Table),
                ("artist", Table),
                ("customer", Table),
                ("employee", Table),
                ("genre", Table),
                ("invoice", Table),
                ("invoice_line", Table),
                ("media_type", Table),
                ("playlist", Table),
                ("playlist_track", Table),
                ("track", Table),
            ])
        );
        let album = s
            .columns(&RelationRef::new("public", "album"))
            .await
            .unwrap();
        assert_eq!(
            cols(&album),
            [
                col("album_id", "integer", false, Some(1), None),
                col("title", "character varying(160)", false, None, None),
                col("artist_id", "integer", false, None, None),
            ]
        );
        let pt = s
            .columns(&RelationRef::new("public", "playlist_track"))
            .await
            .unwrap();
        assert_eq!(
            cols(&pt),
            [
                col("playlist_id", "integer", false, Some(1), None),
                col("track_id", "integer", false, Some(2), None),
            ]
        );
        // Each call's read-only wrapper has ended.
        assert_eq!(fx.db.scalar(STATE).await.as_deref(), Some("idle"));
        s.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn torture_names_kinds_and_columns_come_back_exactly() {
    bounded(async {
        let fx = pg_fixture("").await;
        fx.db.admin().await.batch_execute(TORTURE).await.unwrap();
        let mut s = fx.session(read_only()).await;
        let schemas: Vec<String> = s
            .schemas()
            .await
            .unwrap()
            .iter()
            .map(|x| x.name().to_owned())
            .collect();
        assert_eq!(schemas, ["public", SCHEMA]);
        let list = s.relations(SCHEMA).await.unwrap();
        assert_eq!(
            rels(&list),
            want(&[
                ("mv", MaterializedView),
                ("pair", Table),
                ("parted", Table),
                ("select", Table),
                ("we\"ird view", View),
            ])
        );
        let select = s
            .columns(&RelationRef::new(SCHEMA, "select"))
            .await
            .unwrap();
        assert_eq!(
            cols(&select),
            [
                col("id", "integer", false, Some(1), None),
                col("Mixed Col", "text", false, None, Some("'x'::text")),
            ]
        );
        let pair = s.columns(&RelationRef::new(SCHEMA, "pair")).await.unwrap();
        assert_eq!(
            cols(&pair),
            [
                col("b", "integer", false, Some(2), None),
                col("a", "integer", false, Some(1), None),
                col("note", "text", true, None, None),
            ]
        );
        let view = s
            .columns(&RelationRef::new(SCHEMA, "we\"ird view"))
            .await
            .unwrap();
        assert_eq!(cols(&view), [col("one", "integer", true, None, None)]);
        assert_eq!(fx.db.scalar(STATE).await.as_deref(), Some("idle"));
        s.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn unknown_names_read_as_empty() {
    bounded(async {
        let fx = pg_fixture("").await;
        let mut s = fx.session(read_only()).await;
        let list = s.relations("nope").await.unwrap();
        assert!(list.items().is_empty() && !list.truncated());
        assert!(
            s.columns(&RelationRef::new("public", "nope"))
                .await
                .unwrap()
                .is_empty()
        );
        s.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn a_read_write_transaction_survives_introspection() {
    bounded(async {
        let fx = pg_fixture("").await;
        let mut s = fx.session(SessionOptions::default()).await;
        for r in run(
            &mut s,
            "BEGIN; CREATE TABLE inflight(x int)",
            Limits::default(),
        )
        .await
        {
            r.unwrap();
        }
        // The open transaction's own table is visible to the catalog query.
        assert_eq!(
            rels(&s.relations("public").await.unwrap()),
            want(&[("inflight", Table)])
        );
        for r in run(&mut s, "ROLLBACK", Limits::default()).await {
            r.unwrap();
        }
        // Not committed by introspection: the rollback removed it.
        let gone = "SELECT count(*)::text FROM pg_class WHERE relname = 'inflight'";
        assert_eq!(fx.db.scalar(gone).await.as_deref(), Some("0"));
        for r in run(&mut s, "BEGIN; CREATE TABLE kept(x int)", Limits::default()).await {
            r.unwrap();
        }
        assert_eq!(
            s.columns(&RelationRef::new("public", "kept"))
                .await
                .unwrap()
                .len(),
            1
        );
        for r in run(&mut s, "COMMIT", Limits::default()).await {
            r.unwrap();
        }
        // Not rolled back by introspection: the commit kept it.
        let kept = "SELECT count(*)::text FROM pg_class WHERE relname = 'kept'";
        assert_eq!(fx.db.scalar(kept).await.as_deref(), Some("1"));
        s.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn a_terminated_backend_reads_as_a_closed_session() {
    bounded(async {
        let fx = pg_fixture("").await;
        let mut s = fx.session(read_only()).await;
        s.schemas().await.unwrap();
        fx.db
            .scalar("SELECT pg_terminate_backend(pid)::text FROM pg_stat_activity WHERE application_name = 'dbento' AND datname = current_database()")
            .await;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // The first call after the kill already reads as closed (57P01 maps to it).
            match s.schemas().await {
                Err(IntrospectError::Execute(ExecuteError::SessionClosed)) => break,
                other => {
                    assert!(Instant::now() < deadline, "{other:?}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    })
    .await;
}

#[tokio::test]
async fn every_torture_relation_selects_through_quote_ident() {
    bounded(async {
        let fx = pg_fixture("").await;
        fx.db.admin().await.batch_execute(TORTURE).await.unwrap();
        let mut s = fx.session(read_only()).await;
        for r in s.relations(SCHEMA).await.unwrap().items().to_vec() {
            let sql = format!(
                "SELECT * FROM {}.{}",
                quote_ident(Engine::Postgres, SCHEMA),
                quote_ident(Engine::Postgres, r.name())
            );
            for ran in run(&mut s, &sql, Limits::default()).await {
                ran.unwrap_or_else(|e| panic!("{sql}: {e}"));
            }
        }
        s.close().await.unwrap();
    })
    .await;
}
