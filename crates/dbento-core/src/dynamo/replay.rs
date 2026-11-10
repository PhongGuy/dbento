//! A scripted HTTP client for the SDK: it records each request and answers from a queue, so
//! paging, retries, deadlines and error bodies are tested without a server.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aws_sdk_dynamodb::config::RuntimeComponents;
use aws_smithy_runtime_api::client::http::{
    HttpClient, HttpConnector, HttpConnectorFuture, HttpConnectorSettings, SharedHttpClient,
    SharedHttpConnector,
};
use aws_smithy_runtime_api::client::orchestrator::{HttpRequest, HttpResponse};
use aws_smithy_runtime_api::client::result::ConnectorError;
use aws_smithy_runtime_api::http::StatusCode;
use aws_smithy_types::body::SdkBody;
use serde_json::{Value, json};

use super::client::{CONNECT_TIMEOUT, ClientSettings, DynamoClient};
use super::creds::AwsCredentials;
use crate::secrets::Secret;

/// The connection a replayed session reports as its target.
pub(crate) const TARGET: &str = "eu-west-1 at http://replay.test";
/// The secret key every replayed client signs with.
pub(crate) const SECRET: &str = "replay-secret";

/// One scripted answer: a status, a JSON body and how long to wait before sending it.
#[derive(Clone)]
pub(crate) struct Reply {
    status: u16,
    body: String,
    delay: Duration,
}

impl Reply {
    pub(crate) fn ok(body: impl Into<String>) -> Self {
        Reply::error(200, body)
    }

    pub(crate) fn error(status: u16, body: impl Into<String>) -> Self {
        Reply {
            status,
            body: body.into(),
            delay: Duration::ZERO,
        }
    }

    /// The same answer, sent only after `delay`.
    pub(crate) fn after(self, delay: Duration) -> Self {
        Reply { delay, ..self }
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
pub(crate) struct Replay {
    replies: Arc<Mutex<VecDeque<Reply>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl fmt::Debug for Replay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Replay")
    }
}

impl Replay {
    pub(crate) fn new(replies: Vec<Reply>) -> Self {
        Replay {
            replies: Arc::new(Mutex::new(replies.into())),
            seen: Arc::default(),
        }
    }

    /// Every request so far, in order.
    pub(crate) fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    pub(crate) fn http(&self) -> SharedHttpClient {
        SharedHttpClient::new(self.clone())
    }

    pub(crate) fn client(&self, read_only: bool) -> DynamoClient {
        self.client_with(test_settings(read_only, None))
    }

    pub(crate) fn client_with(&self, settings: ClientSettings) -> DynamoClient {
        DynamoClient::new(settings, self.http())
    }
}

impl HttpConnector for Replay {
    fn call(&self, request: HttpRequest) -> HttpConnectorFuture {
        // The op name is the X-Amz-Target suffix: DynamoDB_20120810.<Op>.
        let target = request.headers().get("x-amz-target").unwrap_or_default();
        let target = target.rsplit('.').next().unwrap_or_default().to_owned();
        let body = request
            .body()
            .bytes()
            .and_then(|b| serde_json::from_slice(b).ok())
            .unwrap_or(Value::Null);
        self.seen.lock().unwrap().push(Seen { target, body });
        let next = self.replies.lock().unwrap().pop_front();
        HttpConnectorFuture::new(async move {
            let Some(reply) = next else {
                return Err(ConnectorError::other(
                    "the replay ran out of replies".into(),
                    None,
                ));
            };
            tokio::time::sleep(reply.delay).await;
            let status = StatusCode::try_from(reply.status).expect("a valid status");
            let mut response = HttpResponse::new(status, SdkBody::from(reply.body));
            response
                .headers_mut()
                .insert("content-type", "application/x-amz-json-1.0");
            Ok(response)
        })
    }
}

impl HttpClient for Replay {
    fn http_connector(
        &self,
        _: &HttpConnectorSettings,
        _: &RuntimeComponents,
    ) -> SharedHttpConnector {
        SharedHttpConnector::new(self.clone())
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
