//! The runner's Target command.
use crate::support;
use crate::support::{fixture, here, next_event, run_cmd, spec, until_event};
use dbento_app::runner::{Command, Event, spawn_on};
use dbento_core::RelationRef;

#[tokio::test(flavor = "multi_thread")]
async fn target_connects_and_describes_a_table() {
    let fx = fixture(
        "CREATE TABLE plain(a, b); CREATE TABLE k(id INTEGER PRIMARY KEY, v TEXT);",
        "",
    );
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(Command::Target {
        rel: RelationRef::new("main", "plain"),
    });
    assert!(matches!(next_event(&ev).await, Event::Connected { .. }));
    match next_event(&ev).await {
        Event::Target {
            rel, result: Ok(t), ..
        } => {
            assert_eq!(rel, RelationRef::new("main", "plain"));
            assert_eq!(t.locators(), ["rowid"]);
            assert!(t.has_identity());
        }
        other => panic!("{other:?}"),
    }
    r.send(Command::Target {
        rel: RelationRef::new("main", "k"),
    });
    match next_event(&ev).await {
        Event::Target { result: Ok(t), .. } => assert_eq!(t.identity(), ["id"]),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_table_is_an_error_and_the_session_still_runs() {
    let fx = fixture("", "");
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(Command::Target {
        rel: RelationRef::new("main", "gone"),
    });
    assert!(matches!(next_event(&ev).await, Event::Connected { .. }));
    assert!(matches!(
        next_event(&ev).await,
        Event::Target { result: Err(_), .. }
    ));
    r.send(run_cmd(1, "SELECT 1", None));
    let (_, rows) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })).await;
    assert_eq!(rows, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_connect_still_answers_the_target() {
    let fx = fixture("", "");
    let r = spawn_on(&here(), spec(&fx, "nopg"));
    let ev = r.events();
    r.send(Command::Target {
        rel: RelationRef::new("public", "t"),
    });
    assert!(matches!(next_event(&ev).await, Event::ConnectFailed { .. }));
    assert!(matches!(
        next_event(&ev).await,
        Event::Target { result: Err(_), .. }
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn target_carries_the_tables_foreign_keys() {
    let fx = fixture(
        "CREATE TABLE a(id INTEGER PRIMARY KEY); CREATE TABLE b(id INTEGER PRIMARY KEY, a_id REFERENCES a);",
        "",
    );
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(Command::Target {
        rel: RelationRef::new("main", "b"),
    });
    assert!(matches!(next_event(&ev).await, Event::Connected { .. }));
    match next_event(&ev).await {
        Event::Target {
            result: Ok(_),
            foreign_keys,
            ..
        } => {
            assert_eq!(foreign_keys.len(), 1);
            assert_eq!(foreign_keys[0].referenced_columns(), ["id"]);
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_foreign_key_read_still_answers_the_target_without_keys() {
    // `b` names only the table it references, so reading its key looks up `v`'s primary key,
    // and `v` is a virtual table whose module this build lacks.
    let fx = fixture(
        "CREATE TABLE b(id INTEGER PRIMARY KEY, v_id REFERENCES v); \
         PRAGMA writable_schema = ON; \
         INSERT INTO sqlite_schema VALUES('table', 'v', 'v', 0, 'CREATE VIRTUAL TABLE v USING no_such_module()'); \
         PRAGMA writable_schema = OFF;",
        "",
    );
    let probe = rusqlite::Connection::open(support::db(&fx)).unwrap();
    assert!(
        probe
            .query_row("SELECT count(*) FROM pragma_table_info('v')", [], |r| r
                .get::<_, i64>(0))
            .is_err(),
        "the key read must fail for this test to mean anything"
    );
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(Command::Target {
        rel: RelationRef::new("main", "b"),
    });
    assert!(matches!(next_event(&ev).await, Event::Connected { .. }));
    match next_event(&ev).await {
        Event::Target {
            result: Ok(t),
            foreign_keys,
            ..
        } => {
            assert_eq!(t.identity(), ["id"]);
            assert!(foreign_keys.is_empty(), "{foreign_keys:?}");
        }
        other => panic!("{other:?}"),
    }
    r.send(run_cmd(1, "SELECT 1", None));
    let (_, rows) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })).await;
    assert_eq!(rows, 1, "the session still runs");
}
