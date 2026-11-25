//! The MySQL apply session: a five-second lock wait, the browse session's pins made strict,
//! keys re-read after a key update, and notes that do not fail a change.
use std::time::{Duration, Instant};

use crate::support::{MyFixture, my_fixtures, pairs, rejected, row_key, run, val};
use dbento_core::{Change, ChangeOutcome, EditTarget, EditValue, Limits, SessionOptions};
use mysql_async::prelude::*;

const SETUP: &str = "CREATE TABLE t (id INT PRIMARY KEY, v VARCHAR(3), n INT) ENGINE=InnoDB;
INSERT INTO t VALUES (1, 'a', 10), (2, 'b', 20);";

/// The target of `table` described by a session that first ran `set`.
async fn target_after(fx: &MyFixture, set: &str, table: &str) -> EditTarget {
    let mut s = fx.session(SessionOptions::default()).await;
    run(&mut s, set, Limits::default()).await.remove(0).unwrap();
    let t = s.edit_target(&fx.rel(table)).await.unwrap();
    s.close().await.unwrap();
    t
}

#[tokio::test(flavor = "multi_thread")]
async fn a_row_locked_elsewhere_fails_after_about_five_seconds() {
    for (label, fx) in my_fixtures("").await {
        fx.exec(SETUP).await;
        let target = fx.target("t").await;
        let mut holder = fx.db.admin().await;
        holder
            .query_drop("START TRANSACTION; SELECT * FROM t WHERE id = 1 FOR UPDATE")
            .await
            .unwrap();
        let started = Instant::now();
        let change = Change::Update {
            key: row_key(&[("id", val("1"))]),
            set: pairs(&[("n", val("9"))]),
            expected: vec![],
        };
        let r = tokio::time::timeout(
            Duration::from_secs(30),
            fx.apply_changes(&target, vec![change]),
        )
        .await
        .expect("apply gives up on the lock by itself");
        match rejected(r).as_slice() {
            [ChangeOutcome::Failed(m)] => assert!(m.contains("Lock wait timeout"), "{label}: {m}"),
            other => panic!("{label}: {other:?}"),
        }
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_secs(4) && waited < Duration::from_secs(20),
            "{label}: {waited:?}"
        );
        holder.query_drop("ROLLBACK").await.unwrap();
        holder.disconnect().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_non_strict_browse_session_still_applies_strictly() {
    for (label, fx) in my_fixtures("").await {
        fx.exec(SETUP).await;
        let target = target_after(&fx, "SET SESSION sql_mode = ''", "t").await;
        let change = Change::Update {
            key: row_key(&[("id", val("1"))]),
            set: pairs(&[("v", val("abcdef"))]),
            expected: vec![],
        };
        match rejected(fx.apply_changes(&target, vec![change]).await).as_slice() {
            [ChangeOutcome::Failed(m)] => assert!(m.contains("Data too long"), "{label}: {m}"),
            other => panic!("{label}: {other:?}"),
        }
        assert_eq!(
            fx.db
                .scalar("SELECT v FROM t WHERE id = 1")
                .await
                .as_deref(),
            Some("a"),
            "{label}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn key_text_is_read_in_the_browse_sessions_time_zone() {
    for (label, fx) in my_fixtures("").await {
        fx.exec(
            "CREATE TABLE ts (t TIMESTAMP PRIMARY KEY, n INT); \
            SET time_zone = '+00:00'; INSERT INTO ts VALUES ('2024-01-01 00:00:00', 1);",
        )
        .await;
        let target = target_after(&fx, "SET time_zone = '+05:00'", "ts").await;
        let change = Change::Update {
            key: row_key(&[("t", val("2024-01-01 05:00:00"))]),
            set: pairs(&[("n", val("2"))]),
            expected: vec![],
        };
        fx.apply_changes(&target, vec![change])
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
        assert_eq!(
            fx.db.scalar("SELECT n FROM ts").await.as_deref(),
            Some("2"),
            "{label}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_update_returns_the_key_the_database_stored() {
    for (label, fx) in my_fixtures("").await {
        fx.exec("CREATE TABLE k (id INT PRIMARY KEY, n INT); INSERT INTO k VALUES (1, 1); \
            CREATE TABLE bk (k VARBINARY(4) PRIMARY KEY, n INT); INSERT INTO bk VALUES (X'01', 1); \
            CREATE TABLE pk2 (a INT, b VARCHAR(3), n INT, PRIMARY KEY (a, b)); INSERT INTO pk2 VALUES (1, 'x', 1);").await;
        let cases = [
            (
                "k",
                row_key(&[("id", val("1"))]),
                pairs(&[("id", val("010"))]),
                row_key(&[("id", val("10"))]),
            ),
            (
                "bk",
                row_key(&[("k", EditValue::Bytes(vec![1]))]),
                pairs(&[("k", EditValue::Bytes(vec![0, 0xff]))]),
                row_key(&[("k", EditValue::Bytes(vec![0, 0xff]))]),
            ),
            (
                "pk2",
                row_key(&[("a", val("1")), ("b", val("x"))]),
                pairs(&[("a", val("007"))]),
                row_key(&[("a", val("7")), ("b", val("x"))]),
            ),
        ];
        for (table, key, set, stored) in cases {
            let target = fx.target(table).await;
            let applied = fx
                .apply_changes(
                    &target,
                    vec![Change::Update {
                        key,
                        set,
                        expected: vec![],
                    }],
                )
                .await
                .unwrap();
            assert_eq!(applied.written_key(0), Some(&stored), "{label} {table}");
            applied.commit().await.unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_note_does_not_fail_a_change() {
    for (label, fx) in my_fixtures("").await {
        fx.exec(
            "CREATE TABLE d (k INT PRIMARY KEY, dc DECIMAL(5,2)); INSERT INTO d VALUES (1, 0);",
        )
        .await;
        let target = fx.target("d").await;
        let change = Change::Update {
            key: row_key(&[("k", val("1"))]),
            set: pairs(&[("dc", val("1.005"))]),
            expected: vec![],
        };
        fx.apply_changes(&target, vec![change])
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
        assert_eq!(
            fx.db.scalar("SELECT dc FROM d").await.as_deref(),
            Some("1.01"),
            "{label}"
        );
    }
}
