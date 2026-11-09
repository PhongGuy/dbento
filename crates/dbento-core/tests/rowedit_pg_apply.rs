use crate::support::{PgFixture, edit_value, pairs, pg_fixture, rejected, row_key, run, val};
use dbento_core::{ApplyError, Change, ChangeOutcome, EditValue};
use dbento_core::{Limits, RelationRef, SessionOptions};

const SETUP: &str = r#"
CREATE DOMAIN d3 AS varchar(3) CHECK (VALUE <> 'no');
CREATE DOMAIN dd3 AS d3;
CREATE TABLE sized(id int PRIMARY KEY, v varchar(3), c char(3), b bit(3), va varchar(3)[],
  ca char(3)[], ba bit(3)[], d d3, n dd3, da d3[]);
INSERT INTO sized(id) VALUES (1);
CREATE TABLE t(id int PRIMARY KEY, v varchar(3), n int);
INSERT INTO t VALUES (1, 'a', 10), (2, 'b', 20), (3, 'c', 30);
"#;
const ORIGINAL: &str = "(1,a,10) (2,b,20) (3,c,30)";

async fn setup(extra_toml: &str) -> PgFixture {
    let fx = pg_fixture(extra_toml).await;
    fx.db.admin().await.batch_execute(SETUP).await.unwrap();
    fx
}

fn update(id: &str, set: &[(&str, EditValue)]) -> Change {
    Change::Update {
        key: row_key(&[("id", val(id))]),
        set: pairs(set),
    }
}

async fn rows(fx: &PgFixture) -> String {
    fx.db
        .scalar("SELECT string_agg(t::text, ' ' ORDER BY id) FROM t")
        .await
        .unwrap()
}

#[tokio::test]
async fn sized_types_and_domains_refuse_bad_values_and_store_good_ones() {
    let fx = setup("").await;
    let target = fx.target("public", "sized").await;
    let varchar = "value too long for type character varying(3)";
    let bpchar = "value too long for type character(3)";
    let bad = [
        ("v", "abcdef", varchar),
        ("c", "abcdef", bpchar),
        ("b", "10", "bit string length 2 does not match type bit(3)"),
        (
            "b",
            "1010",
            "bit string length 4 does not match type bit(3)",
        ),
        ("va", "{abcdef}", varchar),
        ("ca", "{abcdef}", bpchar),
        (
            "ba",
            "{10}",
            "bit string length 2 does not match type bit(3)",
        ),
        ("d", "abcdef", varchar),
        (
            "d",
            "no",
            "value for domain d3 violates check constraint \"d3_check\"",
        ),
        ("n", "abcdef", varchar),
        (
            "n",
            "no",
            "value for domain dd3 violates check constraint \"d3_check\"",
        ),
        ("da", "{abcdef}", varchar),
        (
            "da",
            "{no}",
            "value for domain d3 violates check constraint \"d3_check\"",
        ),
    ];
    for (column, value, message) in bad {
        let outcomes = rejected(
            fx.apply_changes(&target, vec![update("1", &[(column, val(value))])])
                .await,
        );
        match outcomes.as_slice() {
            [ChangeOutcome::Failed(m)] => assert!(m.contains(message), "{column} = {value}: {m}"),
            other => panic!("{column} = {value}: {other:?}"),
        }
    }
    assert_eq!(
        fx.db
            .scalar(
                "SELECT count(*) FROM sized WHERE num_nonnulls(v, c, b, va, ca, ba, d, n, da) > 0"
            )
            .await
            .as_deref(),
        Some("0")
    );
    let good = [
        ("v", "ab"),
        ("c", "ab"),
        ("b", "101"),
        ("va", "{ab}"),
        ("ca", "{ab}"),
        ("ba", "{101}"),
        ("d", "ab"),
        ("n", "ab"),
        ("da", "{ab}"),
    ];
    let applied = fx
        .apply_changes(&target, vec![update("1", &good.map(|(c, v)| (c, val(v))))])
        .await
        .unwrap();
    assert_eq!(applied.affected(), 1);
    applied.commit().await.unwrap();
    assert_eq!(
        fx.db
            .scalar("SELECT concat_ws('|', v, c, b, va, ca, ba, d, n, da) FROM sized WHERE id = 1")
            .await
            .as_deref(),
        Some("ab|ab |101|{ab}|{\"ab \"}|{101}|ab|ab|{ab}")
    );
}

#[tokio::test]
async fn updates_commit_and_count_one_row_per_change() {
    let fx = setup("").await;
    let target = fx.target("public", "t").await;
    let applied = fx
        .apply_changes(
            &target,
            vec![
                update("1", &[("v", val("x")), ("n", EditValue::Null)]),
                update("2", &[("n", val("21"))]),
            ],
        )
        .await
        .unwrap();
    assert_eq!(applied.affected(), 2);
    applied.commit().await.unwrap();
    assert_eq!(rows(&fx).await, "(1,x,) (2,b,21) (3,c,30)");
}

#[tokio::test]
async fn rollback_and_drop_leave_the_table_unchanged() {
    let fx = setup("").await;
    let target = fx.target("public", "t").await;
    let applied = fx
        .apply_changes(&target, vec![update("1", &[("v", val("x"))])])
        .await
        .unwrap();
    applied.rollback().await.unwrap();
    assert_eq!(rows(&fx).await, ORIGINAL);
    let applied = fx
        .apply_changes(&target, vec![update("1", &[("v", val("y"))])])
        .await
        .unwrap();
    drop(applied);
    // The dropped session's row lock leaves with its connection, so this apply proceeds.
    let applied = fx
        .apply_changes(&target, vec![update("1", &[("n", val("11"))])])
        .await
        .unwrap();
    applied.commit().await.unwrap();
    assert_eq!(rows(&fx).await, "(1,a,11) (2,b,20) (3,c,30)");
}

#[tokio::test]
async fn a_row_deleted_before_apply_is_a_conflict_and_nothing_commits() {
    let fx = setup("").await;
    let target = fx.target("public", "t").await;
    fx.db
        .admin()
        .await
        .batch_execute("DELETE FROM t WHERE id = 1")
        .await
        .unwrap();
    let outcomes = rejected(
        fx.apply_changes(
            &target,
            vec![
                update("1", &[("v", val("x"))]),
                update("2", &[("v", val("y"))]),
            ],
        )
        .await,
    );
    assert_eq!(
        outcomes,
        vec![ChangeOutcome::Conflict, ChangeOutcome::Applied(1)]
    );
    assert_eq!(rows(&fx).await, "(2,b,20) (3,c,30)");
}

#[tokio::test]
async fn a_failed_change_stops_the_rest_and_nothing_commits() {
    let fx = setup("").await;
    let target = fx.target("public", "t").await;
    let outcomes = rejected(
        fx.apply_changes(
            &target,
            vec![
                update("1", &[("v", val("x"))]),
                update("2", &[("n", val("abc"))]),
                update("3", &[("v", val("z"))]),
            ],
        )
        .await,
    );
    assert_eq!(outcomes.len(), 3);
    assert_eq!(outcomes[0], ChangeOutcome::Applied(1));
    match &outcomes[1] {
        ChangeOutcome::Failed(m) => {
            assert!(m.contains("invalid input syntax for type integer"), "{m}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(outcomes[2], ChangeOutcome::NotAttempted);
    assert_eq!(rows(&fx).await, ORIGINAL);
}

#[tokio::test]
async fn inserts_and_deletes_commit() {
    let fx = setup("").await;
    let target = fx.target("public", "t").await;
    let applied = fx
        .apply_changes(
            &target,
            vec![
                Change::Insert {
                    values: pairs(&[("id", val("4")), ("v", val("d")), ("n", EditValue::Null)]),
                },
                Change::Delete {
                    key: row_key(&[("id", val("2"))]),
                },
            ],
        )
        .await
        .unwrap();
    assert_eq!(applied.affected(), 2);
    applied.commit().await.unwrap();
    assert_eq!(rows(&fx).await, "(1,a,10) (3,c,30) (4,d,)");
}

#[tokio::test]
async fn a_read_only_connection_is_refused() {
    let fx = setup("read_only = true").await;
    let target = fx.target("public", "t").await;
    match fx
        .apply_changes(&target, vec![update("1", &[("v", val("x"))])])
        .await
    {
        Err(ApplyError::ReadOnly) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(rows(&fx).await, ORIGINAL);
}

#[tokio::test]
async fn an_update_of_the_key_reports_each_type_in_its_output_text() {
    let fx = setup("").await;
    fx.db
        .admin()
        .await
        .batch_execute(
            "CREATE TABLE k(c char(3), b boolean, n int, PRIMARY KEY (c, b)); \
             INSERT INTO k VALUES ('x', false, 1);",
        )
        .await
        .unwrap();
    let target = fx.target("public", "k").await;
    let change = Change::Update {
        key: row_key(&[("c", val("x")), ("b", val("false"))]),
        set: pairs(&[("c", val("a")), ("b", val("yes"))]),
    };
    let applied = fx.apply_changes(&target, vec![change]).await.unwrap();
    // A browse reads the padded char and `t`, where `::text` would give `a` and `true`.
    assert_eq!(
        applied.written_key(0),
        Some(&row_key(&[("c", val("a  ")), ("b", val("t"))]))
    );
    applied.commit().await.unwrap();
}

#[tokio::test]
async fn a_commit_the_server_refuses_is_a_failed_commit() {
    let fx = setup("").await;
    fx.db
        .admin()
        .await
        .batch_execute(
            "CREATE TABLE p(id int PRIMARY KEY); INSERT INTO p VALUES (1);
             CREATE TABLE c(id int PRIMARY KEY, p int REFERENCES p DEFERRABLE INITIALLY DEFERRED);
             INSERT INTO c VALUES (1, 1);",
        )
        .await
        .unwrap();
    let target = fx.target("public", "c").await;
    // The deferred key is checked only at COMMIT, so the change applies and the commit fails.
    let applied = fx
        .apply_changes(&target, vec![update("1", &[("p", val("99"))])])
        .await
        .unwrap();
    match applied.commit().await {
        Err(ApplyError::CommitFailed(dbento_core::ExecuteError::Database { code, .. })) => {
            assert_eq!(code, "23503")
        }
        other => panic!("{other:?}"),
    }
    let p = fx.db.scalar("SELECT p::text FROM c").await.unwrap();
    assert_eq!(p, "1");
}

#[tokio::test]
async fn a_connection_lost_before_commit_leaves_the_outcome_unknown() {
    let fx = setup("").await;
    let target = fx.target("public", "t").await;
    let applied = fx
        .apply_changes(&target, vec![update("1", &[("v", val("x"))])])
        .await
        .unwrap();
    fx.db
        .admin()
        .await
        .batch_execute(
            "SELECT pg_terminate_backend(pid, 5000) FROM pg_stat_activity \
             WHERE datname = current_database() AND state = 'idle in transaction'",
        )
        .await
        .unwrap();
    match applied.commit().await {
        Err(ApplyError::CommitUnknown(_)) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(rows(&fx).await, ORIGINAL);
}

#[tokio::test]
async fn a_key_edit_of_a_table_with_an_instead_rule_commits_without_returning() {
    let fx = setup("").await;
    fx.db
        .admin()
        .await
        .batch_execute(
            "CREATE TABLE store(id int PRIMARY KEY, v text); INSERT INTO store VALUES (1, 'a');
             CREATE TABLE ru(id int PRIMARY KEY, v text); INSERT INTO ru VALUES (1, 'a');
             CREATE RULE ru_update AS ON UPDATE TO ru DO INSTEAD
               UPDATE store SET id = NEW.id, v = NEW.v WHERE id = OLD.id;",
        )
        .await
        .unwrap();
    let target = fx.target("public", "ru").await;
    let applied = fx
        .apply_changes(&target, vec![update("1", &[("id", val("5"))])])
        .await
        .unwrap();
    // The rule hides the written row, so no key comes back; the rule's count still holds.
    assert_eq!(applied.affected(), 1);
    assert_eq!(applied.written_key(0), None);
    applied.commit().await.unwrap();
    let store = fx
        .db
        .scalar("SELECT string_agg(store::text, ' ') FROM store")
        .await;
    assert_eq!(store.unwrap(), "(5,a)");
    // A key the rule's UPDATE does not find is still a conflict.
    let outcomes = rejected(
        fx.apply_changes(&target, vec![update("7", &[("id", val("8"))])])
            .await,
    );
    assert_eq!(outcomes, vec![ChangeOutcome::Conflict]);
}

#[tokio::test]
async fn a_key_edit_of_a_deleted_row_is_a_conflict() {
    let fx = setup("").await;
    let target = fx.target("public", "t").await;
    fx.db
        .admin()
        .await
        .batch_execute("DELETE FROM t WHERE id = 1")
        .await
        .unwrap();
    let outcomes = rejected(
        fx.apply_changes(&target, vec![update("1", &[("id", val("9"))])])
            .await,
    );
    assert_eq!(outcomes, vec![ChangeOutcome::Conflict]);
    assert_eq!(rows(&fx).await, "(2,b,20) (3,c,30)");
}

#[tokio::test]
async fn a_key_edit_that_matches_two_rows_is_ambiguous() {
    let fx = setup("").await;
    // A deferred key lets the batch's own insert duplicate the row until COMMIT.
    fx.db
        .admin()
        .await
        .batch_execute(
            "CREATE TABLE dk(id int, v text, PRIMARY KEY (id) DEFERRABLE INITIALLY DEFERRED);
             INSERT INTO dk VALUES (1, 'a');",
        )
        .await
        .unwrap();
    let target = fx.target("public", "dk").await;
    let outcomes = rejected(
        fx.apply_changes(
            &target,
            vec![
                Change::Insert {
                    values: pairs(&[("id", val("1")), ("v", val("b"))]),
                },
                update("1", &[("id", val("9"))]),
            ],
        )
        .await,
    );
    assert_eq!(
        outcomes,
        vec![ChangeOutcome::Applied(1), ChangeOutcome::Ambiguous(2)]
    );
    let dk = fx
        .db
        .scalar("SELECT string_agg(dk::text, ' ') FROM dk")
        .await;
    assert_eq!(dk.unwrap(), "(1,a)");
}

#[tokio::test]
async fn a_key_edit_is_refused_by_a_read_only_connection_and_transaction() {
    let fx = setup("read_only = true").await;
    let target = fx.target("public", "t").await;
    match fx
        .apply_changes(&target, vec![update("1", &[("id", val("9"))])])
        .await
    {
        Err(ApplyError::ReadOnly) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(rows(&fx).await, ORIGINAL);
    // A connection Dbento does not mark read-only still meets the database's refusal.
    let fx = setup("").await;
    let target = fx.target("public", "t").await;
    fx.db
        .admin()
        .await
        .batch_execute(
            "DO $$ BEGIN EXECUTE format('ALTER DATABASE %I SET default_transaction_read_only = on', \
             current_database()); END $$",
        )
        .await
        .unwrap();
    let outcomes = rejected(
        fx.apply_changes(&target, vec![update("1", &[("id", val("9"))])])
            .await,
    );
    match &outcomes[..] {
        [ChangeOutcome::Failed(m)] => assert!(m.contains("read-only transaction"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(rows(&fx).await, ORIGINAL);
}

#[tokio::test]
async fn a_key_edit_reads_and_writes_key_text_as_the_browse_session_does() {
    let fx = setup(r#"settings = { DateStyle = "ISO, MDY", TimeZone = "UTC" }"#).await;
    fx.db
        .admin()
        .await
        .batch_execute(
            "CREATE TABLE ts(at timestamptz, d date, v text, PRIMARY KEY (at, d));
             INSERT INTO ts VALUES ('2026-10-23 10:00:00+00', '2026-10-23', 'a');",
        )
        .await
        .unwrap();
    // The browse session prints dates day first in Tokyo time; the apply session does not.
    let mut s = fx.session(SessionOptions::default()).await;
    let set = "SET DateStyle = 'SQL, DMY'; SET TimeZone = 'Asia/Tokyo'";
    for r in run(&mut s, set, Limits::default()).await {
        r.unwrap();
    }
    let target = s
        .edit_target(&RelationRef::new("public", "ts"))
        .await
        .unwrap();
    let mut read = run(
        &mut s,
        "SELECT at, d FROM ts; \
         SELECT '2026-11-05 08:00:00+00'::timestamptz, '2026-11-05'::date",
        Limits::default(),
    )
    .await
    .into_iter()
    .map(|r| r.unwrap().rows.remove(0));
    let (old, new) = (read.next().unwrap(), read.next().unwrap());
    s.close().await.unwrap();
    let change = Change::Update {
        key: row_key(&[("at", edit_value(&old[0])), ("d", edit_value(&old[1]))]),
        set: pairs(&[
            ("at", val("2026-11-05 08:00:00+00")),
            ("d", val("2026-11-05")),
        ]),
    };
    let applied = fx.apply_changes(&target, vec![change]).await.unwrap();
    assert_eq!(
        applied.written_key(0),
        Some(&row_key(&[
            ("at", edit_value(&new[0])),
            ("d", edit_value(&new[1]))
        ]))
    );
    applied.commit().await.unwrap();
    let moved = fx
        .db
        .scalar("SELECT (at = '2026-11-05 08:00:00+00' AND d = '2026-11-05')::text FROM ts")
        .await;
    assert_eq!(moved.unwrap(), "true");
}

#[tokio::test]
async fn a_ctid_row_edited_twice_without_a_reload_is_found_again() {
    let fx = setup("").await;
    fx.db
        .admin()
        .await
        .batch_execute("CREATE TABLE np(v text, n int); INSERT INTO np VALUES ('a', 1), ('b', 2);")
        .await
        .unwrap();
    let target = fx.target("public", "np").await;
    let cells = fx.row("SELECT ctid, xmin FROM np WHERE v = 'a'").await;
    let key = row_key(&[
        ("ctid", edit_value(&cells[0])),
        ("xmin", edit_value(&cells[1])),
    ]);
    let edit = |key: &dbento_core::RowKey, n: &str| Change::Update {
        key: key.clone(),
        set: pairs(&[("n", val(n))]),
    };
    let first = fx
        .apply_changes(&target, vec![edit(&key, "10")])
        .await
        .unwrap();
    // The grid keeps its staged key when none comes back, as it does for a stable key.
    let moved = first.written_key(0).cloned().unwrap_or(key);
    first.commit().await.unwrap();
    let second = fx.apply_changes(&target, vec![edit(&moved, "11")]).await;
    second.unwrap().commit().await.unwrap();
    let n = fx
        .db
        .scalar("SELECT string_agg(np::text, ' ' ORDER BY v) FROM np")
        .await;
    assert_eq!(n.unwrap(), "(a,11) (b,2)");
}

#[tokio::test]
async fn a_ctid_edit_of_a_table_with_an_instead_rule_commits_without_returning() {
    let fx = setup("").await;
    fx.db
        .admin()
        .await
        .batch_execute(
            "CREATE TABLE shadow(v text); INSERT INTO shadow VALUES ('a');
             CREATE TABLE nr(v text); INSERT INTO nr VALUES ('a');
             CREATE RULE nr_update AS ON UPDATE TO nr DO INSTEAD
               UPDATE shadow SET v = NEW.v WHERE v = OLD.v;",
        )
        .await
        .unwrap();
    let target = fx.target("public", "nr").await;
    let cells = fx.row("SELECT ctid, xmin FROM nr").await;
    let key = row_key(&[
        ("ctid", edit_value(&cells[0])),
        ("xmin", edit_value(&cells[1])),
    ]);
    let change = Change::Update {
        key,
        set: pairs(&[("v", val("b"))]),
    };
    // The rule's UPDATE counts as the one row, and no key comes back.
    let applied = fx.apply_changes(&target, vec![change]).await.unwrap();
    assert_eq!(applied.written_key(0), None);
    applied.commit().await.unwrap();
    assert_eq!(fx.db.scalar("SELECT v FROM shadow").await.unwrap(), "b");
}
