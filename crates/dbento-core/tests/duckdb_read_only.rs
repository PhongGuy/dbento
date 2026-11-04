#![cfg(feature = "duckdb")]
mod support;
use dbento_core::{ConnectError, ExecuteError, Limits, SessionOptions, Writes};
use support::{duckdb_fixture, run, text};

const SETUP: &str = "CREATE TABLE t(a INTEGER); INSERT INTO t VALUES (1)";

fn forced(trusted: bool) -> SessionOptions {
    let mut o = SessionOptions::default();
    o.force_read_only = true;
    o.trusted_sql = trusted;
    o
}

async fn refused(s: &mut dbento_core::Session, sql: &str) -> String {
    match run(s, sql, Limits::default()).await.pop().unwrap() {
        Err(ExecuteError::ReadOnlyViolation { message }) => message,
        other => panic!("{sql}: {:?}", other.err()),
    }
}

#[tokio::test]
async fn a_read_only_connection_is_a_locked_instance() {
    let fx = duckdb_fixture(SETUP, "read_only = true\n");
    let d = fx.dir.path().display().to_string();
    let mut s = fx.session(SessionOptions::default()).await;
    assert!(s.is_read_only());
    for sql in [
        "INSERT INTO t VALUES (2)".to_owned(),
        "CREATE TABLE u(a INTEGER)".to_owned(),
        format!("COPY t TO '{d}/out.csv'"),
        format!("EXPORT DATABASE '{d}/export'"),
        format!("ATTACH '{d}/new.duckdb' AS n (READ_WRITE)"),
        "SET GLOBAL memory_limit = '1GB'".to_owned(),
        format!("INSTALL '{d}/none.duckdb_extension'"),
        format!("LOAD '{d}/none.duckdb_extension'"),
    ] {
        refused(&mut s, &sql).await;
    }
    for made in ["out.csv", "export", "new.duckdb"] {
        assert!(!fx.dir.path().join(made).exists(), "{made}");
    }
    let r = run(&mut s, "BEGIN; SELECT a FROM t; COMMIT", Limits::default()).await;
    assert_eq!(r[1].as_ref().unwrap().rows, [vec![text("1")]]);
    assert!(r.iter().all(Result::is_ok));
}

#[tokio::test]
async fn an_untrusted_forced_read_only_session_is_locked_too() {
    let fx = duckdb_fixture(SETUP, "");
    let d = fx.dir.path().display().to_string();
    let mut s = fx.session(forced(false)).await;
    assert!(
        refused(&mut s, &format!("COPY t TO '{d}/out.csv'"))
            .await
            .contains("disabled by configuration")
    );
    assert!(!fx.dir.path().join("out.csv").exists());
}

#[tokio::test]
async fn a_trusted_read_only_session_writes_nothing_beside_a_working_writer() {
    let fx = duckdb_fixture(SETUP, "");
    let mut w = fx.session(SessionOptions::default()).await;
    let mut r = fx.session(forced(true)).await;
    assert!(r.is_read_only());
    assert!(
        refused(&mut r, "INSERT INTO t VALUES (5)")
            .await
            .contains("read-only mode")
    );
    refused(&mut r, "CREATE TABLE u(a INTEGER)").await;
    run(&mut w, "INSERT INTO t VALUES (2)", Limits::default())
        .await
        .remove(0)
        .unwrap();
    let seen = run(&mut r, "SELECT count(*) FROM t", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(seen.rows, [vec![text("2")]]);
    match run(&mut r, "BEGIN", Limits::default()).await.remove(0) {
        Err(ExecuteError::TransactionControl { .. }) => {}
        other => panic!("{:?}", other.err()),
    }
    assert_eq!(
        run(&mut r, "SELECT 1", Limits::default())
            .await
            .remove(0)
            .unwrap()
            .rows,
        [vec![text("1")]]
    );
    w.close().await.unwrap();
    r.close().await.unwrap();
}

#[tokio::test]
async fn an_untrusted_read_only_open_beside_a_writer_is_refused() {
    let fx = duckdb_fixture(SETUP, "");
    let w = fx.session(SessionOptions::default()).await;
    match fx.try_session_of("duck", forced(false)).await {
        Err(ConnectError::Open { message }) => {
            assert!(
                message.contains("already open read-write in this process"),
                "{message}"
            );
        }
        other => panic!("{:?}", other.err()),
    }
    w.close().await.unwrap();
}

#[tokio::test]
async fn two_connections_on_one_file_with_different_read_only() {
    let fx = duckdb_fixture(
        SETUP,
        "[connections.ro]\nengine = \"duckdb\"\npath = \"data/app.duckdb\"\nread_only = true\n",
    );
    let w = fx.session(SessionOptions::default()).await;
    assert!(matches!(
        fx.try_session_of("ro", SessionOptions::default()).await,
        Err(ConnectError::Open { .. })
    ));
    w.close().await.unwrap();
    let r = fx
        .try_session_of("ro", SessionOptions::default())
        .await
        .unwrap();
    assert!(matches!(
        fx.try_session_of("duck", SessionOptions::default()).await,
        Err(ConnectError::Open { .. })
    ));
    r.close().await.unwrap();
}

#[tokio::test]
async fn mcp_classifies_read_only_then_writes_once_it_closed() {
    let fx = duckdb_fixture(SETUP, "");
    let mut probe = fx.session(forced(false)).await;
    {
        let mut script = probe.script("INSERT INTO t VALUES (3)");
        let p = script.next().await.unwrap().unwrap();
        assert_eq!(p.classification().writes, Writes::Yes);
    }
    probe.close().await.unwrap();
    let mut w = fx.session(SessionOptions::default()).await;
    let r = run(&mut w, "INSERT INTO t VALUES (3)", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(r.outcome.rows_affected, Some(1));
    w.close().await.unwrap();
}

#[tokio::test]
async fn other_engines_ignore_trusted_sql() {
    let fx = support::sqlite_fixture("CREATE TABLE t(a INTEGER)", "");
    let mut s = fx.session(forced(true)).await;
    refused(&mut s, "INSERT INTO t VALUES (1)").await;
}
