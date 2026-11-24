//! Guarded edits on Postgres: output-text compare, locks, isolation and privileges.
use std::time::{Duration, Instant};

use crate::rowedit_pg_torture::TORTURE;
use crate::support::{PgFixture, loaded, pairs, pg_fixture, rejected, row_key, val};
use dbento_core::{Change, ChangeOutcome};

const SETUP: &str = "
CREATE TABLE t(id int PRIMARY KEY, v text, n int);
INSERT INTO t VALUES (1, 'a', 10), (2, 'b', 20), (3, 'c', 30);
CREATE TABLE nokey(a int, b text);
INSERT INTO nokey VALUES (1, 'x');
";
/// A composite whose fields are all NULL is not a NULL datum: the browse shows `(,)`.
pub(crate) const COMPOSITE: &str = "
CREATE TYPE duo AS (a int, b text);
CREATE TABLE cn(id int PRIMARY KEY, c duo, cs duo[]);
INSERT INTO cn VALUES (1, ROW(NULL, NULL), ARRAY[ROW(NULL, NULL)::duo]), (2, NULL, NULL);
";

async fn setup() -> PgFixture {
    let fx = pg_fixture("").await;
    fx.db.admin().await.batch_execute(SETUP).await.unwrap();
    fx
}

async fn scalar(fx: &PgFixture, sql: &str) -> String {
    fx.db.scalar(sql).await.unwrap()
}

fn update_v(id: &str, v: &str, was: &str) -> Change {
    Change::Update {
        key: row_key(&[("id", val(id))]),
        set: pairs(&[("v", val(v))]),
        expected: pairs(&[("v", val(was))]),
    }
}

#[tokio::test]
async fn a_row_changed_after_staging_is_a_conflict_and_keeps_the_other_write() {
    let fx = setup().await;
    let target = fx.target("public", "t").await;
    fx.db
        .admin()
        .await
        .batch_execute("UPDATE t SET v = 'theirs' WHERE id = 1")
        .await
        .unwrap();
    let r = fx
        .apply_changes(
            &target,
            vec![update_v("1", "mine", "a"), update_v("2", "mine", "b")],
        )
        .await;
    assert_eq!(
        rejected(r),
        vec![ChangeOutcome::Conflict, ChangeOutcome::Applied(1)]
    );
    assert_eq!(
        scalar(&fx, "SELECT string_agg(v, ',' ORDER BY id) FROM t").await,
        "theirs,b,c"
    );
}

#[tokio::test]
async fn the_type_torture_row_guards_against_its_own_loaded_text() {
    let fx = pg_fixture("").await;
    fx.db.admin().await.batch_execute(TORTURE).await.unwrap();
    let target = fx.target("public", "tt").await;
    let row = loaded(&target, &fx.row("SELECT * FROM tt WHERE id = 1").await);
    let rest = row[1..].to_vec();
    let applied = fx
        .apply_changes(
            &target,
            vec![Change::Update {
                key: row_key(&[("id", val("1"))]),
                set: rest.clone(),
                expected: rest,
            }],
        )
        .await
        .unwrap();
    applied.commit().await.unwrap();
}

async fn composite_rows(
    fx: &PgFixture,
    target: &dbento_core::EditTarget,
) -> Vec<Vec<(String, dbento_core::EditValue)>> {
    let mut rows = Vec::new();
    for id in ["1", "2"] {
        rows.push(loaded(
            target,
            &fx.row(&format!("SELECT * FROM cn WHERE id = {id}")).await,
        ));
    }
    assert_eq!(
        rows[0][1].1,
        val("(,)"),
        "an all-NULL composite browses as text"
    );
    assert_eq!(
        rows[1][1].1,
        dbento_core::EditValue::Null,
        "a NULL composite browses as NULL"
    );
    rows
}

#[tokio::test]
async fn an_all_null_composite_guards_as_its_text_not_as_null() {
    let fx = pg_fixture("").await;
    fx.db.admin().await.batch_execute(COMPOSITE).await.unwrap();
    let target = fx.target("public", "cn").await;
    let rows = composite_rows(&fx, &target).await;
    let changes = rows
        .iter()
        .map(|row| Change::Update {
            key: row_key(&[("id", row[0].1.clone())]),
            set: row[1..].to_vec(),
            expected: row[1..].to_vec(),
        })
        .collect();
    let applied = fx.apply_changes(&target, changes).await.unwrap();
    applied.rollback().await.unwrap();
}

#[tokio::test]
async fn a_keyless_table_guards_through_its_ctid_locator() {
    let fx = setup().await;
    let target = fx.target("public", "nokey").await;
    let ctid = scalar(&fx, "SELECT ctid FROM nokey").await;
    let xmin = scalar(&fx, "SELECT xmin FROM nokey").await;
    let applied = fx
        .apply_changes(
            &target,
            vec![Change::Update {
                key: row_key(&[("ctid", val(&ctid)), ("xmin", val(&xmin))]),
                set: pairs(&[("b", val("z"))]),
                expected: pairs(&[("a", val("1")), ("b", val("x"))]),
            }],
        )
        .await
        .unwrap();
    applied.commit().await.unwrap();
    assert_eq!(scalar(&fx, "SELECT b FROM nokey").await, "z");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_repeatable_read_default_still_reports_a_race_as_a_conflict() {
    let fx = setup().await;
    let db = fx.db.database().to_owned();
    fx.db
        .admin()
        .await
        .batch_execute(&format!(
            "ALTER DATABASE \"{db}\" SET default_transaction_isolation = 'repeatable read'"
        ))
        .await
        .unwrap();
    let target = fx.target("public", "t").await;
    let blocker = fx.db.admin().await;
    blocker
        .batch_execute("BEGIN; UPDATE t SET v = 'z' WHERE id = 1")
        .await
        .unwrap();
    // The other write commits only once the apply waits on its row lock, after its snapshot.
    let release = async {
        let deadline = Instant::now() + Duration::from_secs(4);
        while scalar(
            &fx,
            "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock'",
        )
        .await
            == "0"
        {
            assert!(
                Instant::now() < deadline,
                "the apply never waited on the row lock"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        blocker.batch_execute("COMMIT").await.unwrap();
    };
    let (r, ()) = tokio::join!(
        fx.apply_changes(&target, vec![update_v("1", "mine", "a")]),
        release
    );
    assert_eq!(rejected(r), vec![ChangeOutcome::Conflict]);
    assert_eq!(scalar(&fx, "SELECT v FROM t WHERE id = 1").await, "z");
}

fn delete_t1(was_n: &str) -> Change {
    Change::Delete {
        key: row_key(&[("id", val("1"))]),
        expected: pairs(&[("id", val("1")), ("v", val("a")), ("n", val(was_n))]),
    }
}

#[tokio::test]
async fn a_guarded_delete_of_a_changed_row_conflicts_and_the_row_survives() {
    let fx = setup().await;
    let target = fx.target("public", "t").await;
    fx.db
        .admin()
        .await
        .batch_execute("UPDATE t SET n = 11 WHERE id = 1")
        .await
        .unwrap();
    let r = fx.apply_changes(&target, vec![delete_t1("10")]).await;
    assert_eq!(rejected(r), vec![ChangeOutcome::Conflict]);
    assert_eq!(
        scalar(&fx, "SELECT v || n FROM t WHERE id = 1").await,
        "a11"
    );
}

#[tokio::test]
async fn the_type_torture_row_deletes_against_its_loaded_text() {
    let fx = pg_fixture("").await;
    fx.db.admin().await.batch_execute(TORTURE).await.unwrap();
    let target = fx.target("public", "tt").await;
    let row = loaded(&target, &fx.row("SELECT * FROM tt WHERE id = 1").await);
    let applied = fx
        .apply_changes(
            &target,
            vec![Change::Delete {
                key: row_key(&[("id", val("1"))]),
                expected: row,
            }],
        )
        .await
        .unwrap();
    applied.commit().await.unwrap();
    assert_eq!(
        scalar(&fx, "SELECT count(*) FROM tt WHERE id = 1").await,
        "0"
    );
}

#[tokio::test]
async fn an_all_null_composite_deletes_against_its_text() {
    let fx = pg_fixture("").await;
    fx.db.admin().await.batch_execute(COMPOSITE).await.unwrap();
    let target = fx.target("public", "cn").await;
    let changes = composite_rows(&fx, &target)
        .await
        .into_iter()
        .map(|row| Change::Delete {
            key: row_key(&[("id", row[0].1.clone())]),
            expected: row,
        })
        .collect();
    let applied = fx.apply_changes(&target, changes).await.unwrap();
    applied.commit().await.unwrap();
    assert_eq!(scalar(&fx, "SELECT count(*) FROM cn").await, "0");
}

/// Points `fx` at `user`, who has the admin's password.
fn as_user(fx: &mut PgFixture, user: &str) {
    let path = fx.dir.path().join("dbento.toml");
    let toml = std::fs::read_to_string(&path).unwrap();
    let admin = format!("user = \"{}\"", fx.db.user());
    assert!(toml.contains(&admin), "{toml}");
    std::fs::write(&path, toml.replace(&admin, &format!("user = \"{user}\""))).unwrap();
    fx.ws = dbento_core::Workspace::open(fx.dir.path()).unwrap();
}

#[tokio::test]
async fn a_role_with_select_and_delete_but_no_update_deletes_a_guarded_row() {
    let mut fx = setup().await;
    let role = format!("del_{}", fx.db.database());
    fx.db
        .admin()
        .await
        .batch_execute(&format!(
            "DROP ROLE IF EXISTS \"{role}\"; CREATE ROLE \"{role}\" LOGIN PASSWORD '{}'; \
             GRANT SELECT, DELETE ON t TO \"{role}\"",
            fx.db.password()
        ))
        .await
        .unwrap();
    as_user(&mut fx, &role);
    let target = fx.target("public", "t").await;
    let r = fx.apply_changes(&target, vec![delete_t1("10")]).await;
    let committed = match r {
        Ok(applied) => applied.commit().await.map_err(|e| e.to_string()),
        Err(e) => Err(format!("{e:?}")),
    };
    let left = scalar(&fx, "SELECT string_agg(id::text, ',' ORDER BY id) FROM t").await;
    fx.db
        .admin()
        .await
        .batch_execute(&format!(
            "REVOKE ALL ON t FROM \"{role}\"; DROP ROLE \"{role}\""
        ))
        .await
        .unwrap();
    committed.unwrap();
    assert_eq!(left, "2,3");
}

#[tokio::test]
async fn a_guarded_delete_on_a_table_with_an_instead_rule_fails_and_rolls_back() {
    let fx = setup().await;
    fx.db
        .admin()
        .await
        .batch_execute("CREATE RULE keep AS ON DELETE TO t DO INSTEAD NOTHING")
        .await
        .unwrap();
    let target = fx.target("public", "t").await;
    let outcomes = rejected(fx.apply_changes(&target, vec![delete_t1("10")]).await);
    assert!(
        matches!(outcomes[..], [ChangeOutcome::Failed(_)]),
        "{outcomes:?}"
    );
    assert_eq!(scalar(&fx, "SELECT count(*) FROM t").await, "3");
}
