use std::time::Duration as Wait;

use serde_json::json;

use super::*;
use crate::Secret;
use crate::dynamo::ops::DynamoOp;
use crate::dynamo::replay::{Replay, Reply, aws_error, page, scanned, tables};
use crate::dynamo::statement::Kind;
use crate::session::ConnectError;

fn req(kind: Kind, text: &str) -> StatementRequest<'_> {
    StatementRequest {
        kind,
        text,
        next_token: None,
        consistent_read: false,
    }
}

#[tokio::test]
async fn a_statement_is_sent_as_written_with_total_capacity_and_no_limit() {
    let replay = Replay::new(vec![Reply::ok(page(&[], None, None))]);
    let text = "SELECT * FROM \"t\" WHERE pk = 'a'";
    replay
        .client(false)
        .execute_statement(req(Kind::Select, text))
        .await
        .unwrap();
    let seen = replay.seen();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].target, "ExecuteStatement");
    assert_eq!(
        seen[0].body,
        json!({ "Statement": text, "ReturnConsumedCapacity": "TOTAL" })
    );
}

#[tokio::test]
async fn a_next_token_and_consistent_read_ride_along_when_asked() {
    let replay = Replay::new(vec![Reply::ok(page(&[], None, None))]);
    let request = StatementRequest {
        kind: Kind::Select,
        text: "SELECT * FROM \"t\"",
        next_token: Some("tok-1"),
        consistent_read: true,
    };
    replay
        .client(false)
        .execute_statement(request)
        .await
        .unwrap();
    assert_eq!(
        replay.seen()[0].body,
        json!({ "Statement": "SELECT * FROM \"t\"", "ReturnConsumedCapacity": "TOTAL",
                "NextToken": "tok-1", "ConsistentRead": true })
    );
}

#[tokio::test(start_paused = true)]
async fn a_read_is_retried_but_a_write_or_unknown_statement_gets_one_attempt() {
    let failing = || -> Vec<Reply> {
        (0..4)
            .map(|_| Reply::error(500, aws_error("InternalServerError", "boom")))
            .collect()
    };
    let read = Replay::new(failing());
    assert!(
        read.client(false)
            .execute_statement(req(Kind::Select, "SELECT 1"))
            .await
            .is_err()
    );
    assert_eq!(read.seen().len(), 3, "the standard mode's three attempts");
    for kind in [Kind::Insert, Kind::Update, Kind::Delete, Kind::Other] {
        let once = Replay::new(failing());
        assert!(
            once.client(false)
                .execute_statement(req(kind, "x"))
                .await
                .is_err()
        );
        assert_eq!(once.seen().len(), 1, "{kind:?}");
    }
}

#[tokio::test]
async fn a_read_only_client_refuses_writes_without_sending() {
    let replay = Replay::new(vec![]);
    let client = replay.client(true);
    for kind in [Kind::Insert, Kind::Update, Kind::Delete, Kind::Other] {
        match client.execute_statement(req(kind, "x")).await {
            Err(CallError::Refused(op)) => assert_eq!(op, DynamoOp::ExecuteStatement(kind)),
            Err(CallError::Sdk(_)) => panic!("{kind:?} reached the SDK"),
            Ok(_) => panic!("{kind:?} was sent"),
        }
    }
    assert!(replay.seen().is_empty());
}

#[tokio::test]
async fn a_read_only_client_still_sends_catalog_calls_scans_and_selects() {
    let replay = Replay::new(vec![
        Reply::ok(tables(&[], None)),
        Reply::ok(r#"{"Table":{"TableName":"t"}}"#),
        Reply::ok(scanned(&[])),
        Reply::ok(page(&[], None, None)),
    ]);
    let client = replay.client(true);
    client.list_tables(None, Some(1)).await.unwrap();
    client.describe_table("t").await.unwrap();
    client.scan("t", 100).await.unwrap();
    client
        .execute_statement(req(Kind::Select, "SELECT * FROM \"t\""))
        .await
        .unwrap();
    let seen = replay.seen();
    let targets: Vec<&str> = seen.iter().map(|s| s.target.as_str()).collect();
    assert_eq!(
        targets,
        ["ListTables", "DescribeTable", "Scan", "ExecuteStatement"]
    );
    assert_eq!(seen[0].body, json!({ "Limit": 1 }));
    assert_eq!(seen[1].body, json!({ "TableName": "t" }));
    assert_eq!(seen[2].body, json!({ "TableName": "t", "Limit": 100 }));
}

#[test]
fn an_endpoint_is_handed_over_as_its_normalised_origin() {
    for (written, origin) in [
        ("http://127.0.0.1:5000", "http://127.0.0.1:5000"),
        ("http://LOCALHOST:5000/", "http://localhost:5000"),
        (" http://localhost:8000", "http://localhost:8000"),
        ("http://%6cocalhost:8000", "http://localhost:8000"),
        ("http:localhost:8000", "http://localhost:8000"),
        (
            "https://dynamodb.eu-west-1.amazonaws.com:443/",
            "https://dynamodb.eu-west-1.amazonaws.com",
        ),
    ] {
        assert_eq!(
            endpoint_origin(written).as_deref(),
            Some(origin),
            "{written}"
        );
    }
    assert_eq!(endpoint_origin("not a url"), None);
    assert_eq!(
        endpoint_origin("file:///x"),
        None,
        "an opaque origin has no endpoint"
    );
}

#[test]
fn settings_resolve_the_region_credentials_and_endpoint_origin() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dbento.toml"), "dbento = 1\n\
        [connections.a]\nengine = \"dynamodb\"\nregion = \"eu-west-1\"\nendpoint = \"http://LOCALHOST:5000/\"\n\
        access_key_id = \"AKIDA\"\nsecret = \"keychain\"\n\
        [connections.b]\nengine = \"dynamodb\"\n").unwrap();
    let ws = crate::workspace::Workspace::open(dir.path()).unwrap();
    let no_env = |_: &str| None;
    let a = ClientSettings::from_connection(
        ws.declared("a").unwrap(),
        Some(&Secret::new("s")),
        Some(Wait::from_secs(5)),
        true,
        no_env,
    )
    .unwrap();
    assert_eq!(
        (a.region.as_str(), a.endpoint.as_deref()),
        ("eu-west-1", Some("http://localhost:5000"))
    );
    assert_eq!(
        (
            a.credentials.access_key_id.as_str(),
            a.operation_timeout,
            a.read_only
        ),
        ("AKIDA", Some(Wait::from_secs(5)), true)
    );
    assert_eq!(a.connect_timeout, CONNECT_TIMEOUT);
    match ClientSettings::from_connection(ws.declared("b").unwrap(), None, None, false, no_env) {
        Err(ConnectError::Open { message }) => assert!(message.contains("AWS_REGION"), "{message}"),
        other => panic!("{:?}", other.err()),
    }
    let env = |k: &str| match k {
        "AWS_REGION" => Some("us-east-1".to_owned()),
        "AWS_ACCESS_KEY_ID" => Some("AKIDENV".to_owned()),
        "AWS_SECRET_ACCESS_KEY" => Some("envsecret".to_owned()),
        _ => None,
    };
    let b =
        ClientSettings::from_connection(ws.declared("b").unwrap(), None, None, false, env).unwrap();
    assert_eq!(
        (
            b.region.as_str(),
            b.credentials.access_key_id.as_str(),
            b.endpoint
        ),
        ("us-east-1", "AKIDENV", None)
    );
}
