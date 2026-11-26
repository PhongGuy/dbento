//! A connection whose password is not saved says how to save it.
use crate::common::TestServer;
use serde_json::json;

const PG: &str = "dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"127.0.0.1\"\nport = 9\ndatabase = \"x\"\nuser = \"x\"\n";

#[tokio::test]
async fn a_connect_without_a_saved_password_carries_the_cli_hint() {
    let server = TestServer::start_with(PG, |o| o.env_lookup(|_| None)).await;
    let e = server
        .err("list_schemas", json!({ "connection": "pg" }))
        .await;
    assert_eq!(e["code"], "connect", "{e}");
    assert_eq!(
        e["hint"],
        "no secret was found for 'pg': run `dbento secret set pg` or set DBENTO_SECRET_PG",
        "{e}"
    );
}

#[tokio::test]
async fn a_connection_that_needs_no_password_gets_no_hint() {
    let toml = format!("{PG}secret = \"none\"\n");
    let server = TestServer::start_with(&toml, |o| o.env_lookup(|_| None)).await;
    let e = server
        .err("list_schemas", json!({ "connection": "pg" }))
        .await;
    assert_eq!(e["code"], "connect", "{e}");
    assert!(e.get("hint").is_none(), "{e}");
}
