//! DynamoDB sessions on Moto: reads, paging, writes, refusals and connect failures.
#![cfg(feature = "dynamodb")]
use dbento_core::{
    ConnectError, ExecuteError, Flavor, Limits, OnTruncate, ReadOnlyEnforcement, Secret,
    SessionOptions, TruncatedBy, Workspace, connect,
};
use dbento_testkit::{DY_ACCESS_KEY, DY_SECRET, DY_SECRET_VAR, ORDERS};

use crate::support::{Cell, code_of, dy, failure, forced, read, text};

#[tokio::test]
async fn a_session_names_dynamodb_and_a_forced_one_is_best_effort() {
    let fx = dy("").await;
    let s = fx.session(SessionOptions::default()).await;
    let server = s.server().unwrap();
    assert_eq!(
        (server.flavor, server.version),
        (Flavor::DynamoDb, (2012, 8, 10))
    );
    assert_eq!(s.read_only_enforcement(), None);
    let ro = fx.session(forced()).await;
    assert_eq!(
        ro.read_only_enforcement(),
        Some(ReadOnlyEnforcement::BestEffort)
    );
}

#[tokio::test]
async fn select_star_puts_the_keys_first_and_moto_reports_no_capacity() {
    let fx = dy("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let got = read(&mut s, "SELECT * FROM \"orders\" WHERE pk = 'cust1'").await;
    assert_eq!(got.rows.len(), ORDERS / 5);
    let first: Vec<(&str, &str)> = got
        .columns
        .iter()
        .take(2)
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .collect();
    assert_eq!(first, [("pk", "S"), ("sk", "N")]);
    // Moto ignores ReturnConsumedCapacity; real DynamoDB's sums are Task 3's replay tests.
    assert_eq!(got.outcome.consumed_capacity, None);
}

#[tokio::test]
async fn a_row_limit_truncates_and_park_then_extend_continues() {
    let fx = dy("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let mut script = s.script("SELECT * FROM \"orders\"");
    let pending = script.next().await.unwrap().unwrap();
    let park = {
        let mut l = Limits::default();
        l.max_rows = Some(100);
        l.on_truncate = OnTruncate::Park;
        l
    };
    let mut stream = pending.execute(park).await.unwrap();
    let mut rows = 0;
    for (more, expected, cut) in [
        (None, 100, true),
        (Some(Some(100)), 200, true),
        (Some(None), ORDERS, false),
    ] {
        if let Some(more) = more {
            stream.extend(more).await.unwrap();
        }
        while let Some(batch) = stream.next_batch().await.unwrap() {
            rows += batch.len();
        }
        let outcome = stream.outcome().unwrap();
        assert_eq!((rows, outcome.truncated), (expected, cut));
        assert_eq!(outcome.truncated_by, cut.then_some(TruncatedBy::Rows));
    }
}

#[tokio::test]
async fn writes_round_trip_on_the_string_keyed_table() {
    let fx = dy("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let inserted = read(&mut s, "INSERT INTO \"h\" VALUE {'id': 'a', 'v': '1'}").await;
    assert!(inserted.columns.is_empty() && inserted.outcome.rows_affected.is_none());
    read(&mut s, "UPDATE \"h\" SET v = '2' WHERE id = 'a'").await;
    assert_eq!(
        read(&mut s, "SELECT v FROM \"h\" WHERE id = 'a'")
            .await
            .rows,
        [vec![text("2")]]
    );
    read(&mut s, "DELETE FROM \"h\" WHERE id = 'a'").await;
    assert_eq!(fx.db.count("h").await, 0);
}

#[tokio::test]
async fn a_read_only_session_refuses_every_write_before_sending() {
    let fx = dy("").await;
    let mut w = fx.session(SessionOptions::default()).await;
    read(&mut w, "INSERT INTO \"h\" VALUE {'id': 'keep', 'v': '1'}").await;
    let mut ro = fx.session(forced()).await;
    for text in [
        "INSERT INTO \"h\" VALUE {'id': 'b', 'v': '1'}",
        "UPDATE \"h\" SET v = '9' WHERE id = 'keep'",
        "DELETE FROM \"h\" WHERE id = 'keep'",
        "EXISTS(SELECT * FROM \"h\" WHERE id = 'keep')",
    ] {
        assert!(
            matches!(
                failure(&mut ro, text).await,
                ExecuteError::ReadOnlyViolation { .. }
            ),
            "{text}"
        );
    }
    assert_eq!(fx.db.count("h").await, 1);
    assert_eq!(
        read(&mut ro, "SELECT v FROM \"h\" WHERE id = 'keep'")
            .await
            .rows,
        [vec![Cell::Text("1".into())]]
    );
}

#[tokio::test]
async fn a_missing_table_carries_the_server_code() {
    let fx = dy("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    assert_eq!(
        code_of(failure(&mut s, "SELECT * FROM \"nope\"").await),
        "ResourceNotFoundException"
    );
}

#[tokio::test]
async fn an_unknown_region_or_a_missing_secret_is_open() {
    let fx = dy("").await;
    let dir = tempfile::tempdir().unwrap();
    let toml = format!(
        "dbento = 1\n[connections.far]\nengine = \"dynamodb\"\nregion = \"xx-nowhere-9\"\nendpoint = \"{}\"\n\
         access_key_id = \"{DY_ACCESS_KEY}\"\nsecret = {{ env = \"{DY_SECRET_VAR}\" }}\n",
        fx.db.endpoint()
    );
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    match connect(
        &ws,
        ws.connection("far").unwrap(),
        Some(Secret::new(DY_SECRET)),
        SessionOptions::default(),
    )
    .await
    {
        Err(ConnectError::Open { message }) => assert!(
            message.contains(fx.db.endpoint()) && message.contains("500"),
            "{message}"
        ),
        Err(other) => panic!("{other}"),
        Ok(_) => panic!("an unknown region connected"),
    }
    match connect(&fx.ws, fx.conn(), None, SessionOptions::default()).await {
        Err(ConnectError::Open { message }) => assert!(message.contains("secret"), "{message}"),
        Err(other) => panic!("{other}"),
        Ok(_) => panic!("connected without a secret"),
    }
}
