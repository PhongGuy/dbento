use serde_json::{Value, json};

use super::*;
use crate::dynamo::replay::{
    Replay, Reply, SECRET, TARGET, aws_error, described, orders, page, tables, test_settings,
};
use crate::session::{
    ConnectError, Driver, ExecuteError, Flavor, Limits, OnTruncate, ReadOnlyEnforcement,
    ServerInfo, StreamItem, Writes,
};

async fn session(replay: &Replay, read_only: bool, consistent: bool) -> DynamoSession {
    DynamoSession::open(
        test_settings(read_only, None),
        TARGET.to_owned(),
        consistent,
        replay.http(),
    )
    .await
    .unwrap()
}

fn ping() -> Reply {
    Reply::ok(tables(&[], None))
}

/// Runs every statement of `text` to its end; the first error stops the script.
async fn run(s: &mut DynamoSession, text: &str) -> Result<Vec<Vec<String>>, ExecuteError> {
    let mut script = s.start_script(text);
    let mut names = Vec::new();
    while let Some(prepared) = DynamoSession::prepare_next(&mut script).await {
        prepared?;
        let (columns, mut stream) = DynamoSession::execute(&mut script, Limits::default()).await?;
        names.push(columns.iter().map(|c| c.name().to_owned()).collect());
        while let StreamItem::Batch(_) = DynamoSession::next_event(&mut stream).await? {}
    }
    Ok(names)
}

fn statements(replay: &Replay) -> Vec<Value> {
    replay
        .seen()
        .into_iter()
        .filter(|s| s.target == "ExecuteStatement")
        .map(|s| s.body)
        .collect()
}

#[tokio::test]
async fn open_pings_with_one_table_and_reports_the_api_version() {
    let replay = Replay::new(vec![ping()]);
    let s = session(&replay, false, false).await;
    assert_eq!(
        (replay.seen()[0].target.as_str(), &replay.seen()[0].body),
        ("ListTables", &json!({ "Limit": 1 }))
    );
    assert_eq!(
        s.server(),
        ServerInfo {
            flavor: Flavor::DynamoDb,
            version: (2012, 8, 10)
        }
    );
    assert_eq!(
        s.read_only_level(true),
        Some(ReadOnlyEnforcement::BestEffort)
    );
    assert_eq!(s.read_only_level(false), None);
}

#[tokio::test]
async fn a_bad_key_fails_open_without_the_secret_and_a_denied_ping_connects() {
    let bad = Replay::new(vec![Reply::error(
        400,
        aws_error("UnrecognizedClientException", "invalid token"),
    )]);
    match DynamoSession::open(
        test_settings(false, None),
        TARGET.to_owned(),
        false,
        bad.http(),
    )
    .await
    {
        Err(ConnectError::Open { message }) => assert!(
            message.contains("UnrecognizedClientException") && !message.contains(SECRET),
            "{message}"
        ),
        Err(other) => panic!("{other}"),
        Ok(_) => panic!("connected with a bad key"),
    }
    let denied = Replay::new(vec![Reply::error(
        400,
        aws_error("AccessDeniedException", "no ListTables"),
    )]);
    session(&denied, false, false).await;
}

#[tokio::test]
async fn statements_are_sent_as_their_ranges_never_the_script() {
    let replay = Replay::new(vec![
        ping(),
        Reply::ok(page(&[], None, None)),
        Reply::ok(page(&[], None, None)),
    ]);
    let mut s = session(&replay, false, false).await;
    run(&mut s, "-- lead\nSELECT pk FROM \"t\";\n/* /* */ SELECT pk FROM \"t\" */ DELETE FROM \"t\" WHERE pk = 'a' ;").await.unwrap();
    let bodies = statements(&replay);
    let sent: Vec<&str> = bodies
        .iter()
        .map(|b| b["Statement"].as_str().unwrap())
        .collect();
    assert_eq!(
        sent,
        [
            "SELECT pk FROM \"t\"",
            "SELECT pk FROM \"t\" */ DELETE FROM \"t\" WHERE pk = 'a'"
        ]
    );
}

#[tokio::test]
async fn a_read_only_session_refuses_writes_and_unknown_before_sending() {
    let replay = Replay::new(vec![ping()]);
    let mut s = session(&replay, true, false).await;
    for (text, writes) in [
        ("INSERT INTO \"t\" VALUE {'pk': 'a'}", Writes::Yes),
        ("UPDATE \"t\" SET v = 1 WHERE pk = 'a'", Writes::Yes),
        ("DELETE FROM \"t\" WHERE pk = 'a'", Writes::Yes),
        (
            "EXISTS(SELECT * FROM \"t\" WHERE pk = 'a')",
            Writes::Unknown,
        ),
    ] {
        let mut script = s.start_script(text);
        let prepared = DynamoSession::prepare_next(&mut script)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(prepared.classification.writes, writes, "{text}");
        match DynamoSession::execute(&mut script, Limits::default()).await {
            Err(ExecuteError::ReadOnlyViolation { .. }) => {}
            Err(other) => panic!("{text}: {other}"),
            Ok(_) => panic!("{text} ran"),
        }
    }
    assert_eq!(replay.seen().len(), 1, "only the ping was sent");
}

#[tokio::test]
async fn consistent_read_goes_with_table_selects_only() {
    let empty = || Reply::ok(page(&[], None, None));
    let t = json!({ "TableName": "t", "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
                    "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }] });
    // The insert has no projection, so its key lookup comes before it.
    let replay = Replay::new(vec![
        ping(),
        empty(),
        empty(),
        Reply::ok(described(t)),
        empty(),
    ]);
    let mut s = session(&replay, false, true).await;
    run(
        &mut s,
        "SELECT pk FROM \"t\"; SELECT pk FROM \"t\".\"i\"; INSERT INTO \"t\" VALUE {'pk': 'a'}",
    )
    .await
    .unwrap();
    let consistent: Vec<Option<bool>> = statements(&replay)
        .iter()
        .map(|b| b.get("ConsistentRead").and_then(Value::as_bool))
        .collect();
    assert_eq!(consistent, [Some(true), None, None]);
}

#[tokio::test]
async fn an_index_target_takes_its_tables_key_columns_from_one_lookup() {
    let item = json!({ "sk": { "N": "1" }, "pk": { "S": "a" }, "x": { "S": "v" } });
    let replay = Replay::new(vec![
        ping(),
        Reply::ok(described(orders())),
        Reply::ok(page(std::slice::from_ref(&item), None, None)),
        Reply::ok(page(&[item], None, None)),
    ]);
    let mut s = session(&replay, false, false).await;
    let names = run(
        &mut s,
        "SELECT * FROM \"orders\".\"by_status\"; SELECT * FROM \"orders\"",
    )
    .await
    .unwrap();
    assert_eq!(names, [vec!["pk", "sk", "x"], vec!["pk", "sk", "x"]]);
    let lookups: Vec<Value> = replay
        .seen()
        .into_iter()
        .filter(|s| s.target == "DescribeTable")
        .map(|s| s.body)
        .collect();
    assert_eq!(lookups, [json!({ "TableName": "orders" })]);
}

#[tokio::test]
async fn a_failed_key_lookup_still_runs_the_statement() {
    let item = json!({ "pk": { "S": "a" }, "b": { "S": "v" } });
    let replay = Replay::new(vec![
        ping(),
        Reply::error(400, aws_error("AccessDeniedException", "no DescribeTable")),
        Reply::ok(page(&[item], None, None)),
    ]);
    let mut s = session(&replay, false, false).await;
    assert_eq!(
        run(&mut s, "SELECT * FROM \"t\"").await.unwrap(),
        [vec!["b", "pk"]]
    );
}

#[tokio::test]
async fn a_cancel_before_execute_sends_nothing_and_settle_spends_it() {
    let replay = Replay::new(vec![ping(), Reply::ok(page(&[], None, None))]);
    let mut s = session(&replay, false, false).await;
    s.cancel_handle().cancel();
    assert!(matches!(
        run(&mut s, "SELECT pk FROM \"t\"").await,
        Err(ExecuteError::Cancelled)
    ));
    assert!(statements(&replay).is_empty());
    s.settle().await.unwrap();
    run(&mut s, "SELECT pk FROM \"t\"").await.unwrap();
}

#[tokio::test]
async fn a_close_cut_ends_the_session() {
    let two = json!([{ "pk": { "S": "a" } }, { "pk": { "S": "b" } }]);
    let replay = Replay::new(vec![
        ping(),
        Reply::ok(page(two.as_array().unwrap(), None, None)),
    ]);
    let mut s = session(&replay, false, false).await;
    let mut script = s.start_script("SELECT pk FROM \"t\"; SELECT pk FROM \"t\"");
    DynamoSession::prepare_next(&mut script)
        .await
        .unwrap()
        .unwrap();
    let close = Limits {
        max_rows: Some(1),
        max_bytes: None,
        on_truncate: OnTruncate::Close,
    };
    let (_, mut stream) = DynamoSession::execute(&mut script, close).await.unwrap();
    while let StreamItem::Batch(_) = DynamoSession::next_event(&mut stream).await.unwrap() {}
    assert!(matches!(
        DynamoSession::prepare_next(&mut script).await,
        Some(Err(ExecuteError::SessionClosed))
    ));
    assert!(matches!(s.settle().await, Err(ExecuteError::SessionClosed)));
}
