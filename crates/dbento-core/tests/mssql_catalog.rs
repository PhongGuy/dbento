//! The SQL Server catalog: schemas, relations and columns of the connection's database, with
//! names compared as the database's collation compares them.
mod support;

use std::time::{Duration, Instant};

use dbento_core::{
    ColumnMeta, ExecuteError, IntrospectError, Limits, Relation, RelationKind, RelationRef,
    SessionOptions,
};
use support::{MsFixture, ms_off, run, text};

/// Runs each batch on a client of the fixture's database.
async fn setup(fx: &MsFixture, batches: &[&str]) {
    let mut c = fx.db.client().await;
    for b in batches {
        c.simple_query(*b)
            .await
            .unwrap()
            .into_results()
            .await
            .unwrap();
    }
}

const SALES: &[&str] = &[
    "CREATE SCHEMA sales",
    "CREATE TYPE dbo.phone FROM varchar(20) NOT NULL",
    "CREATE TABLE sales.Orders (id int IDENTITY PRIMARY KEY, code nvarchar(10) NOT NULL \
     DEFAULT N'it''s', amt decimal(10,2) NULL DEFAULT 0, ph dbo.phone, rv rowversion, \
     h hierarchyid, t2 datetime2(3), c char(3), nm nvarchar(max), f float(24))",
    "CREATE TABLE sales.orders2 (a int NOT NULL, b int NOT NULL, CONSTRAINT pk2 PRIMARY KEY (b, a))",
    "CREATE VIEW sales.v AS SELECT id FROM sales.Orders",
    "CREATE SYNONYM sales.syn FOR sales.Orders",
];

/// `Pk` and `pk` differ only in case, which a case-sensitive database keeps apart.
const CASED: &[&str] = &[
    "ALTER DATABASE CURRENT COLLATE Latin1_General_100_CS_AS",
    "CREATE TABLE dbo.Pk (a int NOT NULL, b int NOT NULL, PRIMARY KEY (b))",
    "CREATE TABLE dbo.pk (a int NOT NULL PRIMARY KEY, b int NULL)",
];

#[tokio::test]
async fn schemas_hide_guest_and_the_role_schemas_and_put_the_systems_last() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(&fx, &SALES[..1]).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let names: Vec<String> = s
        .schemas()
        .await
        .unwrap()
        .iter()
        .map(|x| x.name().to_owned())
        .collect();
    assert_eq!(names, ["dbo", "sales", "INFORMATION_SCHEMA", "sys"]);
}

#[tokio::test]
async fn relations_are_a_schemas_tables_and_views_named_as_the_database_compares() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(&fx, SALES).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let want = [
        Relation::new("Orders", RelationKind::Table),
        Relation::new("orders2", RelationKind::Table),
        Relation::new("v", RelationKind::View),
    ];
    for schema in ["sales", "SALES"] {
        let list = s.relations(schema).await.unwrap();
        assert_eq!(
            (list.items(), list.truncated()),
            (&want[..], false),
            "{schema}"
        );
    }
    let none = s.relations("nope").await.unwrap();
    assert!(none.items().is_empty() && !none.truncated());
    let sys = s.relations("sys").await.unwrap();
    assert!(
        sys.items()
            .contains(&Relation::new("objects", RelationKind::View)),
        "{:?}",
        &sys.items()[..sys.items().len().min(5)]
    );
}

#[tokio::test]
async fn on_a_case_sensitive_database_relation_names_match_exactly() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(&fx, CASED).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let list = s.relations("dbo").await.unwrap();
    assert_eq!(
        list.items(),
        [
            Relation::new("pk", RelationKind::Table),
            Relation::new("Pk", RelationKind::Table)
        ]
    );
    assert!(s.relations("DBO").await.unwrap().items().is_empty());
}

#[tokio::test]
async fn the_catalog_stays_on_the_connections_database_after_a_use() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(&fx, SALES).await;
    let mut s = fx.session(SessionOptions::default()).await;
    run(&mut s, "USE master", Limits::default())
        .await
        .remove(0)
        .unwrap();
    let list = s.relations("sales").await.unwrap();
    assert_eq!(list.items().len(), 3, "{:?}", list.items());
    let ran = run(&mut s, "SELECT DB_NAME() AS db", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(ran.rows, vec![vec![text("master")]]);
}

fn col(name: &str, ty: &str, nullable: bool, pk: Option<u16>, default: Option<&str>) -> ColumnMeta {
    ColumnMeta::new(name, ty, nullable, pk, default.map(str::to_owned))
}

#[tokio::test]
async fn columns_spell_types_as_result_columns_do_with_keys_and_defaults() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(&fx, SALES).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let want = [
        col("id", "INT", false, Some(1), None),
        col("code", "NVARCHAR(10)", false, None, Some("(N'it''s')")),
        col("amt", "DECIMAL(10,2)", true, None, Some("((0))")),
        col("ph", "VARCHAR(20)", false, None, None),
        col("rv", "ROWVERSION", false, None, None),
        col("h", "HIERARCHYID", true, None, None),
        col("t2", "DATETIME2(3)", true, None, None),
        col("c", "CHAR(3)", true, None, None),
        col("nm", "NVARCHAR(MAX)", true, None, None),
        col("f", "REAL", true, None, None),
    ];
    for name in ["Orders", "ORDERS"] {
        let got = s.columns(&RelationRef::new("sales", name)).await.unwrap();
        assert_eq!(got, want, "{name}");
    }
    let keyed = s
        .columns(&RelationRef::new("sales", "orders2"))
        .await
        .unwrap();
    assert_eq!(
        keyed,
        [
            col("a", "INT", false, Some(2), None),
            col("b", "INT", false, Some(1), None)
        ]
    );
    let view = s.columns(&RelationRef::new("sales", "v")).await.unwrap();
    assert_eq!(view, [col("id", "INT", false, None, None)]);
    let system = s
        .columns(&RelationRef::new("sys", "objects"))
        .await
        .unwrap();
    assert_eq!(system[0], col("name", "NVARCHAR(128)", false, None, None));
    for missing in ["nope", "syn"] {
        let got = s
            .columns(&RelationRef::new("sales", missing))
            .await
            .unwrap();
        assert!(got.is_empty(), "{missing}: {got:?}");
    }
}

#[tokio::test]
async fn on_a_case_sensitive_database_column_lookups_match_exactly() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(&fx, CASED).await;
    let mut s = fx.session(SessionOptions::default()).await;
    assert_eq!(
        s.columns(&RelationRef::new("dbo", "Pk")).await.unwrap(),
        [
            col("a", "INT", false, None, None),
            col("b", "INT", false, Some(1), None)
        ]
    );
    assert_eq!(
        s.columns(&RelationRef::new("dbo", "pk")).await.unwrap(),
        [
            col("a", "INT", false, Some(1), None),
            col("b", "INT", true, None, None)
        ]
    );
    assert!(
        s.columns(&RelationRef::new("dbo", "PK"))
            .await
            .unwrap()
            .is_empty()
    );
}

/// A name is quoted, not spliced, so `]`, `.` and spaces find their table, and a name past
/// SQL Server's 128-character limit finds nothing instead of failing.
#[tokio::test]
async fn columns_find_names_with_brackets_dots_and_spaces_and_none_past_128_characters() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(
        &fx,
        &[
            "CREATE SCHEMA [odd.s ]]]",
            "CREATE TABLE [odd.s ]]].[we]]ird. one] (x int)",
        ],
    )
    .await;
    let mut s = fx.session(SessionOptions::default()).await;
    let got = s
        .columns(&RelationRef::new("odd.s ]", "we]ird. one"))
        .await
        .unwrap();
    assert_eq!(got, [col("x", "INT", true, None, None)]);
    let long = "a".repeat(129);
    let none = s.columns(&RelationRef::new("dbo", &long)).await.unwrap();
    assert!(none.is_empty(), "{none:?}");
}

#[tokio::test]
async fn a_catalog_call_gives_up_on_a_lock_and_keeps_the_users_transaction() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(
        &fx,
        &[
            "CREATE TABLE dbo.locked (a int)",
            "CREATE TABLE dbo.kept (a int)",
        ],
    )
    .await;
    let mut s = fx.session(SessionOptions::default()).await;
    for r in run(
        &mut s,
        "SET XACT_ABORT ON; BEGIN TRANSACTION; INSERT dbo.kept VALUES (1)",
        Limits::default(),
    )
    .await
    {
        r.unwrap();
    }
    let mut holder = fx.db.client().await;
    holder
        .simple_query("BEGIN TRANSACTION; ALTER TABLE dbo.locked ADD b int")
        .await
        .unwrap()
        .into_results()
        .await
        .unwrap();
    let started = Instant::now();
    let err = s
        .columns(&RelationRef::new("dbo", "locked"))
        .await
        .unwrap_err();
    let waited = started.elapsed();
    assert!(
        matches!(&err, IntrospectError::Execute(ExecuteError::Database { code, .. }) if code == "1222"),
        "{err}"
    );
    // The server sometimes gives up early, so this proves it blocked; the value has a unit pin.
    assert!(
        waited >= Duration::from_secs(2) && waited < Duration::from_secs(15),
        "{waited:?}"
    );
    let state = run(
        &mut s,
        "SELECT @@TRANCOUNT AS tc, (SELECT COUNT(*) FROM dbo.kept) AS n, \
         CASE WHEN @@OPTIONS & 16384 <> 0 THEN 1 ELSE 0 END AS xa",
        Limits::default(),
    )
    .await
    .remove(0)
    .unwrap();
    assert_eq!(state.rows, vec![vec![text("1"), text("1"), text("1")]]);
    holder
        .simple_query("ROLLBACK")
        .await
        .unwrap()
        .into_results()
        .await
        .unwrap();
    run(&mut s, "ROLLBACK", Limits::default())
        .await
        .remove(0)
        .unwrap();
}

/// A catalog call is a send of its own, so a cancel stops it with Attention and the session
/// reads on.
#[tokio::test]
async fn a_cancel_stops_a_catalog_call_waiting_on_a_lock() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(&fx, &["CREATE TABLE dbo.locked (a int)"]).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let mut holder = fx.db.client().await;
    holder
        .simple_query("BEGIN TRANSACTION; ALTER TABLE dbo.locked ADD b int")
        .await
        .unwrap()
        .into_results()
        .await
        .unwrap();
    let cancel = s.cancel_handle();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        cancel.cancel();
    });
    let started = Instant::now();
    let err = s
        .columns(&RelationRef::new("dbo", "locked"))
        .await
        .unwrap_err();
    let waited = started.elapsed();
    t.join().unwrap();
    assert!(
        matches!(&err, IntrospectError::Execute(ExecuteError::Cancelled)),
        "{err}"
    );
    assert!(waited < Duration::from_secs(4), "{waited:?}");
    holder
        .simple_query("ROLLBACK")
        .await
        .unwrap()
        .into_results()
        .await
        .unwrap();
    let one = run(&mut s, "SELECT 1 AS one", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(one.rows, vec![vec![text("1")]]);
}
