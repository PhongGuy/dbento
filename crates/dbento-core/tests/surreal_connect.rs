//! Connecting: the socket, the version floor, signin, and `ws` against `wss`.

use crate::support::{sr_root, sr_try};
use dbento_core::{ConnectError, Engine, Flavor, Session, SessionOptions};
use dbento_testkit::{surreal_db, surreal_server, surreal_tls};

fn open_message(e: Result<Session, ConnectError>) -> String {
    match e {
        Err(ConnectError::Open { message }) => message,
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("connected"),
    }
}

#[tokio::test]
async fn a_session_reports_its_engine_and_server() {
    let db = surreal_db().await;
    let s = sr_root(&db, "", SessionOptions::default()).await;
    assert_eq!(s.engine(), Engine::Surrealdb);
    let server = s.server().unwrap();
    assert_eq!(server.flavor, Flavor::SurrealDb);
    assert_eq!(server.version, (3, 3, 5));
    assert_eq!(s.server_warning(), None);
    s.close().await.unwrap();
}

#[tokio::test]
async fn a_bad_password_names_what_to_check() {
    let db = surreal_db().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!(
            "dbento = 1\n{}",
            db.connection_toml("sr", Some(("dv", "database")), "")
        ),
    )
    .unwrap();
    let ws = dbento_core::Workspace::open(dir.path()).unwrap();
    let conn = ws.declared("sr").unwrap();
    let secret = Some(dbento_core::Secret::new("wrong"));
    let message =
        open_message(dbento_core::connect(&ws, conn, secret, SessionOptions::default()).await);
    for word in ["auth_level", "database", db.namespace(), db.database()] {
        assert!(message.contains(word), "{message}");
    }
    assert!(!message.contains("wrong"), "{message}");
}

#[tokio::test]
async fn a_server_below_the_floor_is_refused_by_version() {
    let old = surreal_server("v3.2.6", &[]).await;
    let message =
        open_message(sr_try(&old, Some(("root", "root")), "", SessionOptions::default()).await);
    assert!(message.contains("3.2.6"), "{message}");
    assert!(message.contains("3.3.0"), "{message}");
}

#[tokio::test]
async fn a_beta_of_a_higher_major_connects_with_a_warning() {
    let beta = surreal_server("v4.0.0-beta.1", &[]).await;
    let s = sr_root(&beta, "", SessionOptions::default()).await;
    assert_eq!(s.server().unwrap().version, (4, 0, 0));
    let warning = s.server_warning().unwrap();
    assert!(warning.contains("4.0.0-beta.1"), "{warning}");
}

fn verify(ca: &str) -> String {
    format!("tls_ca = \"{ca}\"\n")
}

fn wss_toml(toml: String) -> String {
    toml.replace("tls = \"off\"\n", "")
}

async fn wss_try(db: &dbento_testkit::SrDb, extra: &str) -> Result<Session, ConnectError> {
    let dir = tempfile::tempdir().unwrap();
    let toml = wss_toml(db.connection_toml("sr", Some(("root", "root")), extra));
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!("dbento = 1\n{toml}"),
    )
    .unwrap();
    let ws = dbento_core::Workspace::open(dir.path()).unwrap();
    let conn = ws.declared("sr").unwrap();
    let secret = Some(dbento_core::Secret::new(db.password()));
    dbento_core::connect(&ws, conn, secret, SessionOptions::default()).await
}

#[tokio::test]
async fn wss_works_with_the_servers_ca_and_each_wrong_pairing_names_tls() {
    let (db, ca) = surreal_tls().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ca.pem");
    std::fs::write(&path, ca).unwrap();
    let s = wss_try(&db, &verify(path.to_str().unwrap())).await.unwrap();
    assert_eq!(s.server().unwrap().version, (3, 3, 5));
    drop(s);

    // An untrusted certificate.
    match wss_try(&db, "").await {
        Err(ConnectError::Tls { message, .. }) => {
            assert!(message.contains("tls_ca"), "{message}");
            assert!(message.contains("tls = \"off\""), "{message}");
        }
        other => panic!("{:?}", other.err()),
    }
    // Plain ws to the TLS port: a TLS alert where an HTTP reply should be.
    let message =
        open_message(sr_try(&db, Some(("root", "root")), "", SessionOptions::default()).await);
    assert!(message.contains("tls = \"verify\""), "{message}");
    // wss to a plain port: the connection is reset.
    let plain = surreal_db().await;
    match wss_try(&plain, "").await {
        Err(ConnectError::Tls { message, .. }) => {
            assert!(message.contains("tls = \"off\""), "{message}");
        }
        other => panic!("{:?}", other.err()),
    }
}
