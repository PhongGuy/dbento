//! The MySQL catalog on both flavors: schemas, relations and columns, with names compared as
//! the server compares them and MariaDB's spellings read as MySQL 8 prints them.
mod support;

use dbento_core::{ColumnMeta, Limits, Relation, RelationKind, RelationRef, SessionOptions};
use mysql_async::prelude::*;
use support::{MyFixture, my_fixtures, run, text};

/// `Pk` and `pk` differ only in case and in their keys; `kinds` has one column per spelling
/// the two flavors disagree on.
const SETUP: &str = "CREATE TABLE Pk (a INT, b INT, PRIMARY KEY (b));
CREATE TABLE pk (a INT PRIMARY KEY, b INT);
CREATE TABLE kinds (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT,
  code VARCHAR(10) NOT NULL DEFAULT 'it''s',
  n TINYINT(1) DEFAULT 0,
  t TINYINT,
  y YEAR DEFAULT 2024,
  m MEDIUMINT UNSIGNED,
  z INT(5) ZEROFILL,
  d DATE DEFAULT '2024-01-02',
  e ENUM('x','y') DEFAULT 'x',
  s VARCHAR(5) DEFAULT 'NULL',
  nul VARCHAR(5) DEFAULT NULL,
  f BIT(1) DEFAULT b'1',
  amount DECIMAL(5,2) DEFAULT 1.5,
  g INT GENERATED ALWAYS AS (n * 2) VIRTUAL,
  PRIMARY KEY (code, id), KEY (id));
CREATE VIEW v AS SELECT id FROM kinds;";

async fn setup(fx: &MyFixture) {
    let mut c = fx.db.admin().await;
    c.query_drop(SETUP).await.unwrap();
    c.disconnect().await.unwrap();
}

fn col(name: &str, ty: &str, nullable: bool, pk: Option<u16>, default: Option<&str>) -> ColumnMeta {
    ColumnMeta::new(name, ty, nullable, pk, default.map(str::to_owned))
}

#[tokio::test]
async fn schemas_put_the_system_ones_last() {
    for (label, fx) in my_fixtures("").await {
        let mut s = fx.session(SessionOptions::default()).await;
        let names: Vec<String> = s
            .schemas()
            .await
            .unwrap()
            .iter()
            .map(|x| x.name().to_owned())
            .collect();
        let n = names.len();
        assert!(n > 4, "{label}: {names:?}");
        assert_eq!(
            &names[n - 4..],
            ["information_schema", "mysql", "performance_schema", "sys"],
            "{label}"
        );
        let user = &names[..n - 4];
        assert!(
            user.iter().any(|x| x == fx.db.database()),
            "{label}: {names:?}"
        );
        assert!(user.windows(2).all(|w| w[0] < w[1]), "{label}: {user:?}");
    }
}

#[tokio::test]
async fn relations_are_listed_in_byte_order_with_their_kinds() {
    for (label, fx) in my_fixtures("").await {
        setup(&fx).await;
        let mut s = fx.session(SessionOptions::default()).await;
        let list = s.relations(fx.db.database()).await.unwrap();
        assert_eq!(
            list.items(),
            [
                Relation::new("Pk", RelationKind::Table),
                Relation::new("kinds", RelationKind::Table),
                Relation::new("pk", RelationKind::Table),
                Relation::new("v", RelationKind::View),
            ],
            "{label}"
        );
        assert!(!list.truncated(), "{label}");
        let none = s.relations("dbento_no_such_schema").await.unwrap();
        assert!(none.items().is_empty() && !none.truncated(), "{label}");
        let system = s.relations("information_schema").await.unwrap();
        assert!(
            system
                .items()
                .iter()
                .any(|r| r.name() == "TABLES" && r.kind() == RelationKind::View),
            "{label}"
        );
    }
}

#[tokio::test]
async fn columns_read_the_same_on_both_flavors() {
    for (label, fx) in my_fixtures("").await {
        setup(&fx).await;
        let mut s = fx.session(SessionOptions::default()).await;
        let db = fx.db.database();
        assert_eq!(
            s.columns(&RelationRef::new(db, "kinds")).await.unwrap(),
            vec![
                col("id", "bigint unsigned", false, Some(2), None),
                col("code", "varchar(10)", false, Some(1), Some("'it''s'")),
                col("n", "tinyint(1)", true, None, Some("0")),
                col("t", "tinyint", true, None, None),
                col("y", "year", true, None, Some("2024")),
                col("m", "mediumint unsigned", true, None, None),
                col("z", "int(5) unsigned zerofill", true, None, None),
                col("d", "date", true, None, Some("'2024-01-02'")),
                col("e", "enum('x','y')", true, None, Some("'x'")),
                col("s", "varchar(5)", true, None, Some("'NULL'")),
                col("nul", "varchar(5)", true, None, None),
                col("f", "bit(1)", true, None, Some("b'1'")),
                col("amount", "decimal(5,2)", true, None, Some("1.50")),
                col("g", "int", true, None, None),
            ],
            "{label}"
        );
        assert_eq!(
            s.columns(&RelationRef::new(db, "pk")).await.unwrap(),
            vec![
                col("a", "int", false, Some(1), None),
                col("b", "int", true, None, None)
            ],
            "{label}"
        );
        assert_eq!(
            s.columns(&RelationRef::new(db, "Pk")).await.unwrap(),
            vec![
                col("a", "int", true, None, None),
                col("b", "int", false, Some(1), None)
            ],
            "{label}"
        );
        assert!(
            s.columns(&RelationRef::new(db, "PK"))
                .await
                .unwrap()
                .is_empty(),
            "{label}"
        );
        assert!(
            s.columns(&RelationRef::new("dbento_no_such_schema", "pk"))
                .await
                .unwrap()
                .is_empty(),
            "{label}"
        );
    }
}

#[tokio::test]
async fn the_catalog_settles_a_dropped_stream_and_reads_in_read_only_sessions() {
    for (label, fx) in my_fixtures("").await {
        setup(&fx).await;
        let mut opts = SessionOptions::default();
        opts.force_read_only = true;
        let mut s = fx.session(opts).await;
        let pk = RelationRef::new(fx.db.database(), "pk");
        let mut script = s.script("SELECT TABLE_NAME FROM information_schema.COLUMNS");
        let mut stream = script
            .next()
            .await
            .unwrap()
            .unwrap()
            .execute(Limits::default())
            .await
            .unwrap();
        assert!(stream.next_batch().await.unwrap().is_some(), "{label}");
        drop(stream);
        drop(script);
        assert_eq!(s.columns(&pk).await.unwrap().len(), 2, "{label}");
        let ran = run(&mut s, "SELECT COUNT(*) FROM pk", Limits::default())
            .await
            .remove(0)
            .unwrap();
        assert_eq!(ran.rows, vec![vec![text("0")]], "{label}");
    }
}
