mod support;
use dbento_core::{Limits, SessionOptions, Unguarded, Writes};
use support::{run, sqlite_fixture};

#[tokio::test]
async fn statements_are_classified_before_they_run() {
    use Unguarded::{DeleteWithoutWhere as D, UpdateWithoutWhere as U};
    let cases: &[(&str, Writes, &[Unguarded])] = &[
        ("SELECT 1", Writes::No, &[]),
        ("PRAGMA table_info(t)", Writes::No, &[]),
        ("PRAGMA query_only = 0", Writes::No, &[]),
        ("BEGIN", Writes::No, &[]),
        ("INSERT INTO t(a) VALUES (1)", Writes::Yes, &[]),
        (
            "INSERT INTO t(a) VALUES (1) ON CONFLICT(a) DO UPDATE SET a = excluded.a",
            Writes::Yes,
            &[],
        ),
        ("PRAGMA journal_mode = WAL", Writes::Yes, &[]),
        ("DELETE FROM t", Writes::Yes, &[D]),
        ("DELETE FROM t WHERE a = 1", Writes::Yes, &[]),
        ("WITH x AS (SELECT 1) DELETE FROM t", Writes::Yes, &[D]),
        ("UPDATE t SET a = 1", Writes::Yes, &[U]),
        ("UPDATE t SET a = s.b FROM s", Writes::Yes, &[U]),
        (
            "UPDATE t SET a = s.b FROM s WHERE t.a = s.b",
            Writes::Yes,
            &[],
        ),
        ("UPDATE OR IGNORE t SET a = 1", Writes::Yes, &[U]),
        ("EXPLAIN DELETE FROM t", Writes::Yes, &[D]),
        (
            "CREATE TRIGGER tr AFTER INSERT ON t BEGIN DELETE FROM u; END",
            Writes::Yes,
            &[],
        ),
    ];
    let fx = sqlite_fixture(
        "CREATE TABLE t(a INTEGER UNIQUE); CREATE TABLE s(b INTEGER); CREATE TABLE u(c INTEGER)",
        "",
    );
    let mut s = fx.session(SessionOptions::default()).await;
    for (sql, writes, unguarded) in cases {
        let mut script = s.script(sql);
        let pending = script.next().await.unwrap().unwrap();
        let c = pending.classification();
        assert_eq!(&c.writes, writes, "{sql}");
        assert_eq!(c.unguarded.as_slice(), *unguarded, "{sql}");
        pending.skip();
    }
}

#[tokio::test]
async fn classification_never_blocks_execution() {
    let fx = sqlite_fixture(
        "CREATE TABLE t(a INTEGER); INSERT INTO t VALUES (1),(2)",
        "",
    );
    let mut s = fx.session(SessionOptions::default()).await;
    let r = run(&mut s, "DELETE FROM t", Limits::default()).await;
    let Ok(ran) = &r[0] else {
        panic!("an unguarded DELETE still runs when executed")
    };
    assert_eq!(ran.outcome.rows_affected, Some(2));
    assert_eq!(fx.count("SELECT count(*) FROM t"), 0);
}

#[tokio::test]
async fn transaction_control_is_flagged_on_the_pending_statement() {
    let fx = sqlite_fixture("CREATE TABLE t(a INTEGER)", "");
    let mut read_only = SessionOptions::default();
    read_only.force_read_only = true;
    for opts in [SessionOptions::default(), read_only] {
        let mut s = fx.session(opts).await;
        for (sql, tc) in [
            ("BEGIN", true),
            ("SAVEPOINT a", true),
            ("COMMIT", true),
            ("INSERT INTO t VALUES (1)", false),
            ("SELECT 1", false),
        ] {
            let mut script = s.script(sql);
            let pending = script.next().await.unwrap().unwrap();
            assert_eq!(pending.classification().transaction_control, tc, "{sql}");
            pending.skip();
        }
    }
}

#[tokio::test]
async fn every_sqlite_drop_form_is_classified_on_the_pending_statement() {
    let fx = sqlite_fixture(
        "CREATE TABLE t(a); CREATE INDEX i ON t(a); CREATE VIEW v AS SELECT a FROM t; \
         CREATE TRIGGER tr AFTER INSERT ON t BEGIN SELECT 1; END;",
        "",
    );
    let mut s = fx.session(SessionOptions::default()).await;
    for sql in [
        "DROP TABLE t",
        "DROP TABLE IF EXISTS main.t",
        "drop index i",
        "DROP INDEX IF EXISTS i",
        "DROP VIEW v",
        "DROP TRIGGER tr",
        "-- c\nDROP TRIGGER IF EXISTS tr",
    ] {
        let mut script = s.script(sql);
        let pending = script.next().await.unwrap().unwrap();
        let c = pending.classification();
        assert_eq!(c.writes, Writes::Yes, "{sql}");
        assert_eq!(c.unguarded.as_slice(), [Unguarded::Drop], "{sql}");
        pending.skip();
    }
}

#[test]
fn every_unguarded_kind_has_one_label_for_every_shell() {
    assert_eq!(
        Unguarded::UpdateWithoutWhere.label(),
        "UPDATE without WHERE"
    );
    assert_eq!(
        Unguarded::DeleteWithoutWhere.label(),
        "DELETE without WHERE"
    );
    assert_eq!(Unguarded::Drop.label(), "DROP");
    assert_eq!(Unguarded::Truncate.label(), "TRUNCATE");
}
