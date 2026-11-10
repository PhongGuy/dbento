//! Postgres over TLS: a verified connection with an extra CA, cancel over the same connector,
//! and the failures that must never fall back to plaintext.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::support::{pg_fixture, run, text};
use dbento_core::{ConnectError, ExecuteError, Limits, Secret, SessionOptions, Workspace, connect};

async fn tls_workspace(
    ca_file: Option<&str>,
) -> (dbento_testkit::PgDb, tempfile::TempDir, Workspace) {
    let (db, ca_pem) = dbento_testkit::pg_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let mut extra = String::from("tls = \"verify\"\n");
    if let Some(name) = ca_file {
        std::fs::write(dir.path().join(name), &ca_pem).unwrap();
        extra.push_str(&format!("tls_ca = \"{name}\"\n"));
    }
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!("dbento = 1\n{}", db.connection_toml("pg", &extra)),
    )
    .unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    (db, dir, ws)
}

#[tokio::test]
async fn verify_with_tls_ca_connects_and_cancels_over_tls() {
    let (db, _dir, ws) = tls_workspace(Some("ca.pem")).await;
    let mut s = connect(
        &ws,
        ws.connection("pg").unwrap(),
        Some(Secret::new(db.password())),
        SessionOptions::default(),
    )
    .await
    .unwrap();
    let r = run(
        &mut s,
        "SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()",
        Limits::default(),
    )
    .await;
    assert_eq!(r[0].as_ref().unwrap().rows, vec![vec![text("t")]]);
    let cancel = s.cancel_handle();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        cancel.cancel();
    });
    // Bounded: a cancel that never arrives must fail the test, not hang it.
    let r = tokio::time::timeout(
        Duration::from_secs(20),
        run(&mut s, "SELECT pg_sleep(30)", Limits::default()),
    )
    .await
    .expect("cancel over TLS did not stop pg_sleep");
    t.join().unwrap();
    assert!(
        matches!(r[0], Err(ExecuteError::Cancelled)),
        "{:?}",
        r[0].as_ref().err()
    );
}

/// The first eight bytes of an SSLRequest: length 8, then code 80877103.
const SSL_REQUEST: [u8; 8] = [0, 0, 0, 8, 4, 210, 22, 47];

/// A loopback proxy to `upstream` that records the first eight bytes of every connection, so a
/// test can see how each one opened: an SSLRequest, or a plaintext CancelRequest.
async fn recording_proxy(upstream: u16) -> (u16, Arc<Mutex<Vec<[u8; 8]>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            let record = Arc::clone(&record);
            tokio::spawn(async move {
                let mut head = [0u8; 8];
                if client.read_exact(&mut head).await.is_err() {
                    return;
                }
                record.lock().unwrap().push(head);
                let Ok(mut server) = tokio::net::TcpStream::connect(("127.0.0.1", upstream)).await
                else {
                    return;
                };
                if server.write_all(&head).await.is_ok() {
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
                }
            });
        }
    });
    (port, seen)
}

#[tokio::test]
async fn a_cancel_on_a_tls_session_opens_with_an_ssl_request() {
    let (db, ca_pem) = dbento_testkit::pg_tls().await;
    let (port, seen) = recording_proxy(db.port()).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ca.pem"), &ca_pem).unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!(
            "dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"{}\"\nport = {port}\n\
             database = \"{}\"\nuser = \"{}\"\nsecret = \"none\"\ntls = \"verify\"\n\
             tls_ca = \"ca.pem\"\n",
            db.host(),
            db.database(),
            db.user()
        ),
    )
    .unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    let mut s = connect(
        &ws,
        ws.connection("pg").unwrap(),
        Some(Secret::new(db.password())),
        SessionOptions::default(),
    )
    .await
    .unwrap();
    let cancel = s.cancel_handle();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        cancel.cancel();
    });
    let r = tokio::time::timeout(
        Duration::from_secs(20),
        run(&mut s, "SELECT pg_sleep(30)", Limits::default()),
    )
    .await
    .expect("cancel through the proxy did not stop pg_sleep");
    t.join().unwrap();
    assert!(
        matches!(r[0], Err(ExecuteError::Cancelled)),
        "{:?}",
        r[0].as_ref().err()
    );
    // The session's own connection plus the cancel request's, both opened with TLS.
    let seen = seen.lock().unwrap().clone();
    assert!(seen.len() >= 2, "connections seen: {seen:?}");
    assert!(seen.iter().all(|h| *h == SSL_REQUEST), "openings: {seen:?}");
}

#[tokio::test]
async fn an_unknown_issuer_is_a_tls_error_naming_the_host() {
    let (db, _dir, ws) = tls_workspace(None).await;
    let err = connect(
        &ws,
        ws.connection("pg").unwrap(),
        Some(Secret::new(db.password())),
        SessionOptions::default(),
    )
    .await
    .err()
    .expect("unverifiable");
    assert!(
        matches!(&err, ConnectError::Tls { host, .. } if host == "localhost"),
        "{err:?}"
    );
    assert!(err.to_string().contains("UnknownIssuer"), "{err}");
}

#[tokio::test]
async fn verify_against_a_server_without_tls_fails() {
    let fx = pg_fixture("tls = \"verify\"").await;
    let err = connect(
        &fx.ws,
        fx.conn(),
        Some(fx.secret()),
        SessionOptions::default(),
    )
    .await
    .err()
    .expect("no plaintext fallback");
    assert!(matches!(err, ConnectError::Tls { .. }), "{err:?}");
}

#[tokio::test]
async fn an_unreadable_tls_ca_is_a_tls_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        "dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"db.invalid\"\ndatabase = \"d\"\nuser = \"u\"\nsecret = \"none\"\ntls_ca = \"nope.pem\"\n",
    )
    .unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    let err = connect(
        &ws,
        ws.connection("pg").unwrap(),
        None,
        SessionOptions::default(),
    )
    .await
    .err()
    .expect("no CA file");
    assert!(
        matches!(&err, ConnectError::Tls { .. }) && err.to_string().contains("nope.pem"),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_tls_ca_file_without_certificates_is_a_tls_error() {
    // No server is needed: the connector is built before any socket opens.
    for (name, body) in [("empty.pem", ""), ("garbage.pem", "not a certificate\n")] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(name), body).unwrap();
        std::fs::write(
            dir.path().join("dbento.toml"),
            format!(
                "dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"127.0.0.1\"\n\
                 port = 1\ndatabase = \"d\"\nuser = \"u\"\ntls = \"verify\"\ntls_ca = \"{name}\"\n"
            ),
        )
        .unwrap();
        let ws = Workspace::open(dir.path()).unwrap();
        let err = tokio::time::timeout(
            Duration::from_secs(20),
            connect(
                &ws,
                ws.connection("pg").unwrap(),
                Some(Secret::new("p")),
                SessionOptions::default(),
            ),
        )
        .await
        .expect("bounded")
        .err()
        .expect("no certificates");
        assert!(
            matches!(&err, ConnectError::Tls { message, .. } if message.contains("no certificates in") && message.contains(name)),
            "{name}: {err:?}"
        );
    }
}
