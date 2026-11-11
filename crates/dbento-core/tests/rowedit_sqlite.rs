use std::time::{Duration, Instant};

use crate::support::{Cell, Fixture, edit_value, pairs, rejected, row_key, sqlite_fixture, val};
use dbento_core::{
    ApplyError, Change, ChangeOutcome, EditBatch, EditValue, RowKey, apply, render_sql,
};

const SETUP: &str = "
CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT DEFAULT 'x''y', n INT, pic BLOB,
  g INT GENERATED ALWAYS AS (n * 2) VIRTUAL);
INSERT INTO t(id, name, n) VALUES (1, 'a', 10), (2, 'b', 20), (3, 'c', 30);
CREATE TABLE plain(a, b);
INSERT INTO plain VALUES (1, 'p'), (2, 'q');
CREATE TABLE shadow(\"RowId\" TEXT, b);
INSERT INTO shadow VALUES ('r', 1);
CREATE TABLE nk(k TEXT PRIMARY KEY, v);
INSERT INTO nk VALUES (NULL, 1), (NULL, 2);
CREATE TABLE strict_t(id INTEGER PRIMARY KEY, n INTEGER) STRICT;
INSERT INTO strict_t VALUES (1, 1), (2, 2);
";
const SNAP: &str = "SELECT group_concat(id || ':' || coalesce(name, '-') || ':' || coalesce(n, '-') || ':' \
|| iif(pic IS NULL, '-', hex(pic)) || ':' || coalesce(g, '-'), ' ' ORDER BY id) FROM t";
const ORIGINAL: &str = "1:a:10:-:20 2:b:20:-:40 3:c:30:-:60";

fn setup() -> Fixture {
    sqlite_fixture(SETUP, "")
}

fn update(id: &str, set: &[(&str, EditValue)]) -> Change {
    Change::Update {
        key: row_key(&[("id", val(id))]),
        set: pairs(set),
    }
}

fn changes_for_t() -> Vec<Change> {
    vec![
        update(
            "1",
            &[
                ("name", val("it's")),
                ("pic", EditValue::Bytes(vec![0, 255])),
                ("g", val("0")),
            ],
        ),
        Change::Insert {
            values: pairs(&[("id", val("4")), ("n", val("40")), ("g", val("1"))]),
        },
        Change::Delete {
            key: row_key(&[("id", val("2"))]),
        },
    ]
}

#[tokio::test]
async fn updates_inserts_and_deletes_commit_on_sqlite() {
    let fx = setup();
    let target = fx.target("t").await;
    let applied = fx.apply_changes(&target, changes_for_t()).await.unwrap();
    assert_eq!(applied.affected(), 3);
    applied.commit().await.unwrap();
    assert_eq!(fx.text(SNAP), "1:it's:10:00FF:20 3:c:30:-:60 4:x'y:40:-:80");
}

#[tokio::test]
async fn default_is_the_declared_default_or_null() {
    let fx = setup();
    let target = fx.target("t").await;
    let applied = fx
        .apply_changes(
            &target,
            vec![
                update(
                    "1",
                    &[("name", EditValue::Default), ("n", EditValue::Default)],
                ),
                Change::Insert { values: vec![] },
            ],
        )
        .await
        .unwrap();
    applied.commit().await.unwrap();
    assert_eq!(
        fx.text(SNAP),
        "1:x'y:-:-:- 2:b:20:-:40 3:c:30:-:60 4:x'y:-:-:-"
    );
}

#[tokio::test]
async fn rowid_tables_are_edited_through_their_alias() {
    let fx = setup();
    let plain = fx.target("plain").await;
    let applied = fx
        .apply_changes(
            &plain,
            vec![Change::Update {
                key: row_key(&[("rowid", val("1"))]),
                set: pairs(&[("b", val("z"))]),
            }],
        )
        .await
        .unwrap();
    applied.commit().await.unwrap();
    let shadow = fx.target("shadow").await;
    let applied = fx
        .apply_changes(
            &shadow,
            vec![Change::Update {
                key: row_key(&[("_rowid_", val("1"))]),
                set: pairs(&[("b", val("2"))]),
            }],
        )
        .await
        .unwrap();
    applied.commit().await.unwrap();
    assert_eq!(
        fx.text("SELECT group_concat(b, ',' ORDER BY a) FROM plain"),
        "z,q"
    );
    assert_eq!(fx.text("SELECT RowId || '/' || b FROM shadow"), "r/2");
}

#[tokio::test]
async fn null_text_keys_match_together_and_are_ambiguous() {
    let fx = setup();
    let target = fx.target("nk").await;
    let outcomes = rejected(
        fx.apply_changes(
            &target,
            vec![Change::Update {
                key: row_key(&[("k", EditValue::Null)]),
                set: pairs(&[("v", val("9"))]),
            }],
        )
        .await,
    );
    assert_eq!(outcomes, vec![ChangeOutcome::Ambiguous(2)]);
    assert_eq!(
        fx.text("SELECT group_concat(v, ',' ORDER BY v) FROM nk"),
        "1,2"
    );
}

#[tokio::test]
async fn a_conflict_and_a_bad_value_roll_back_on_sqlite() {
    let fx = setup();
    let target = fx.target("strict_t").await;
    rusqlite::Connection::open(fx.db_path())
        .unwrap()
        .execute("DELETE FROM strict_t WHERE id = 2", [])
        .unwrap();
    let outcomes = rejected(
        fx.apply_changes(
            &target,
            vec![
                update("1", &[("n", val("5"))]),
                update("2", &[("n", val("6"))]),
            ],
        )
        .await,
    );
    assert_eq!(
        outcomes,
        vec![ChangeOutcome::Applied(1), ChangeOutcome::Conflict]
    );
    let outcomes = rejected(
        fx.apply_changes(
            &target,
            vec![
                update("1", &[("n", val("abc"))]),
                update("1", &[("n", val("7"))]),
            ],
        )
        .await,
    );
    match outcomes.as_slice() {
        [ChangeOutcome::Failed(m), ChangeOutcome::NotAttempted] => {
            assert!(
                m.contains("cannot store TEXT value in INTEGER column"),
                "{m}"
            )
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        fx.text("SELECT group_concat(id || '=' || n, ',') FROM strict_t"),
        "1=1"
    );
}

#[tokio::test]
async fn a_write_lock_held_elsewhere_fails_as_database_is_locked() {
    let fx = setup();
    let target = fx.target("t").await;
    let holder = rusqlite::Connection::open(fx.db_path()).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        fx.apply_changes(&target, vec![update("1", &[("n", val("9"))])]),
    )
    .await
    .expect("apply must give up on the lock by itself");
    match rejected(result).as_slice() {
        [ChangeOutcome::Failed(m)] => assert!(m.contains("database is locked"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(
        started.elapsed() >= Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    holder.execute_batch("ROLLBACK").unwrap();
    assert_eq!(fx.text(SNAP), ORIGINAL);
}

#[tokio::test]
async fn dropping_applied_rolls_back_on_sqlite() {
    let fx = setup();
    let target = fx.target("t").await;
    let applied = fx
        .apply_changes(&target, vec![update("1", &[("name", val("gone"))])])
        .await
        .unwrap();
    drop(applied);
    // This write waits on the busy timeout until the dropped session's connection closes.
    rusqlite::Connection::open(fx.db_path())
        .unwrap()
        .execute("UPDATE t SET n = 11 WHERE id = 1", [])
        .unwrap();
    assert_eq!(fx.text(SNAP), "1:a:11:-:22 2:b:20:-:40 3:c:30:-:60");
}

#[tokio::test]
async fn render_sql_run_directly_matches_apply_on_sqlite() {
    let applied_fx = setup();
    let target = applied_fx.target("t").await;
    let mut changes = changes_for_t();
    changes.push(update(
        "3",
        &[("name", EditValue::Default), ("n", EditValue::Null)],
    ));
    let batch = EditBatch::new(target, changes).unwrap();
    let script = render_sql(&batch);
    let applied = apply(
        &applied_fx.ws,
        applied_fx.ws.declared("lite").unwrap(),
        None,
        batch,
    )
    .await
    .unwrap();
    applied.commit().await.unwrap();
    let scripted_fx = setup();
    rusqlite::Connection::open(scripted_fx.db_path())
        .unwrap()
        .execute_batch(&script)
        .unwrap();
    assert_eq!(scripted_fx.text(SNAP), applied_fx.text(SNAP));
    assert_eq!(
        applied_fx.text(SNAP),
        "1:it's:10:00FF:20 3:x'y:-:-:- 4:x'y:40:-:80"
    );
}

#[tokio::test]
async fn untyped_keys_match_numbers_by_value() {
    let fx = sqlite_fixture(
        "CREATE TABLE ku(k PRIMARY KEY, v); \
         INSERT INTO ku VALUES (5, 0), (0.30000000000000004, 0), ('abc', 0), (X'0a', 0);",
        "",
    );
    let target = fx.target("ku").await;
    let changes = fx
        .rows("SELECT k FROM ku")
        .await
        .iter()
        .map(|r| Change::Update {
            key: RowKey::new(vec![("k".to_owned(), edit_value(&r[0]))]),
            set: pairs(&[("v", val("1"))]),
        })
        .collect();
    let applied = fx.apply_changes(&target, changes).await.unwrap();
    assert_eq!(applied.affected(), 4);
    applied.commit().await.unwrap();
    assert_eq!(fx.count("SELECT count(*) FROM ku WHERE v = '1'"), 4);
}

const TORTURE: &str = "
CREATE TABLE lt(id INTEGER PRIMARY KEY, r REAL, big INTEGER, flag BOOLEAN, at DATETIME,
  name VARCHAR(10), fp FLOATING POINT, raw, b BLOB);
INSERT INTO lt VALUES
  (1, 0.1, 9223372036854775807, 1, '2024-01-02 03:04:05', 'é ''q'' 👩‍👩‍👧 too long', 1e300, 42, X'00ff'),
  (2, -1e-300, -9223372036854775808, 0, 2460000.5, '', 2.5, 'text', NULL),
  (3, NULL, NULL, NULL, NULL, NULL, NULL, 1.5, X'01'),
  (4, NULL, NULL, NULL, NULL, NULL, NULL, X'02', NULL);
CREATE TABLE kr(k REAL PRIMARY KEY, v);
INSERT INTO kr VALUES (0.1, 0), (1e300, 0);
CREATE TABLE ki(k INT PRIMARY KEY, v);
INSERT INTO ki VALUES (-9223372036854775808, 0), (9223372036854775807, 0);
CREATE TABLE kb(k BOOLEAN PRIMARY KEY, v);
INSERT INTO kb VALUES (1, 0), (0, 0);
CREATE TABLE kd(k DATETIME PRIMARY KEY, v);
INSERT INTO kd VALUES ('2024-01-02 03:04:05', 0), (2460000.5, 0);
CREATE TABLE kv(k VARCHAR(10) PRIMARY KEY, v);
INSERT INTO kv VALUES ('é''q', 0), ('', 0);
";
const TYPES: &str = "typeof(r) || typeof(big) || typeof(flag) || typeof(at) || typeof(name) || typeof(fp) || typeof(b)";

#[tokio::test]
async fn the_sqlite_torture_rows_round_trip_by_text() {
    let fx = sqlite_fixture(TORTURE, "");
    let target = fx.target("lt").await;
    let names: Vec<String> = target
        .columns()
        .iter()
        .map(|c| c.name().to_owned())
        .collect();
    let mut copies = Vec::new();
    for id in 1..=4 {
        let row = fx.row(&format!("SELECT * FROM lt WHERE id = {id}")).await;
        let mut values: Vec<(String, EditValue)> = names
            .iter()
            .zip(&row)
            .skip(1)
            .map(|(n, c)| (n.clone(), edit_value(c)))
            .collect();
        values.insert(0, ("id".to_owned(), val(&(10 + id).to_string())));
        copies.push(Change::Insert { values });
    }
    fx.apply_changes(&target, copies)
        .await
        .unwrap()
        .commit()
        .await
        .unwrap();
    for id in 1..=4 {
        let source = fx.row(&format!("SELECT * FROM lt WHERE id = {id}")).await;
        let copy = fx
            .row(&format!("SELECT * FROM lt WHERE id = {}", 10 + id))
            .await;
        assert_eq!(&copy[1..], &source[1..], "row {id}");
        // Typed columns keep their storage class; the untyped `raw` keeps only its text.
        assert_eq!(
            fx.text(&format!("SELECT {TYPES} FROM lt WHERE id = {}", 10 + id)),
            fx.text(&format!("SELECT {TYPES} FROM lt WHERE id = {id}")),
            "row {id}"
        );
    }
    let nulls = names[1..]
        .iter()
        .map(|n| (n.clone(), EditValue::Null))
        .collect();
    fx.apply_changes(
        &target,
        vec![Change::Update {
            key: row_key(&[("id", val("11"))]),
            set: nulls,
        }],
    )
    .await
    .unwrap()
    .commit()
    .await
    .unwrap();
    assert!(
        fx.row("SELECT * FROM lt WHERE id = 11").await[1..]
            .iter()
            .all(|c| *c == Cell::Null)
    );
    // A key of each declared type matches the text the result delivered.
    for table in ["kr", "ki", "kb", "kd", "kv"] {
        let target = fx.target(table).await;
        let changes = fx
            .rows(&format!("SELECT k FROM {table}"))
            .await
            .iter()
            .map(|r| Change::Update {
                key: RowKey::new(vec![("k".to_owned(), edit_value(&r[0]))]),
                set: pairs(&[("v", val("1"))]),
            })
            .collect();
        let applied = fx.apply_changes(&target, changes).await.unwrap();
        assert_eq!(applied.affected(), 2, "{table}");
        applied.commit().await.unwrap();
    }
}

#[tokio::test]
async fn a_read_only_sqlite_connection_is_refused() {
    let fx = sqlite_fixture(SETUP, "read_only = true\n");
    let target = fx.target("t").await;
    match fx
        .apply_changes(&target, vec![update("1", &[("n", val("9"))])])
        .await
    {
        Err(ApplyError::ReadOnly) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(fx.text(SNAP), ORIGINAL);
}

const LIT: &str = "CREATE TABLE lit(id INTEGER PRIMARY KEY, n INT, r REAL, d DECIMAL(10,2), s TEXT, u); \
INSERT INTO lit VALUES (1, 1, 0.5, 1.25, '7', 'x'), (2, 2, 1.5, 2.5, '8', 'y');";
const LIT_SNAP: &str = "SELECT group_concat(quote(id) || ',' || quote(n) || ',' || quote(r) || ',' \
|| quote(d) || ',' || quote(s) || ',' || quote(u), ' ' ORDER BY id) FROM lit";

#[tokio::test]
async fn typed_literals_run_directly_store_what_apply_stores_on_sqlite() {
    let applied_fx = sqlite_fixture(LIT, "");
    let target = applied_fx.target("lit").await;
    let changes = vec![
        Change::Update {
            key: row_key(&[("id", val("1"))]),
            set: pairs(&[
                ("n", val("-3")),
                ("r", val("2.5")),
                ("d", val("10.75")),
                ("s", val("4")),
                ("u", val("5")),
            ]),
        },
        Change::Insert {
            values: pairs(&[("id", val("3")), ("n", val("007"))]),
        },
        Change::Delete {
            key: row_key(&[("id", val("2"))]),
        },
    ];
    let batch = EditBatch::new(target, changes).unwrap();
    let script = render_sql(&batch);
    assert!(
        script.contains("SET n = -3, r = 2.5, d = 10.75, s = '4', u = '5' WHERE id = 1;"),
        "{script}"
    );
    let applied = apply(
        &applied_fx.ws,
        applied_fx.ws.declared("lite").unwrap(),
        None,
        batch,
    )
    .await
    .unwrap();
    applied.commit().await.unwrap();
    let scripted_fx = sqlite_fixture(LIT, "");
    rusqlite::Connection::open(scripted_fx.db_path())
        .unwrap()
        .execute_batch(&script)
        .unwrap();
    assert_eq!(scripted_fx.text(LIT_SNAP), applied_fx.text(LIT_SNAP));
    assert_eq!(
        applied_fx.text(LIT_SNAP),
        "1,-3,2.5,10.75,'4','5' 3,7,NULL,NULL,NULL,NULL"
    );
}

#[tokio::test]
async fn an_update_of_the_key_reports_the_key_as_stored() {
    let fx = setup();
    let target = fx.target("t").await;
    let changes = vec![
        update("1", &[("id", val("010"))]),
        update("2", &[("n", val("7"))]),
    ];
    let applied = fx.apply_changes(&target, changes).await.unwrap();
    assert_eq!(applied.written_key(0), Some(&row_key(&[("id", val("10"))])));
    assert_eq!(applied.written_key(1), None, "the key was not written");
    applied.rollback().await.unwrap();
    assert_eq!(fx.text(SNAP), ORIGINAL);
}
