//! Guarded edits on MySQL and MariaDB: CAST text compare, case and pad-space changes, triggers, privileges.
use crate::support::{MyFixture, loaded, my_fixtures, pairs, rejected, row_key, val};
use dbento_core::{Change, ChangeOutcome};

const SETUP: &str = "CREATE TABLE t (id INT PRIMARY KEY, v VARCHAR(10) COLLATE utf8mb4_general_ci, n INT) ENGINE=InnoDB;
INSERT INTO t VALUES (1, 'abc', 10), (2, 'abc', 20), (3, 'abc', 30);";
pub(crate) const TYPES: &str = "CREATE TABLE ty (id INT PRIMARY KEY, f FLOAT, dbl DOUBLE, j JSON, b8 BIT(8),
  g GEOMETRY, dec30 DECIMAL(30,10), ts TIMESTAMP(6) NULL, vb VARBINARY(8)) ENGINE=InnoDB;
INSERT INTO ty VALUES (1, 1.1, 0.1, '{\"b\": null, \"a\": [1, 2.50]}', b'101', ST_GeomFromText('POINT(1 2)'),
  12345678901234567890.0123456789, '2024-01-02 03:04:05.6', x'00ff');";

fn update_v(id: &str, v: &str, was: &str) -> Change {
    Change::Update {
        key: row_key(&[("id", val(id))]),
        set: pairs(&[("v", val(v))]),
        expected: pairs(&[("v", val(was))]),
    }
}

async fn vs(fx: &MyFixture) -> String {
    fx.db
        .scalar("SELECT GROUP_CONCAT(CONCAT('[', v, ']') ORDER BY id) FROM t")
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_row_changed_after_staging_is_a_conflict_and_keeps_the_other_write() {
    for (label, fx) in my_fixtures("").await {
        fx.exec(SETUP).await;
        let target = fx.target("t").await;
        fx.exec("UPDATE t SET v = 'theirs' WHERE id = 1").await;
        let r = fx
            .apply_changes(
                &target,
                vec![update_v("1", "mine", "abc"), update_v("2", "mine", "abc")],
            )
            .await;
        assert_eq!(
            rejected(r),
            vec![ChangeOutcome::Conflict, ChangeOutcome::Applied(1)],
            "{label}"
        );
        assert_eq!(vs(&fx).await, "[theirs],[abc],[abc]", "{label}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn case_only_and_trailing_space_changes_are_caught() {
    for (label, fx) in my_fixtures("").await {
        fx.exec(SETUP).await;
        let target = fx.target("t").await;
        fx.exec("UPDATE t SET v = 'ABC' WHERE id = 1; UPDATE t SET v = 'abc ' WHERE id = 2")
            .await;
        let r = fx
            .apply_changes(
                &target,
                vec![update_v("1", "mine", "abc"), update_v("2", "mine", "abc")],
            )
            .await;
        assert_eq!(
            rejected(r),
            vec![ChangeOutcome::Conflict, ChangeOutcome::Conflict],
            "{label}"
        );
        assert_eq!(vs(&fx).await, "[ABC],[abc ],[abc]", "{label}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn float_json_bit_geometry_decimal_and_timestamp_guard_without_a_false_conflict() {
    for (label, fx) in my_fixtures("").await {
        fx.exec(TYPES).await;
        let target = fx.target("ty").await;
        let row = loaded(&target, &fx.row("SELECT * FROM ty").await);
        let rest = row[1..].to_vec();
        let key = || row_key(&[("id", val("1"))]);
        let applied = fx
            .apply_changes(
                &target,
                vec![Change::Update {
                    key: key(),
                    set: rest.clone(),
                    expected: rest,
                }],
            )
            .await
            .unwrap_or_else(|e| panic!("{label}: {e:?}"));
        applied.rollback().await.unwrap();
        let applied = fx
            .apply_changes(
                &target,
                vec![Change::Delete {
                    key: key(),
                    expected: row,
                }],
            )
            .await
            .unwrap_or_else(|e| panic!("{label}: {e:?}"));
        applied.commit().await.unwrap();
        assert_eq!(
            fx.db.scalar("SELECT COUNT(*) FROM ty").await.as_deref(),
            Some("0"),
            "{label}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_on_update_timestamp_is_guarded_and_a_fresh_browse_applies() {
    for (label, fx) in my_fixtures("").await {
        fx.exec(
            "CREATE TABLE st (id INT PRIMARY KEY, v VARCHAR(10), ts TIMESTAMP(6) NOT NULL \
                 DEFAULT '2000-01-01 00:00:00' ON UPDATE CURRENT_TIMESTAMP(6)) ENGINE=InnoDB; \
                 INSERT INTO st VALUES (1, 'a', '2000-01-01 00:00:00')",
        )
        .await;
        let target = fx.target("st").await;
        let first = loaded(&target, &fx.row("SELECT * FROM st").await);
        let key = || row_key(&[("id", val("1"))]);
        let applied = fx
            .apply_changes(
                &target,
                vec![Change::Update {
                    key: key(),
                    set: pairs(&[("v", val("b"))]),
                    expected: pairs(&[("v", val("a"))]),
                }],
            )
            .await
            .unwrap();
        applied.commit().await.unwrap();
        // The stale row differs from the stored one only in the trigger-set `ts`.
        let mut stale_row = first;
        stale_row[1].1 = val("b");
        let stale = fx
            .apply_changes(
                &target,
                vec![Change::Delete {
                    key: key(),
                    expected: stale_row,
                }],
            )
            .await;
        assert_eq!(
            rejected(stale),
            vec![ChangeOutcome::Conflict],
            "{label}: ts moved on update"
        );
        let fresh = loaded(&target, &fx.row("SELECT * FROM st").await);
        let applied = fx
            .apply_changes(
                &target,
                vec![Change::Delete {
                    key: key(),
                    expected: fresh,
                }],
            )
            .await
            .unwrap_or_else(|e| panic!("{label}: {e:?}"));
        applied.commit().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_user_with_select_and_delete_but_no_update_deletes_a_guarded_row() {
    for (label, mut fx) in my_fixtures("").await {
        fx.exec(SETUP).await;
        let db = fx.db.database().to_owned();
        let user = format!("del_{db}");
        fx.exec(&format!(
            "DROP USER IF EXISTS '{user}'@'%'; CREATE USER '{user}'@'%' IDENTIFIED BY '{}'; \
             GRANT SELECT, DELETE ON `{db}`.`t` TO '{user}'@'%'",
            fx.db.password()
        ))
        .await;
        let path = fx.dir.path().join("dbento.toml");
        let toml = std::fs::read_to_string(&path).unwrap();
        let admin = format!("user = \"{}\"", fx.db.user());
        assert!(toml.contains(&admin), "{toml}");
        std::fs::write(&path, toml.replace(&admin, &format!("user = \"{user}\""))).unwrap();
        fx.ws = dbento_core::Workspace::open(fx.dir.path()).unwrap();
        let target = fx.target("t").await;
        let r = fx
            .apply_changes(
                &target,
                vec![Change::Delete {
                    key: row_key(&[("id", val("1"))]),
                    expected: pairs(&[("id", val("1")), ("v", val("abc")), ("n", val("10"))]),
                }],
            )
            .await;
        let committed = match r {
            Ok(applied) => applied.commit().await.map_err(|e| e.to_string()),
            Err(e) => Err(format!("{e:?}")),
        };
        fx.exec(&format!("DROP USER '{user}'@'%'")).await;
        committed.unwrap_or_else(|e| panic!("{label}: {e}"));
        assert_eq!(
            fx.db.scalar("SELECT COUNT(*) FROM t").await.as_deref(),
            Some("2"),
            "{label}"
        );
    }
}
