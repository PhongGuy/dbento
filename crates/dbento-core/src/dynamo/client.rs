//! The one door to the DynamoDB API: an SDK client that only sends the allowlisted calls.

use std::time::Duration;

use aws_sdk_dynamodb::config::retry::{RetryConfig, RetryPartition, TokenBucket};
use aws_sdk_dynamodb::config::timeout::TimeoutConfig;
use aws_sdk_dynamodb::config::{BehaviorVersion, Credentials, Region, SharedHttpClient};
use aws_sdk_dynamodb::error::SdkError;
use aws_sdk_dynamodb::operation::describe_table::{DescribeTableError, DescribeTableOutput};
use aws_sdk_dynamodb::operation::execute_statement::{
    ExecuteStatementError, ExecuteStatementOutput,
};
use aws_sdk_dynamodb::operation::list_tables::{ListTablesError, ListTablesOutput};
use aws_sdk_dynamodb::operation::scan::{ScanError, ScanOutput};
use aws_sdk_dynamodb::types::ReturnConsumedCapacity;
use aws_smithy_http_client::tls::{self, rustls_provider::CryptoMode};

use super::creds::{AwsCredentials, resolve_credentials, resolve_region};
use super::ops::{DynamoOp, allowed};
use super::statement::Kind;
use crate::secrets::Secret;
use crate::session::ConnectError;
use crate::workspace::ConnectionConfig;

/// How long opening a TCP connection may take before the call fails.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Everything a client is built from; `endpoint` is already an origin (see [`endpoint_origin`]).
pub(crate) struct ClientSettings {
    pub(crate) region: String,
    pub(crate) endpoint: Option<String>,
    pub(crate) credentials: AwsCredentials,
    pub(crate) connect_timeout: Duration,
    pub(crate) operation_timeout: Option<Duration>,
    pub(crate) read_only: bool,
}

impl ClientSettings {
    /// Region, credentials and endpoint origin of `conn`; `env` stands in for the process environment.
    pub(crate) fn from_connection(
        conn: &ConnectionConfig,
        secret: Option<&Secret>,
        timeout: Option<Duration>,
        read_only: bool,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<ClientSettings, ConnectError> {
        let open = |message: String| ConnectError::Open { message };
        let region = resolve_region(conn, &env).map_err(open)?;
        let credentials = resolve_credentials(conn, secret, &env).map_err(open)?;
        let endpoint = match conn.endpoint() {
            Some(text) => Some(
                endpoint_origin(text)
                    .ok_or_else(|| open(format!("endpoint `{text}` is not a URL with a host")))?,
            ),
            None => None,
        };
        Ok(ClientSettings {
            region,
            endpoint,
            credentials,
            connect_timeout: CONNECT_TIMEOUT,
            operation_timeout: timeout,
            read_only,
        })
    }
}

/// A call the allowlist refused before sending, or the SDK's own failure (boxed: it runs to
/// hundreds of bytes and would bloat every `Result`).
#[derive(Debug)]
pub(crate) enum CallError<E> {
    Refused(DynamoOp),
    Sdk(Box<SdkError<E>>),
}

impl<E> From<SdkError<E>> for CallError<E> {
    fn from(error: SdkError<E>) -> Self {
        CallError::Sdk(Box::new(error))
    }
}

/// One PartiQL statement, sent exactly as written; `kind` decides the gate and the retries.
pub(crate) struct StatementRequest<'a> {
    pub(crate) kind: Kind,
    pub(crate) text: &'a str,
    pub(crate) next_token: Option<&'a str>,
    pub(crate) consistent_read: bool,
}

/// The SDK client with the read-only allowlist in front of every method.
pub(crate) struct DynamoClient {
    inner: aws_sdk_dynamodb::Client,
    read_only: bool,
}

impl DynamoClient {
    /// Builds the client over `http`; nothing is sent until the first call.
    pub(crate) fn new(settings: ClientSettings, http: SharedHttpClient) -> Self {
        let ClientSettings {
            region,
            endpoint,
            credentials,
            connect_timeout,
            operation_timeout,
            read_only,
        } = settings;
        let mut timeouts = TimeoutConfig::builder().connect_timeout(connect_timeout);
        if let Some(limit) = operation_timeout {
            timeouts = timeouts.operation_timeout(limit);
        }
        let credentials = Credentials::new(
            credentials.access_key_id,
            credentials.secret_access_key.expose(),
            credentials.session_token.map(|t| t.expose().to_owned()),
            None,
            "dbento",
        );
        let mut config = aws_sdk_dynamodb::Config::builder()
            .behavior_version(BehaviorVersion::v2026_11_10())
            .retry_partition(own_retry_quota(&region))
            .region(Region::new(region))
            .timeout_config(timeouts.build())
            .credentials_provider(credentials)
            .http_client(http);
        if let Some(endpoint) = endpoint {
            config = config.endpoint_url(endpoint);
        }
        DynamoClient {
            inner: aws_sdk_dynamodb::Client::from_conf(config.build()),
            read_only,
        }
    }

    fn gate<E>(&self, op: DynamoOp) -> Result<(), CallError<E>> {
        if allowed(op, self.read_only) {
            Ok(())
        } else {
            Err(CallError::Refused(op))
        }
    }

    /// One page of table names, starting after `start`.
    pub(crate) async fn list_tables(
        &self,
        start: Option<&str>,
        limit: Option<i32>,
    ) -> Result<ListTablesOutput, CallError<ListTablesError>> {
        self.gate(DynamoOp::ListTables)?;
        self.inner
            .list_tables()
            .set_exclusive_start_table_name(start.map(str::to_owned))
            .set_limit(limit)
            .send()
            .await
            .map_err(CallError::from)
    }

    /// The table's key schema, indexes and attribute definitions.
    pub(crate) async fn describe_table(
        &self,
        table: &str,
    ) -> Result<DescribeTableOutput, CallError<DescribeTableError>> {
        self.gate(DynamoOp::DescribeTable)?;
        self.inner
            .describe_table()
            .table_name(table)
            .send()
            .await
            .map_err(CallError::from)
    }

    /// The first `limit` items of the table, as a sample.
    pub(crate) async fn scan(
        &self,
        table: &str,
        limit: i32,
    ) -> Result<ScanOutput, CallError<ScanError>> {
        self.gate(DynamoOp::Scan)?;
        self.inner
            .scan()
            .table_name(table)
            .limit(limit)
            .send()
            .await
            .map_err(CallError::from)
    }

    /// Sends one statement with its capacity reported. Only a `SELECT` is retried: a write
    /// that timed out may have landed, so anything else gets a single attempt.
    pub(crate) async fn execute_statement(
        &self,
        req: StatementRequest<'_>,
    ) -> Result<ExecuteStatementOutput, CallError<ExecuteStatementError>> {
        self.gate(DynamoOp::ExecuteStatement(req.kind))?;
        let mut call = self
            .inner
            .execute_statement()
            .statement(req.text)
            .return_consumed_capacity(ReturnConsumedCapacity::Total)
            .set_next_token(req.next_token.map(str::to_owned));
        if req.consistent_read {
            call = call.consistent_read(true);
        }
        let sent = if req.kind == Kind::Select {
            call.send().await
        } else {
            let once = RetryConfig::standard().with_max_attempts(1);
            call.customize()
                .config_override(aws_sdk_dynamodb::config::Builder::default().retry_config(once))
                .send()
                .await
        };
        sent.map_err(CallError::from)
    }
}

/// A retry quota this client alone draws on. The SDK's default partition is one bucket per
/// region for the whole process, so one connection's failures could refuse every other's
/// retries. The costs are those the SDK's Retry Behavior 2.1 gives its default bucket.
fn own_retry_quota(region: &str) -> RetryPartition {
    let bucket = TokenBucket::builder()
        .retry_cost(14)
        .throttling_retry_cost(5)
        .timeout_retry_cost(14)
        .build();
    RetryPartition::custom(format!("dbento-dynamodb-{region}"))
        .token_bucket(bucket)
        .build()
}

/// An HTTPS client on ring rustls, the one every real connection uses.
pub(crate) fn https_client() -> SharedHttpClient {
    aws_smithy_http_client::Builder::new()
        .tls_provider(tls::Provider::Rustls(CryptoMode::Ring))
        .build_https()
}

/// The WHATWG-normalised origin of an endpoint, or `None` when it is not a URL with a host.
pub(crate) fn endpoint_origin(text: &str) -> Option<String> {
    let origin = url::Url::parse(text).ok()?.origin();
    origin.is_tuple().then(|| origin.ascii_serialization())
}

#[cfg(test)]
mod tests;
