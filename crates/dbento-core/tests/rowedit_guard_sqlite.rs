//! Guarded edits on SQLite: an Update or Delete applies only while the row holds the loaded values.
use crate::support::{Fixture, loaded, pairs, rejected, row_key, sqlite_fixture, val};
use dbento_core::{Change, ChangeOutcome, EditValue};

pub(crate) const SETUP: &str = "
CREATE TABLE audit_log(at TEXT, detail TEXT);
INSERT INTO audit_log VALUES ('2024-01-01', 'orig'), ('2024-01-02', 'other');
CREATE TABLE odd(id INTEGER PRIMARY KEY, r REAL, u, b TEXT, bl BLOB);
INSERT INTO odd VALUES (1, 0.1, 7, CAST(x'ff00' AS TEXT), x'00ff');
";

fn setup() -> Fixture {
    sqlite_fixture(SETUP, "")
}

/// A write through a separate connection, as another client makes it.
fn outside(fx: &Fixture, sql: &str) {
    rusqlite::Connection::open(fx.db_path())
        .unwrap()
        .execute_batch(sql)
        .unwrap();
}

fn update(rowid: &str, detail: &str, was: &str) -> Change {
    Change::Update {
        key: row_key(&[("rowid", val(rowid))]),
        set: pairs(&[("detail", val(detail))]),
        expected: pairs(&[("detail", val(was))]),
    }
}

#[tokio::test]
async fn a_row_changed_after_staging_is_a_conflict_and_keeps_the_other_write() {
    let fx = setup();
    let target = fx.target("audit_log").await;
    outside(
        &fx,
        "UPDATE audit_log SET detail = 'theirs' WHERE rowid = 1",
    );
    let r = fx
        .apply_changes(
            &target,
            vec![update("1", "mine", "orig"), update("2", "mine", "other")],
        )
        .await;
    assert_eq!(
        rejected(r),
        vec![ChangeOutcome::Conflict, ChangeOutcome::Applied(1)]
    );
    assert_eq!(
        fx.text("SELECT group_concat(detail, ',' ORDER BY rowid) FROM audit_log"),
        "theirs,other"
    );
}

#[tokio::test]
async fn a_change_to_a_column_the_update_does_not_set_survives_it() {
    let fx = setup();
    let target = fx.target("audit_log").await;
    outside(
        &fx,
        "UPDATE audit_log SET at = '2099-01-01' WHERE rowid = 1",
    );
    let applied = fx
        .apply_changes(&target, vec![update("1", "mine", "orig")])
        .await
        .unwrap();
    applied.commit().await.unwrap();
    assert_eq!(
        fx.text("SELECT at || '/' || detail FROM audit_log WHERE rowid = 1"),
        "2099-01-01/mine"
    );
}

#[tokio::test]
async fn a_guarded_delete_conflicts_on_a_change_to_any_column() {
    let fx = setup();
    let target = fx.target("audit_log").await;
    outside(
        &fx,
        "UPDATE audit_log SET at = '2099-01-01' WHERE rowid = 1",
    );
    let r = fx
        .apply_changes(
            &target,
            vec![Change::Delete {
                key: row_key(&[("rowid", val("1"))]),
                expected: pairs(&[("at", val("2024-01-01")), ("detail", val("orig"))]),
            }],
        )
        .await;
    assert_eq!(rejected(r), vec![ChangeOutcome::Conflict]);
    assert_eq!(fx.count("SELECT count(*) FROM audit_log"), 2);
}

#[tokio::test]
async fn real_untyped_and_non_utf8_values_guard_without_a_false_conflict() {
    let fx = setup();
    let target = fx.target("odd").await;
    let row = loaded(&target, &fx.row("SELECT * FROM odd").await);
    assert_eq!(row[1].1, val("0.1"));
    assert_eq!(
        row[3].1,
        EditValue::Bytes(vec![0xff, 0x00]),
        "non-UTF-8 text loads as bytes"
    );
    let set = row[1..].to_vec();
    let applied = fx
        .apply_changes(
            &target,
            vec![
                Change::Update {
                    key: row_key(&[("id", val("1"))]),
                    set: set.clone(),
                    expected: set,
                },
                Change::Delete {
                    key: row_key(&[("id", val("1"))]),
                    expected: row,
                },
            ],
        )
        .await
        .unwrap();
    applied.commit().await.unwrap();
    assert_eq!(fx.count("SELECT count(*) FROM odd"), 0);
}
