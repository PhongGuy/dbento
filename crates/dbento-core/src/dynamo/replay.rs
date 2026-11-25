//! DynamoDB's view of the scripted AWS client: JSON replies and requests named by their
//! X-Amz-Target operation.

use std::fmt;
use std::time::Duration;

use aws_smithy_runtime_api::client::http::SharedHttpClient;
use serde_json::{Value, json};

use super::client::{CONNECT_TIMEOUT, ClientSettings, DynamoClient};
use crate::aws::AwsCredentials;
use crate::aws::replay::{RawReplay, RawReply};
use crate::secrets::Secret;

/// The connection a replayed session reports as its target.
pub(crate) const TARGET: &str = "eu-west-1 at http://replay.test";
/// The secret key every replayed client signs with.
pub(crate) const SECRET: &str = "replay-secret";

/// One scripted answer: a JSON body with DynamoDB's content type, or a connector timeout.
#[derive(Clone)]
pub(crate) struct Reply(RawReply);

impl Reply {
    pub(crate) fn ok(body: impl Into<String>) -> Self {
        Reply::error(200, body)
    }

    pub(crate) fn error(status: u16, body: impl Into<String>) -> Self {
        Reply(
            RawReply::status(status)
                .header("content-type", "application/x-amz-json-1.0")
                .body(body.into().into_bytes()),
        )
    }

    /// A connector timeout, as the SDK sees a connect or read that never finished.
    pub(crate) fn dispatch_timeout() -> Self {
        Reply(RawReply::dispatch_timeout())
    }

    /// The same answer, sent only after `delay`.
    pub(crate) fn after(self, delay: Duration) -> Self {
        Reply(self.0.after(delay))
    }
}

/// A request as the server saw it: the operation name and the parsed JSON body.
#[derive(Debug, Clone)]
pub(crate) struct Seen {
    pub(crate) target: String,
    pub(crate) body: Value,
}

/// The scripted client; clones share the queue and the record.
#[derive(Clone, Default)]
pub(crate) struct Replay(RawReplay);

impl fmt::Debug for Replay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Replay")
    }
}

impl Replay {
    pub(crate) fn new(replies: Vec<Reply>) -> Self {
        Replay(RawReplay::new(replies.into_iter().map(|r| r.0).collect()))
    }

    /// Every request so far, in order, by the operation its X-Amz-Target names
    /// (`DynamoDB_20120810.<Op>`).
    pub(crate) fn seen(&self) -> Vec<Seen> {
        self.0
            .seen()
            .into_iter()
            .map(|raw| Seen {
                target: raw
                    .header("x-amz-target")
                    .unwrap_or_default()
                    .rsplit('.')
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
                body: serde_json::from_slice(&raw.body).unwrap_or(Value::Null),
            })
            .collect()
    }

    pub(crate) fn http(&self) -> SharedHttpClient {
        self.0.http()
    }

    pub(crate) fn client(&self, read_only: bool) -> DynamoClient {
        self.client_with(test_settings(read_only, None))
    }

    pub(crate) fn client_with(&self, settings: ClientSettings) -> DynamoClient {
        DynamoClient::new(settings, self.http())
    }
}

/// Settings for a client pointed at the replay endpoint.
pub(crate) fn test_settings(read_only: bool, timeout: Option<Duration>) -> ClientSettings {
    ClientSettings {
        region: "eu-west-1".to_owned(),
        endpoint: Some("http://replay.test".to_owned()),
        credentials: AwsCredentials {
            access_key_id: "AKIDREPLAY".to_owned(),
            secret_access_key: Secret::new(SECRET),
            session_token: None,
        },
        connect_timeout: CONNECT_TIMEOUT,
        operation_timeout: timeout,
        read_only,
    }
}

/// An ExecuteStatement answer (API_ExecuteStatement.html: Items, NextToken, ConsumedCapacity).
pub(crate) fn page(items: &[Value], next: Option<&str>, capacity: Option<f64>) -> String {
    let mut body = json!({ "Items": items });
    if let Some(token) = next {
        body["NextToken"] = json!(token);
    }
    if let Some(units) = capacity {
        body["ConsumedCapacity"] = json!({ "CapacityUnits": units });
    }
    body.to_string()
}

/// An error body as every operation sends it (Programming.Errors.html: `__type` and `message`).
pub(crate) fn aws_error(code: &str, message: &str) -> String {
    json!({
        "__type": format!("com.amazonaws.dynamodb.v20120810#{code}"),
        "message": message,
    })
    .to_string()
}

/// A ListTables answer (API_ListTables.html: TableNames, LastEvaluatedTableName).
pub(crate) fn tables(names: &[&str], last: Option<&str>) -> String {
    let mut body = json!({ "TableNames": names });
    if let Some(last) = last {
        body["LastEvaluatedTableName"] = json!(last);
    }
    body.to_string()
}

/// A DescribeTable answer wrapping a TableDescription (API_DescribeTable.html: Table).
pub(crate) fn described(table: Value) -> String {
    json!({ "Table": table }).to_string()
}

/// A Scan answer with every item on one page (API_Scan.html: Items, Count, ScannedCount).
pub(crate) fn scanned(items: &[Value]) -> String {
    json!({ "Items": items, "Count": items.len(), "ScannedCount": items.len() }).to_string()
}

/// The `orders` TableDescription (API_TableDescription.html): pk S HASH, sk N RANGE, a GSI
/// `by_status` on (status, created) and an LSI `by_created` on (pk, created).
pub(crate) fn orders() -> Value {
    let key = |hash: &str, range: &str| {
        json!([
            { "AttributeName": hash, "KeyType": "HASH" },
            { "AttributeName": range, "KeyType": "RANGE" },
        ])
    };
    json!({
        "TableName": "orders",
        "TableStatus": "ACTIVE",
        "KeySchema": key("pk", "sk"),
        "AttributeDefinitions": [
            { "AttributeName": "pk", "AttributeType": "S" },
            { "AttributeName": "sk", "AttributeType": "N" },
            { "AttributeName": "status", "AttributeType": "S" },
            { "AttributeName": "created", "AttributeType": "N" },
        ],
        "GlobalSecondaryIndexes": [{
            "IndexName": "by_status",
            "KeySchema": key("status", "created"),
            "Projection": { "ProjectionType": "ALL" },
            "IndexStatus": "ACTIVE",
        }],
        "LocalSecondaryIndexes": [{
            "IndexName": "by_created",
            "KeySchema": key("pk", "created"),
            "Projection": { "ProjectionType": "ALL" },
        }],
    })
}
