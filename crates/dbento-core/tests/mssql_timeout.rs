//! The statement timeout on SQL Server: a client watchdog sends Attention at the deadline.
mod support;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dbento_core::{ExecuteError, Limits, SessionOptions, Workspace};
use support::{MsFixture, collect, ms_fixture, run, text};
use tokio::sync::Notify;

const CUBE: &str = "SELECT a.object_id FROM sys.all_objects a CROSS JOIN sys.all_objects b \
                    CROSS JOIN sys.all_objects c";

async fn timed(extra: &str) -> MsFixture {
    ms_fixture(
        dbento_testkit::mssql_db().await,
        &format!("tls = \"off\"\n{extra}"),
    )
}

#[tokio::test]
async fn a_batch_past_the_timeout_stops_and_the_session_goes_on() {
    dbento_testkit::skip_without_mssql!();
    let fx = timed("statement_timeout = \"1s\"").await;
    let mut s = fx.session(SessionOptions::default()).await;
    for sql in [
        "WAITFOR DELAY '00:00:05'",
        "SELECT 1 AS early; WAITFOR DELAY '00:00:05'",
        CUBE,
    ] {
        let started = Instant::now();
        let r = run(&mut s, sql, Limits::default()).await;
        let last = r.last().and_then(|x| x.as_ref().err());
        assert!(
            matches!(last, Some(ExecuteError::Timeout)),
            "{sql}: {last:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "{sql}: {:?}",
            started.elapsed()
        );
        let after = run(&mut s, "SELECT 7 AS seven", Limits::default())
            .await
            .remove(0)
            .unwrap();
        assert_eq!(after.rows, vec![vec![text("7")]], "{sql}");
    }
}

#[tokio::test]
async fn each_batch_gets_its_own_deadline() {
    dbento_testkit::skip_without_mssql!();
    let fx = timed("statement_timeout = \"1s\"").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let script =
        "WAITFOR DELAY '00:00:00.700'\nGO\nWAITFOR DELAY '00:00:00.700'\nGO\nSELECT 1 AS one";
    let r = run(&mut s, script, Limits::default()).await;
    assert_eq!(r.len(), 3);
    assert!(r.iter().all(Result::is_ok));
}

#[tokio::test]
async fn a_session_timeout_overrides_the_connections() {
    dbento_testkit::skip_without_mssql!();
    let fx = timed("").await;
    let mut opts = SessionOptions::default();
    opts.statement_timeout = Some(Duration::from_millis(500));
    let mut s = fx.session(opts).await;
    let r = run(&mut s, "WAITFOR DELAY '00:00:05'", Limits::default()).await;
    assert!(matches!(r.last(), Some(Err(ExecuteError::Timeout))));
}

#[tokio::test]
async fn a_batch_read_to_its_end_is_neither_timed_out_nor_cancelled_while_the_caller_waits() {
    dbento_testkit::skip_without_mssql!();
    let fx = timed("statement_timeout = \"1s\"").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let cancel = s.cancel_handle();
    let mut script = s.script("SELECT 1 AS one");
    let p = script.next().await.unwrap().unwrap();
    collect(p, Limits::default()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    cancel.cancel();
    assert!(script.next().await.is_none());
}

#[tokio::test]
async fn a_send_read_to_its_final_done_is_not_timed_out_before_the_next_send() {
    dbento_testkit::skip_without_mssql!();
    let fx = timed("statement_timeout = \"1s\"").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let mut script = s.script("SELECT 1 AS one\nGO\nSELECT 2 AS two");
    let p = script.next().await.unwrap().unwrap();
    collect(p, Limits::default()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let p = script.next().await.unwrap().unwrap();
    let two = collect(p, Limits::default()).await.unwrap();
    assert_eq!(two.rows, vec![vec![text("2")]]);
}

const FLUSHED_THEN_WAITS: &str = "INSERT dbo.t VALUES (1) RAISERROR('f', 0, 1) WITH NOWAIT \
                                  WAITFOR DELAY '00:00:05' INSERT dbo.t VALUES (2)";

/// Reads the batch's first item, keeps its script alive for `held`, then drops it and closes.
async fn first_item_then_close(fx: &MsFixture, held: Duration) -> Result<(), ExecuteError> {
    fx.db.scalar("CREATE TABLE dbo.t (v int)").await;
    let mut s = fx.session(SessionOptions::default()).await;
    {
        let mut script = s.script(FLUSHED_THEN_WAITS);
        let first = script.next().await.unwrap().unwrap();
        collect(first, Limits::default()).await.unwrap();
        tokio::time::sleep(held).await;
    }
    s.close().await
}

#[tokio::test]
async fn a_timeout_after_the_script_is_dropped_is_reported_on_closing() {
    dbento_testkit::skip_without_mssql!();
    let fx = timed("statement_timeout = \"1s\"").await;
    let closed = first_item_then_close(&fx, Duration::ZERO).await;
    assert!(matches!(closed, Err(ExecuteError::Timeout)), "{closed:?}");
    let second = fx.db.scalar("SELECT COUNT(*) FROM dbo.t WHERE v = 2").await;
    assert_eq!(second.as_deref(), Some("0"));
}

#[tokio::test]
async fn a_timeout_before_the_script_is_dropped_is_reported_on_closing() {
    dbento_testkit::skip_without_mssql!();
    let fx = timed("statement_timeout = \"1s\"").await;
    let closed = first_item_then_close(&fx, Duration::from_millis(1500)).await;
    assert!(matches!(closed, Err(ExecuteError::Timeout)), "{closed:?}");
    let second = fx.db.scalar("SELECT COUNT(*) FROM dbo.t WHERE v = 2").await;
    assert_eq!(second.as_deref(), Some("0"));
}

/// Routes connection `ms` through a local proxy. At `pause` it stops carrying bytes, closing
/// both sockets, or holding them open when `stall`, so no server error ever reaches the client.
async fn proxied(fx: &mut MsFixture, stall: bool) -> Arc<Notify> {
    let (host, port) = (fx.db.host().to_owned(), fx.db.port());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = listener.local_addr().unwrap().port();
    let pause = Arc::new(Notify::new());
    let cut = pause.clone();
    tokio::spawn(async move {
        let (mut client, _) = listener.accept().await.unwrap();
        let mut server = tokio::net::TcpStream::connect((host.as_str(), port))
            .await
            .unwrap();
        tokio::select! {
            _ = tokio::io::copy_bidirectional(&mut client, &mut server) => {}
            () = cut.notified() => {
                if stall {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
            }
        }
    });
    let path = fx.dir.path().join("dbento.toml");
    let direct = format!("host = \"{}\"\nport = {}\n", fx.db.host(), fx.db.port());
    let through = format!("host = \"127.0.0.1\"\nport = {local}\n");
    let toml = std::fs::read_to_string(&path).unwrap();
    assert!(toml.contains(&direct), "{toml}");
    std::fs::write(&path, toml.replace(&direct, &through)).unwrap();
    fx.ws = Workspace::open(fx.dir.path()).unwrap();
    pause
}

/// Reads the first item of a long batch, drops its script, pauses the proxy and closes.
async fn dropped_then_paused(fx: &MsFixture, pause: &Notify) -> Result<(), ExecuteError> {
    let mut s = fx.session(SessionOptions::default()).await;
    {
        let sql = "SELECT 1 AS one RAISERROR('f', 0, 1) WITH NOWAIT WAITFOR DELAY '00:00:08'";
        let mut script = s.script(sql);
        let first = script.next().await.unwrap().unwrap();
        collect(first, Limits::default()).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    pause.notify_one();
    s.close().await
}

// Nobody reads a dropped script's response, so a connection lost under it is only known here.
#[tokio::test]
async fn a_client_lost_after_the_script_is_dropped_is_reported_on_closing() {
    dbento_testkit::skip_without_mssql!();
    let mut fx = timed("").await;
    let cut = proxied(&mut fx, false).await;
    let closed = dropped_then_paused(&fx, &cut).await;
    assert!(
        matches!(closed, Err(ExecuteError::SessionClosed)),
        "{closed:?}"
    );
}

#[tokio::test]
async fn a_timeout_whose_acknowledgement_never_comes_is_reported_on_closing() {
    dbento_testkit::skip_without_mssql!();
    let mut fx = timed("statement_timeout = \"1s\"").await;
    let stall = proxied(&mut fx, true).await;
    let closed = dropped_then_paused(&fx, &stall).await;
    assert!(matches!(closed, Err(ExecuteError::Timeout)), "{closed:?}");
}
