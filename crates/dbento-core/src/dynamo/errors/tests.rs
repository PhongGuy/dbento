use std::time::Duration;

use super::*;
use crate::dynamo::client::{CallError, DynamoClient, StatementRequest};
use crate::dynamo::ops::DynamoOp;
use crate::dynamo::replay::{Replay, Reply, SECRET, TARGET, aws_error, page, test_settings};
use crate::dynamo::statement::Kind;
use crate::session::{ConnectError, ExecuteError};

fn insert() -> StatementRequest<'static> {
    StatementRequest {
        kind: Kind::Insert,
        text: "x",
        next_token: None,
        consistent_read: false,
    }
}

/// The mapped error of one insert, which gets a single attempt.
async fn failed(client: &DynamoClient) -> ExecuteError {
    execute_error(
        client
            .execute_statement(insert())
            .await
            .expect_err("an error"),
    )
}

async fn failed_with(reply: Reply) -> ExecuteError {
    failed(&Replay::new(vec![reply]).client(false)).await
}

fn database(e: ExecuteError) -> (String, String, Option<String>) {
    match e {
        ExecuteError::Database {
            code,
            message,
            hint,
            ..
        } => (code, message, hint),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn json_error_bodies_keep_their_code_and_message() {
    for code in [
        "ValidationException",
        "AccessDeniedException",
        "DuplicateItemException",
        "ConditionalCheckFailedException",
        "ResourceNotFoundException",
    ] {
        let e = failed_with(Reply::error(400, aws_error(code, &format!("m-{code}")))).await;
        assert_eq!(
            database(e),
            (code.to_owned(), format!("m-{code}"), None),
            "{code}"
        );
    }
}

#[tokio::test]
async fn every_throttling_code_carries_the_capacity_hint() {
    for code in [
        "ProvisionedThroughputExceededException",
        "RequestLimitExceeded",
        "ThrottlingException",
    ] {
        let (got, _, hint) =
            database(failed_with(Reply::error(400, aws_error(code, "slow"))).await);
        assert_eq!(
            (got.as_str(), hint.as_deref()),
            (code, Some(THROTTLED_HINT)),
            "{code}"
        );
    }
}

#[tokio::test]
async fn a_body_that_is_not_json_keeps_the_http_status() {
    let e = failed_with(Reply::error(500, "<!doctype html><title>500</title>")).await;
    let (code, message, _) = database(e);
    assert_eq!(code, "HTTP 500");
    assert!(
        message.contains("500") && message.contains("not DynamoDB JSON"),
        "{message}"
    );
}

#[tokio::test(start_paused = true)]
async fn the_operation_timeout_is_a_timeout() {
    let replay = Replay::new(vec![
        Reply::ok(page(&[], None, None)).after(Duration::from_secs(30)),
    ]);
    let client = replay.client_with(test_settings(false, Some(Duration::from_secs(5))));
    assert!(matches!(failed(&client).await, ExecuteError::Timeout));
}

#[test]
fn a_refused_call_is_a_read_only_violation() {
    let refused: CallError<aws_sdk_dynamodb::operation::execute_statement::ExecuteStatementError> =
        CallError::Refused(DynamoOp::ExecuteStatement(Kind::Delete));
    match execute_error(refused) {
        ExecuteError::ReadOnlyViolation { message } => {
            assert!(message.contains("ExecuteStatement"), "{message}")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn only_a_validation_error_gains_the_stale_token_hint() {
    let stale = stale_token(ExecuteError::database("ValidationException", "bad token"));
    assert_eq!(database(stale).2.as_deref(), Some(STALE_TOKEN_HINT));
    let other = stale_token(ExecuteError::database("ThrottlingException", "slow"));
    assert_eq!(database(other).2, None);
}

async fn ping(replies: Vec<Reply>) -> Option<ConnectError> {
    let replay = Replay::new(replies);
    let e = replay
        .client(false)
        .list_tables(None, Some(1))
        .await
        .expect_err("an error");
    connect_error(e, TARGET)
}

#[tokio::test]
async fn an_auth_failure_names_its_code_and_never_the_secret() {
    for code in [
        "UnrecognizedClientException",
        "InvalidClientTokenId",
        "InvalidSignatureException",
        "ExpiredTokenException",
    ] {
        match ping(vec![Reply::error(
            400,
            aws_error(code, "The security token is invalid."),
        )])
        .await
        {
            Some(ConnectError::Open { message }) => {
                assert!(
                    message.contains(code) && message.contains(TARGET),
                    "{message}"
                );
                assert!(
                    message.contains("access_key_id") && !message.contains(SECRET),
                    "{message}"
                );
            }
            other => panic!("{code}: {other:?}"),
        }
    }
}

#[tokio::test]
async fn access_denied_on_the_ping_is_no_connect_error() {
    let denied = Reply::error(400, aws_error("AccessDeniedException", "not authorized"));
    assert!(ping(vec![denied]).await.is_none());
}

#[tokio::test(start_paused = true)]
async fn an_html_reply_or_no_reply_names_the_endpoint() {
    let html = || Reply::error(500, "<html>boom</html>");
    match ping(vec![html(), html(), html()]).await {
        Some(ConnectError::Open { message }) => assert!(
            message.contains(TARGET) && message.contains("HTTP 500"),
            "{message}"
        ),
        other => panic!("{other:?}"),
    }
    match ping(vec![]).await {
        Some(ConnectError::Open { message }) => {
            assert!(message.contains("cannot reach the endpoint"), "{message}")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_failure_before_sending_is_other_and_not_unreachable() {
    let e: CallError<aws_sdk_dynamodb::operation::list_tables::ListTablesError> =
        SdkError::construction_failure("the request could not be built").into();
    match connect_error(e, TARGET) {
        Some(ConnectError::Open { message }) => {
            assert!(
                message.starts_with(TARGET) && message.contains("could not be built"),
                "{message}"
            );
            assert!(!message.contains("cannot reach"), "{message}");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn a_ping_that_times_out_in_dispatch_reports_the_timeout() {
    let replay = Replay::new(vec![Reply::dispatch_timeout(); 3]);
    let e = replay
        .client(false)
        .list_tables(None, Some(1))
        .await
        .expect_err("an error");
    match connect_error(e, TARGET) {
        Some(ConnectError::Open { message }) => assert!(
            message.contains(TARGET) && message.contains("timeout"),
            "{message}"
        ),
        other => panic!("{other:?}"),
    }
}
