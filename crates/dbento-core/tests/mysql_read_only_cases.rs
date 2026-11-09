//! Every blocked case and every documented hole of the MySQL read-only recipe, on each gate
//! server. A hole's test pins today's behavior, so a server change is noticed here.

use crate::support::{MyFixture, Ran, my_fixtures, run, text};
use dbento_core::{ExecuteError, Limits, Session, SessionOptions};
use dbento_testkit::MyFlavor;
use mysql_async::prelude::*;

const FIXTURE: &str = "CREATE TABLE t1 (a INT) ENGINE = InnoDB; \
    CREATE TABLE audit (n INT) ENGINE = InnoDB; \
    CREATE TRIGGER t1_audit BEFORE INSERT ON t1 FOR EACH ROW INSERT INTO audit VALUES (NEW.a); \
    CREATE PROCEDURE p_write() INSERT INTO t1 VALUES (1); \
    CREATE PROCEDURE p_commit_write() BEGIN COMMIT; INSERT INTO t1 VALUES (2); END; \
    CREATE FUNCTION f_write() RETURNS INT DETERMINISTIC MODIFIES SQL DATA \
        BEGIN INSERT INTO t1 VALUES (3); RETURN 1; END";

const BLOCKED: &[&str] = &[
    "INSERT INTO t1 VALUES (9)",
    "UPDATE t1 SET a = 9",
    "DELETE FROM t1",
    "REPLACE INTO t1 VALUES (9)",
    "DELETE t1, audit FROM t1 JOIN audit ON t1.a = audit.n",
    "SELECT * FROM t1 FOR UPDATE",
    "CREATE TABLE u (a INT)",
    "ALTER TABLE t1 ADD COLUMN b INT",
    "TRUNCATE TABLE audit",
    "RENAME TABLE audit TO audit2",
    "DROP TABLE audit",
    "CREATE DATABASE dbento_never",
    "CREATE VIEW v AS SELECT 1",
    "CREATE INDEX i ON t1 (a)",
    "CREATE USER dbento_never",
    "GRANT SELECT ON t1 TO CURRENT_USER",
    "CREATE TEMPORARY TABLE tmp (a INT)",
    "CALL p_write()",
    "CALL p_commit_write()",
    "SELECT f_write()",
    "OPTIMIZE TABLE t1",
    "LOAD DATA INFILE '/etc/hostname' INTO TABLE t1",
    "PREPARE ps FROM 'INSERT INTO t1 VALUES (9)'; EXECUTE ps",
];
const BLOCKED_MYSQL: &[&str] = &["WITH d AS (SELECT 1) DELETE FROM t1"];
const BLOCKED_MARIADB: &[&str] = &["CREATE SEQUENCE sq", "SELECT NEXTVAL(sq0)"];

async fn session_on(fx: &MyFixture) -> Session {
    let mut admin = fx.db.admin().await;
    admin.query_drop(FIXTURE).await.unwrap();
    let old = fx
        .db
        .scalar("SELECT @@version LIKE '10.%'")
        .await
        .as_deref()
        == Some("1");
    let var = if old {
        "tx_read_only"
    } else {
        "transaction_read_only"
    };
    let flip = format!(
        "CREATE PROCEDURE p_flip_write() BEGIN COMMIT; SET SESSION {var} = 0; INSERT INTO t1 VALUES (4); END"
    );
    admin.query_drop(flip).await.unwrap();
    if fx.db.flavor() == MyFlavor::Mariadb {
        admin.query_drop("CREATE SEQUENCE sq0").await.unwrap();
    }
    let mut opts = SessionOptions::default();
    opts.force_read_only = true;
    fx.session(opts).await
}

async fn one(s: &mut Session, sql: &str) -> Result<Ran, ExecuteError> {
    run(s, sql, Limits::default()).await.pop().unwrap()
}

async fn rows(fx: &MyFixture) -> (String, String) {
    let count = |t: &str| format!("SELECT COUNT(*) FROM {t}");
    (
        fx.db.scalar(&count("t1")).await.unwrap(),
        fx.db.scalar(&count("audit")).await.unwrap(),
    )
}

#[tokio::test]
async fn every_blocked_case_is_refused_with_error_1792() {
    for (label, fx) in my_fixtures("").await {
        let mut s = session_on(&fx).await;
        let own = if fx.db.flavor() == MyFlavor::Mysql {
            BLOCKED_MYSQL
        } else {
            BLOCKED_MARIADB
        };
        for sql in BLOCKED.iter().chain(own) {
            let r = one(&mut s, sql).await;
            assert!(
                matches!(r, Err(ExecuteError::ReadOnlyViolation { .. })),
                "{label}: {sql}: {:?}",
                r.err()
            );
        }
        assert_eq!(rows(&fx).await, ("0".to_owned(), "0".to_owned()), "{label}");
    }
}

#[tokio::test]
async fn the_documented_holes_still_pass() {
    for (label, fx) in my_fixtures("").await {
        let mut s = session_on(&fx).await;
        let db = fx.db.database().to_owned();
        let mysql = fx.db.flavor() == MyFlavor::Mysql;
        // MySQL images limit file output to secure_file_priv; MariaDB images do not.
        let dir = if mysql {
            "/var/lib/mysql-files"
        } else {
            "/tmp"
        };
        let max = fx
            .db
            .scalar("SELECT @@GLOBAL.max_connections")
            .await
            .unwrap();
        let mut holes = vec![
            format!("SELECT 1 INTO OUTFILE '{dir}/{db}.out'"),
            format!("SELECT 'x' INTO DUMPFILE '{dir}/{db}.dump'"),
            format!("SET GLOBAL max_connections = {max}"),
            "FLUSH TABLES".to_owned(),
            format!("SELECT GET_LOCK('{db}', 0)"),
            "CALL p_flip_write()".to_owned(),
        ];
        if mysql {
            holes.push(format!("SET PERSIST max_connections = {max}"));
        }
        for sql in &holes {
            assert!(
                one(&mut s, sql).await.is_ok(),
                "{label}: {sql} is no longer a hole"
            );
        }
        if mysql {
            fx.db
                .admin()
                .await
                .query_drop("RESET PERSIST max_connections")
                .await
                .unwrap();
        }
        s.settle().await.unwrap();
        // GET_LOCK outlived its statement's ROLLBACK, and the flip's write landed.
        let held = format!("SELECT IS_USED_LOCK('{db}') IS NOT NULL");
        assert_eq!(fx.db.scalar(&held).await.as_deref(), Some("1"), "{label}");
        let freed = one(&mut s, &format!("SELECT RELEASE_LOCK('{db}')"))
            .await
            .unwrap();
        assert_eq!(freed.rows, vec![vec![text("1")]], "{label}");
        assert_eq!(rows(&fx).await, ("1".to_owned(), "1".to_owned()), "{label}");
        let after = one(&mut s, "INSERT INTO t1 VALUES (5)").await;
        assert!(
            matches!(after, Err(ExecuteError::ReadOnlyViolation { .. })),
            "{label}: read-only again"
        );
    }
}
