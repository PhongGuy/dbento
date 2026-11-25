//! Applying staged edits to MySQL and MariaDB: commit, roll back, conflicts, inserts and deletes.
use crate::support::{MyFixture, my_fixtures, pairs, rejected, row_key, val};
use dbento_core::{ApplyError, Change, ChangeOutcome, EditValue};
use mysql_async::prelude::*;

const SETUP: &str = "CREATE TABLE t (id INT PRIMARY KEY, v VARCHAR(3), n INT) ENGINE=InnoDB;
INSERT INTO t VALUES (1, 'a', 10), (2, 'b', 20), (3, 'c', 30);
CREATE TABLE ai (id INT AUTO_INCREMENT PRIMARY KEY, n INT DEFAULT 3) ENGINE=InnoDB;";
const ORIGINAL: &str = "1,a,10 2,b,20 3,c,30";

async fn rows(fx: &MyFixture) -> String {
    fx.db
        .scalar("SELECT GROUP_CONCAT(CONCAT_WS(',', id, v, n) ORDER BY id SEPARATOR ' ') FROM t")
        .await
        .unwrap()
}

fn update(id: &str, set: &[(&str, EditValue)]) -> Change {
    Change::Update {
        key: row_key(&[("id", val(id))]),
        set: pairs(set),
        expected: vec![],
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn updates_including_a_same_value_one_commit_one_row_each() {
    for (label, fx) in my_fixtures("").await {
        fx.exec(SETUP).await;
        let target = fx.target("t").await;
        let changes = vec![
            update("1", &[("v", val("x"))]),
            update("2", &[("n", val("21"))]),
            update("3", &[("n", val("30"))]),
        ];
        let applied = fx.apply_changes(&target, changes).await.unwrap();
        assert_eq!(applied.affected(), 3, "{label}");
        applied.commit().await.unwrap();
        assert_eq!(rows(&fx).await, "1,x,10 2,b,21 3,c,30", "{label}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rollback_and_drop_leave_the_table_unchanged_and_unlocked() {
    for (label, fx) in my_fixtures("").await {
        fx.exec(SETUP).await;
        let target = fx.target("t").await;
        let applied = fx
            .apply_changes(&target, vec![update("1", &[("v", val("x"))])])
            .await
            .unwrap();
        applied.rollback().await.unwrap();
        assert_eq!(rows(&fx).await, ORIGINAL, "{label}");
        // A dropped apply holds row 1's lock until the server sees its connection go; a locking
        // read waits for that rollback and then reads the committed value.
        let applied = fx
            .apply_changes(&target, vec![update("1", &[("v", val("y"))])])
            .await
            .unwrap();
        drop(applied);
        let mut probe = fx.db.admin().await;
        probe
            .query_drop("SET SESSION innodb_lock_wait_timeout = 15")
            .await
            .unwrap();
        let v: Option<String> = probe
            .query_first("SELECT v FROM t WHERE id = 1 FOR UPDATE")
            .await
            .unwrap();
        assert_eq!(v.as_deref(), Some("a"), "{label}");
        probe.disconnect().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_row_deleted_before_apply_is_a_conflict_and_nothing_commits() {
    for (label, fx) in my_fixtures("").await {
        fx.exec(SETUP).await;
        let target = fx.target("t").await;
        fx.exec("DELETE FROM t WHERE id = 2").await;
        let r = fx
            .apply_changes(
                &target,
                vec![
                    update("1", &[("v", val("x"))]),
                    update("2", &[("v", val("y"))]),
                ],
            )
            .await;
        assert_eq!(
            rejected(r),
            [ChangeOutcome::Applied(1), ChangeOutcome::Conflict],
            "{label}"
        );
        assert_eq!(rows(&fx).await, "1,a,10 3,c,30", "{label}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_change_stops_the_rest_and_nothing_commits() {
    for (label, fx) in my_fixtures("").await {
        fx.exec(SETUP).await;
        let target = fx.target("t").await;
        let r = fx
            .apply_changes(
                &target,
                vec![
                    update("1", &[("v", val("toolong"))]),
                    update("2", &[("v", val("y"))]),
                ],
            )
            .await;
        match rejected(r).as_slice() {
            [ChangeOutcome::Failed(m), ChangeOutcome::NotAttempted] => {
                assert!(m.contains("Data too long"), "{label}: {m}")
            }
            other => panic!("{label}: {other:?}"),
        }
        assert_eq!(rows(&fx).await, ORIGINAL, "{label}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn inserts_deletes_and_an_all_default_insert_commit() {
    for (label, fx) in my_fixtures("").await {
        fx.exec(SETUP).await;
        let target = fx.target("t").await;
        let changes = vec![
            Change::Insert {
                values: pairs(&[("id", val("4")), ("v", val("d")), ("n", val("40"))]),
            },
            Change::Delete {
                key: row_key(&[("id", val("3"))]),
                expected: vec![],
            },
        ];
        fx.apply_changes(&target, changes)
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
        assert_eq!(rows(&fx).await, "1,a,10 2,b,20 4,d,40", "{label}");
        let ai = fx.target("ai").await;
        let all_default = vec![Change::Insert {
            values: pairs(&[("n", EditValue::Default)]),
        }];
        fx.apply_changes(&ai, all_default)
            .await
            .unwrap()
            .commit()
            .await
            .unwrap();
        assert_eq!(
            fx.db
                .scalar("SELECT CONCAT(id, ',', n) FROM ai")
                .await
                .as_deref(),
            Some("1,3"),
            "{label}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_only_connection_is_refused() {
    for (label, fx) in my_fixtures("read_only = true").await {
        fx.exec(SETUP).await;
        let target = fx.target("t").await;
        let r = fx
            .apply_changes(&target, vec![update("1", &[("v", val("x"))])])
            .await;
        assert!(matches!(r, Err(ApplyError::ReadOnly)), "{label}: {r:?}");
    }
}
