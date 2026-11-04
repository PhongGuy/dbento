//! A cancel reaches only the statement it was aimed at, and a cancel during a drain never
//! turns a later timeout into a cancel.
mod support;
use std::time::Duration;

use dbento_core::{ExecuteError, Limits, SessionOptions, Workspace};
use support::{PgFixture, collect, pg_fixture, run};
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};

/// About 30 s of 10 kB rows, one every 100 ms.
const SLOW: &str = "SELECT repeat('x', 10000), pg_sleep(0.1) FROM generate_series(1, 300)";

/// How long the proxy holds a cancel request before it reaches the server.
const CANCEL_DELAY: Duration = Duration::from_millis(150);

/// A TCP proxy to `host:port` on a free local port. The first connection, the session's,
/// passes at once; every later one is a cancel request and reaches the server only after
/// `CANCEL_DELAY`, so a cancel that is not waited for lands on whatever runs next.
async fn delaying_proxy(host: String, port: u16) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut first = true;
        while let Ok((mut client, _)) = listener.accept().await {
            let delay = if std::mem::take(&mut first) {
                Duration::ZERO
            } else {
                CANCEL_DELAY
            };
            let host = host.clone();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                if let Ok(mut server) = TcpStream::connect((host.as_str(), port)).await {
                    let _ = copy_bidirectional(&mut client, &mut server).await;
                }
            });
        }
    });
    local
}

/// A Postgres fixture whose connection `pg` goes through `delaying_proxy`.
async fn proxied_fixture() -> PgFixture {
    let mut fx = pg_fixture("").await;
    let (host, port) = (fx.db.host().to_owned(), fx.db.port());
    let local = delaying_proxy(host.clone(), port).await;
    let direct = format!("host = \"{host}\"\nport = {port}\n");
    let toml = fx.db.connection_toml("pg", "");
    assert!(toml.contains(&direct), "{toml}");
    let toml = toml.replace(&direct, &format!("host = \"127.0.0.1\"\nport = {local}\n"));
    std::fs::write(
        fx.dir.path().join("dbento.toml"),
        format!("dbento = 1\n{toml}"),
    )
    .unwrap();
    fx.ws = Workspace::open(fx.dir.path()).unwrap();
    fx
}

#[tokio::test]
async fn a_late_cancel_never_hits_the_next_statement() {
    let fx = proxied_fixture().await;
    let mut s = fx.session(SessionOptions::default()).await;
    let cancel = s.cancel_handle();
    for round in 0..20 {
        let first = run(&mut s, "SELECT 1", Limits::default()).await;
        assert!(
            first[0].is_ok(),
            "round {round}: {:?}",
            first[0].as_ref().err()
        );
        cancel.cancel(); // aimed at a statement that has already finished
        let next = run(&mut s, "SELECT pg_sleep(0.5)", Limits::default()).await;
        assert!(
            next[0].is_ok(),
            "round {round}: {:?}",
            next[0].as_ref().err()
        );
    }
    s.close().await.unwrap();
}

#[tokio::test]
async fn a_cancel_during_a_drain_leaves_a_later_timeout_a_timeout() {
    let fx = proxied_fixture().await;
    let mut opts = SessionOptions::default();
    opts.statement_timeout = Some(Duration::from_secs(2));
    let mut s = fx.session(opts).await;
    let cancel = s.cancel_handle();
    let mut limits = Limits::default();
    limits.max_rows = Some(1);
    {
        let mut script = s.script(&format!("{SLOW}; SELECT pg_sleep(5)"));
        let first = collect(script.next().await.unwrap().unwrap(), limits)
            .await
            .unwrap();
        assert!(first.outcome.truncated);
        // The next prepare drains SLOW's rest; this cancel stops that drain.
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            cancel.cancel();
        });
        let second = collect(script.next().await.unwrap().unwrap(), Limits::default()).await;
        assert!(
            matches!(second, Err(ExecuteError::Timeout)),
            "{:?}",
            second.err()
        );
    }
    // SLOW may have written through a function, so close reports that its drain was cancelled.
    let closed = s.close().await;
    assert!(matches!(closed, Err(ExecuteError::Cancelled)), "{closed:?}");
}
