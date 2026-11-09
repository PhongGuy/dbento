//! TLS against the stock and the test-CA certificate, and login errors.

use crate::support::{ms_fixture, ms_off, run, text};
use dbento_core::{ConnectError, Limits, Secret, SessionOptions, connect};

#[tokio::test]
async fn verify_refuses_the_stock_v1_certificate_with_the_tls_hint() {
    dbento_testkit::skip_without_mssql!();
    // The one allowed skip: a DBENTO_MSSQL_URL server is not ours, so its certificate may not be
    // the stock one. The harness container always presents the stock certificate.
    if std::env::var("DBENTO_MSSQL_URL").is_ok_and(|u| !u.is_empty()) {
        eprintln!(
            "skipped: DBENTO_MSSQL_URL names a server whose certificate the harness did not choose"
        );
        return;
    }
    let fx = ms_fixture(dbento_testkit::mssql_db().await, "");
    match fx.try_session(SessionOptions::default()).await {
        Err(ConnectError::Tls { message, .. }) => {
            assert!(
                message.contains("X.509 v1") && message.contains("tls_ca"),
                "{message}"
            );
            assert!(
                message.contains("tls = \"off\"") && !message.contains("trust_cert"),
                "{message}"
            );
        }
        other => panic!("{:?}", other.err()),
    }
}

#[tokio::test]
async fn verify_with_tls_ca_connects_encrypted_and_without_it_names_tls_ca() {
    dbento_testkit::skip_without_mssql!();
    let (db, ca) = dbento_testkit::mssql_tls().await;
    let fx = ms_fixture(db, "tls_ca = \"ca.pem\"");
    std::fs::write(fx.dir.path().join("ca.pem"), &ca).unwrap();
    let mut s = fx.session(SessionOptions::default()).await;
    let sql = "SELECT encrypt_option FROM sys.dm_exec_connections WHERE session_id = @@SPID";
    assert_eq!(
        run(&mut s, sql, Limits::default())
            .await
            .remove(0)
            .unwrap()
            .rows,
        vec![vec![text("TRUE")]]
    );
    s.close().await.unwrap();
    let bare = ms_fixture(fx.db, "");
    match bare.try_session(SessionOptions::default()).await {
        Err(ConnectError::Tls { message, .. }) => assert!(message.contains("tls_ca"), "{message}"),
        other => panic!("{:?}", other.err()),
    }
}

#[tokio::test]
async fn a_wrong_password_names_the_target_and_the_error_number() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let wrong = connect(
        &fx.ws,
        fx.conn(),
        Some(Secret::new("not-the-password")),
        SessionOptions::default(),
    );
    match wrong.await {
        Err(ConnectError::Open { message }) => assert!(
            message.contains("18456") && message.contains(&fx.conn().display_target()),
            "{message}"
        ),
        other => panic!("{:?}", other.err()),
    }
    let missing = connect(&fx.ws, fx.conn(), None, SessionOptions::default());
    match missing.await {
        Err(ConnectError::Open { message }) => assert!(message.contains("18456"), "{message}"),
        other => panic!("{:?}", other.err()),
    }
}

#[tokio::test]
async fn a_server_that_never_answers_times_out_naming_the_target() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    // Accept and hold every connection without a byte of TDS.
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((sock, _)) = listener.accept().await {
            held.push(sock);
        }
    });
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!("dbento = 1\n[connections.dead]\nengine = \"mssql\"\nhost = \"127.0.0.1\"\nport = {port}\nuser = \"sa\"\ntls = \"off\"\n"),
    )
    .unwrap();
    let ws = dbento_core::Workspace::open(dir.path()).unwrap();
    let conn = ws.connection("dead").unwrap();
    let started = std::time::Instant::now();
    match connect(&ws, conn, Some(Secret::new("x")), SessionOptions::default()).await {
        Err(ConnectError::Open { message }) => assert_eq!(
            message,
            format!(
                "{}: the server did not answer within 10 s",
                conn.display_target()
            )
        ),
        other => panic!("{:?}", other.err()),
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(15));
}
