//! The MySQL bound path on every matrix server: string parameters, bytes, matched rows, warnings.
use mysql_async::prelude::Queryable;

use crate::rowedit::{EditValue, Param};
use crate::session::{ExecuteError, Session, SessionOptions, connect};
use crate::{Secret, Workspace};

async fn open(db: &dbento_testkit::MyDb) -> (tempfile::TempDir, Session) {
    let dir = tempfile::tempdir().unwrap();
    let toml = format!("dbento = 1\n{}", db.connection_toml("my", ""));
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    let secret = Some(Secret::new(db.password()));
    let s = connect(
        &ws,
        ws.declared("my").unwrap(),
        secret,
        SessionOptions::default(),
    )
    .await
    .unwrap();
    (dir, s)
}

async fn setup(db: &dbento_testkit::MyDb, sql: &str) {
    let mut c = db.admin().await;
    c.query_drop(sql).await.unwrap();
    c.disconnect().await.unwrap();
}

fn p(s: &str) -> Param {
    Param::Text(s.to_owned())
}

#[tokio::test(flavor = "multi_thread")]
async fn string_parameters_store_each_type_exactly() {
    for (label, db) in dbento_testkit::my_matrix().await {
        setup(
            &db,
            "CREATE TABLE t (k INT PRIMARY KEY, dec30 DECIMAL(30,10), ubig BIGINT UNSIGNED, \
            e ENUM('x','y','z'), st SET('a','b','c'), j JSON, dt6 DATETIME(6), y YEAR, tm TIME, \
            dbl DOUBLE); INSERT INTO t (k) VALUES (1)",
        )
        .await;
        let (_dir, mut s) = open(&db).await;
        let params = [
            p("12345678901234567890.0123456789"),
            p("18446744073709551615"),
            p("y"),
            p("a,b"),
            p("{\"a\": 1, \"b\": null}"),
            p("2024-02-29 23:59:59.123456"),
            p("2024"),
            p("-838:59:59"),
            p("0.1"),
            p("1"),
        ];
        let sql = "UPDATE t SET dec30 = ?, ubig = ?, e = ?, st = ?, j = ?, dt6 = ?, y = ?, tm = ?, \
            dbl = ? WHERE k = ?";
        assert_eq!(s.run_bound(sql, &params).await.unwrap(), 1, "{label}");
        let rows = s
            .query_bound(
                "SELECT CAST(dec30 AS CHAR), CAST(ubig AS CHAR), e, st, \
            CAST(dt6 AS CHAR), CAST(y AS CHAR), CAST(tm AS CHAR), CAST(dbl AS CHAR), \
            CAST(JSON_EXTRACT(j, '$.a') AS CHAR) FROM t WHERE k = ?",
                &[p("1")],
            )
            .await
            .unwrap();
        let want = [
            "12345678901234567890.0123456789",
            "18446744073709551615",
            "y",
            "a,b",
            "2024-02-29 23:59:59.123456",
            "2024",
            "-838:59:59",
            "0.1",
            "1",
        ];
        let want: Vec<EditValue> = want
            .iter()
            .map(|v| EditValue::Text((*v).to_owned()))
            .collect();
        assert_eq!(rows, vec![want], "{label}");
        s.close().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn bytes_through_unhex_round_trip_every_byte_value() {
    for (label, db) in dbento_testkit::my_matrix().await {
        setup(
            &db,
            "CREATE TABLE b (k INT PRIMARY KEY, vb VARBINARY(300), b64 BIT(64)); \
            INSERT INTO b (k) VALUES (1)",
        )
        .await;
        let (_dir, mut s) = open(&db).await;
        let all: Vec<u8> = (0..=255).collect();
        let hex: String = all.iter().map(|b| format!("{b:02x}")).collect();
        let sql = "UPDATE b SET vb = UNHEX(?), b64 = UNHEX(?) WHERE k = ?";
        assert_eq!(
            s.run_bound(sql, &[p(&hex), p("ffffffffffffffff"), p("1")])
                .await
                .unwrap(),
            1,
            "{label}"
        );
        let rows = s
            .query_bound("SELECT vb, b64 FROM b WHERE vb = UNHEX(?)", &[p(&hex)])
            .await
            .unwrap();
        assert_eq!(
            rows,
            vec![vec![EditValue::Bytes(all), EditValue::Bytes(vec![0xff; 8])]],
            "{label}"
        );
        s.close().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn matched_rows_count_a_same_value_update_and_the_text_protocol_runs_transactions() {
    for (label, db) in dbento_testkit::my_matrix().await {
        setup(
            &db,
            "CREATE TABLE c (k INT PRIMARY KEY, v INT); INSERT INTO c VALUES (1, 10)",
        )
        .await;
        let (_dir, mut s) = open(&db).await;
        let upd = "UPDATE c SET v = ? WHERE k = ?";
        assert_eq!(
            s.run_bound(upd, &[p("10"), p("1")]).await.unwrap(),
            1,
            "{label}: same value"
        );
        assert_eq!(
            s.run_bound(upd, &[p("11"), p("99")]).await.unwrap(),
            0,
            "{label}: no row"
        );
        match s.run_bound(upd, &[p("abc"), p("1")]).await {
            Err(ExecuteError::Database { code, .. }) => assert_eq!(code, "1366", "{label}"),
            other => panic!("{label}: {other:?}"),
        }
        s.run_bound("START TRANSACTION", &[]).await.unwrap();
        assert_eq!(
            s.run_bound("UPDATE c SET v = 12 WHERE k = 1", &[])
                .await
                .unwrap(),
            1,
            "{label}"
        );
        s.run_bound("ROLLBACK", &[]).await.unwrap();
        s.close().await.unwrap();
        assert_eq!(
            db.scalar("SELECT v FROM c").await.as_deref(),
            Some("10"),
            "{label}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn warnings_come_back_with_their_level() {
    for (label, db) in dbento_testkit::my_matrix().await {
        let (_dir, mut s) = open(&db).await;
        s.run_bound("DO CAST(? AS SIGNED)", &[p("abc")])
            .await
            .unwrap();
        let w = s.bound_warnings().await.unwrap();
        assert!(
            w.iter()
                .any(|(l, m)| l == "Warning" && m.contains("Truncated incorrect INTEGER value")),
            "{label}: {w:?}"
        );
        s.run_bound("DROP TABLE IF EXISTS no_such_table", &[])
            .await
            .unwrap();
        let notes = s.bound_warnings().await.unwrap();
        assert!(notes.iter().all(|(l, _)| l == "Note"), "{label}: {notes:?}");
        s.run_bound("DO 1", &[]).await.unwrap();
        assert!(
            s.bound_warnings().await.unwrap().is_empty(),
            "{label}: clean"
        );
        s.close().await.unwrap();
    }
}
