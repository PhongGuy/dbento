//! Connecting: the handshake and the privilege check.
#![cfg(feature = "mongodb")]

use std::time::{Duration, Instant};

use crate::support::{forced, mg};
use dbento_core::{
    ConnectError, Flavor, ReadOnlyEnforcement, Secret, SessionOptions, Workspace, connect,
};
use dbento_testkit::MgUser::{Anonymous, Reader, Root, Writer};

#[tokio::test]
async fn an_unauthenticated_session_has_no_privileges_so_it_is_best_effort() {
    let fx = mg("").await;
    let s = fx.session(Anonymous, forced()).await;
    assert_eq!(
        s.read_only_enforcement(),
        Some(ReadOnlyEnforcement::BestEffort)
    );
}

#[tokio::test]
async fn the_level_follows_the_users_privileges() {
    let fx = mg("").await;
    for (user, want) in [
        (Reader, ReadOnlyEnforcement::Database),
        (Writer, ReadOnlyEnforcement::BestEffort),
        (Root, ReadOnlyEnforcement::BestEffort),
    ] {
        let s = fx.session(user, forced()).await;
        assert_eq!(s.read_only_enforcement(), Some(want), "{user:?}");
        assert!(s.is_read_only());
        s.close().await.unwrap();
    }
}

#[tokio::test]
async fn the_read_only_key_checks_privileges_too_and_a_writable_session_skips_them() {
    let fx = mg("read_only = true").await;
    let s = fx.session(Reader, SessionOptions::default()).await;
    assert_eq!(
        s.read_only_enforcement(),
        Some(ReadOnlyEnforcement::Database)
    );
    let plain = mg("").await;
    let s = plain.session(Writer, SessionOptions::default()).await;
    assert_eq!(s.read_only_enforcement(), None);
    assert!(!s.is_read_only());
    let server = s.server().unwrap();
    assert_eq!((server.flavor, server.version.0), (Flavor::MongoDb, 8));
}

#[tokio::test]
async fn a_wrong_password_names_auth_source() {
    let fx = mg("").await;
    let wrong = Some(Secret::new("wrong"));
    match connect(&fx.ws, fx.conn(Reader), wrong, SessionOptions::default()).await {
        Err(ConnectError::Open { message }) => assert!(message.contains("authSource"), "{message}"),
        other => panic!("{:?}", other.err()),
    }
}

#[tokio::test]
async fn an_unreachable_server_is_named() {
    let dir = tempfile::tempdir().unwrap();
    // The URI's own timeout stands (Decision 26), so this does not wait `CONNECT_TIMEOUT`.
    let uri = "mongodb://127.0.0.1:1/x?serverSelectionTimeoutMS=200";
    let toml = format!("dbento = 1\n[connections.c]\nengine = \"mongodb\"\nuri = \"{uri}\"\n");
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    let conn = ws.connection("c").unwrap();
    let at = Instant::now();
    match connect(&ws, conn, None, SessionOptions::default()).await {
        Err(ConnectError::Open { message }) => {
            assert!(message.contains("127.0.0.1:1"), "{message}")
        }
        other => panic!("{:?}", other.err()),
    }
    assert!(at.elapsed() < Duration::from_secs(5), "{:?}", at.elapsed());
}

#[tokio::test]
async fn a_missing_ca_file_is_a_tls_error_named_from_the_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let uri = "mongodb://127.0.0.1:1/x?tls=true&tlsCAFile=certs/none.pem";
    let toml = format!("dbento = 1\n[connections.c]\nengine = \"mongodb\"\nuri = \"{uri}\"\n");
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    let conn = ws.connection("c").unwrap();
    match connect(&ws, conn, None, SessionOptions::default()).await {
        Err(ConnectError::Tls { message, .. }) => {
            let want = dir.path().join("certs/none.pem");
            assert!(message.contains(want.to_str().unwrap()), "{message}");
        }
        other => panic!("{:?}", other.err()),
    }
}
