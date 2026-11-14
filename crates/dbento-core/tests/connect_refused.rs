use std::fs;

use dbento_core::{ConnectError, SessionOptions, Workspace, connect};

// A port that was just free: bound once and released, so nothing listens there.
fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

async fn refused(engine: &str) -> ConnectError {
    let port = closed_port();
    let dir = tempfile::tempdir().unwrap();
    let toml = format!(
        "dbento = 1\n[connections.down]\nengine = \"{engine}\"\nhost = \"127.0.0.1\"\nport = {port}\ndatabase = \"d\"\nuser = \"u\"\nsecret = \"none\"\n"
    );
    fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    let conn = ws.connection("down").unwrap();
    let err = connect(&ws, &conn, None, SessionOptions::default())
        .await
        .err()
        .expect("nothing listens on the port");
    let ConnectError::Refused { address, .. } = &err else {
        panic!("not Refused: {err:?}");
    };
    assert_eq!(
        address,
        &format!("127.0.0.1:{port}"),
        "the configured address"
    );
    err
}

#[tokio::test]
async fn postgres_names_a_refused_connection_and_its_address() {
    let err = refused("postgres").await;
    assert!(
        err.to_string()
            .starts_with("cannot open the database: u@127.0.0.1:"),
        "{err}"
    );
}

#[tokio::test]
async fn mysql_names_a_refused_connection_and_its_address() {
    let err = refused("mysql").await;
    assert!(
        err.to_string()
            .starts_with("cannot open the database: u@127.0.0.1:"),
        "{err}"
    );
}
