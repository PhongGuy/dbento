//! The S3 session: a `Driver` on tokio that runs each command of a script through the one
//! S3 client, every call a read.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use aws_sdk_s3::config::SharedHttpClient;

use super::client::{Bucket, ClientSettings, ListCall, S3Client};
use super::command::{self, S3Command};
use super::errors::{BUCKETS_HINT, S3Failure, classify, execute_error, wrong_region_hint};
use super::lex::{S3Statement, SyntaxError, parse_script};
use super::stream::{Row, S3Cell, S3Stream, Shared};
use crate::aws::https_client;
use crate::introspect::{ColumnMeta, IntrospectError, RelationList, RelationRef, SchemaInfo};
use crate::session::{
    CancelHandle, Classification, ConnectError, ConnectParams, Driver, ExecuteError, ExtendError,
    Limits, Prepared, ReadOnlyEnforcement, StreamItem, Writes,
};
use crate::value::ColumnInfo;

/// How long the connect ping may take before the connect fails.
pub(crate) const PING_BOUND: Duration = Duration::from_secs(30);

/// What a table-shaped introspection call answers on S3, which has buckets and keys instead.
pub(crate) const NO_TABLES: &str = "tables (S3 browses with ls)";

pub(crate) struct S3Session {
    client: Arc<S3Client>,
    buckets: Option<Vec<String>>,
    shared: Arc<Shared>,
}

/// The parsed statements of one script; `current` is the one prepared and not yet run.
pub(crate) struct S3Script {
    client: Arc<S3Client>,
    shared: Arc<Shared>,
    buckets: Option<Vec<String>>,
    src: String,
    statements: VecDeque<Result<S3Statement, SyntaxError>>,
    current: Option<(S3Command, String)>,
}

impl S3Session {
    /// Builds the client and pings: ListBuckets, or a one-key listing of the first configured
    /// bucket. A denied or missing bucket still connects, since the keys were accepted.
    pub(crate) async fn open(
        settings: ClientSettings,
        buckets: Option<Vec<String>>,
        timeout: Option<Duration>,
        http: SharedHttpClient,
    ) -> Result<Self, ConnectError> {
        let client = S3Client::new(settings, http);
        let ping = async {
            match buckets.as_deref().and_then(<[String]>::first) {
                Some(bucket) => {
                    let call = ListCall {
                        bucket: bucket.clone(),
                        prefix: String::new(),
                        delimiter: None,
                        token: None,
                        max_keys: 1,
                    };
                    client.list_objects(&call).await.err().map(classify)
                }
                None => client.list_buckets().await.err().map(classify),
            }
        };
        let failure =
            tokio::time::timeout(PING_BOUND, ping)
                .await
                .map_err(|_| ConnectError::Open {
                    message: format!(
                        "the endpoint did not answer within {} s",
                        PING_BOUND.as_secs()
                    ),
                })?;
        if let Some(refusal) = failure.and_then(connect_refusal) {
            return Err(refusal);
        }
        Ok(S3Session {
            client: Arc::new(client),
            buckets,
            shared: Arc::new(Shared::new(timeout)),
        })
    }
}

/// Why a ping failure refuses the connect; `None` for the failures that still connect.
fn connect_refusal(failure: S3Failure) -> Option<ConnectError> {
    let message = match failure {
        S3Failure::AccessDenied { .. } | S3Failure::NotFound { .. } => return None,
        S3Failure::Auth { code, message } => {
            format!("{code}: {message}; check access_key_id and the secret")
        }
        S3Failure::WrongRegion { region } => wrong_region_hint(region.as_deref()),
        S3Failure::Timeout => "the endpoint did not answer in time".to_owned(),
        S3Failure::Transport { message } => format!("cannot reach the endpoint: {message}"),
        S3Failure::Service { code, message } => format!("{code}: {message}"),
        S3Failure::InvalidRange => "the endpoint refused the ping with InvalidRange".to_owned(),
    };
    Some(ConnectError::Open { message })
}

/// The configured buckets, else ListBuckets; a refused ListBuckets says how to name them.
async fn list_buckets(
    client: &S3Client,
    buckets: Option<&[String]>,
    shared: &Shared,
) -> Result<Vec<Bucket>, ExecuteError> {
    if let Some(names) = buckets {
        return Ok(names
            .iter()
            .map(|name| Bucket {
                name: name.clone(),
                created: None,
            })
            .collect());
    }
    let call = async {
        client.list_buckets().await.map_err(|e| {
            let failure = classify(e);
            let denied = matches!(failure, S3Failure::AccessDenied { .. });
            let mut error = execute_error(failure);
            if denied && let ExecuteError::Database { hint, .. } = &mut error {
                *hint = Some(BUCKETS_HINT.to_owned());
            }
            error
        })
    };
    shared.bounded(call).await
}

/// The read classification every S3 command gets: nothing it sends can write.
fn read_classification() -> Classification {
    Classification {
        writes: Writes::No,
        unguarded: Vec::new(),
        transaction_control: false,
        categories: Vec::new(),
    }
}

impl Driver for S3Session {
    type Script = S3Script;
    type Stream = S3Stream;

    async fn connect(params: ConnectParams<'_>) -> Result<Self, ConnectError> {
        let conn = params.conn;
        let settings =
            ClientSettings::from_connection(conn, params.secret, |k| std::env::var(k).ok())?;
        let buckets = conn.buckets().map(<[String]>::to_vec);
        S3Session::open(settings, buckets, params.timeout, https_client()).await
    }

    fn start_script(&mut self, sql: &str) -> S3Script {
        S3Script {
            client: Arc::clone(&self.client),
            shared: Arc::clone(&self.shared),
            buckets: self.buckets.clone(),
            src: sql.to_owned(),
            statements: parse_script(sql).into(),
            current: None,
        }
    }

    async fn prepare_next(script: &mut S3Script) -> Option<Result<Prepared, ExecuteError>> {
        let next = script.statements.pop_front()?;
        let parsed = if script.shared.closed.load(Ordering::Relaxed) {
            Err(ExecuteError::SessionClosed)
        } else {
            next.map_err(|e| ExecuteError::Syntax {
                message: e.message,
                offset: e.offset,
            })
            .and_then(|statement| {
                let parsed = command::parse(&statement.words)
                    .map_err(|message| ExecuteError::Syntax { message, offset: 0 })?;
                Ok((parsed, statement.range))
            })
        };
        let (parsed, range) = match parsed {
            Ok(parsed) => parsed,
            Err(failure) => {
                // A failure ends the script, so no later statement runs after it.
                script.statements.clear();
                return Some(Err(failure));
            }
        };
        let text = script.src.get(range).unwrap_or_default().to_owned();
        let prepared = Prepared {
            sql: text.clone(),
            classification: read_classification(),
        };
        script.current = Some((parsed, text));
        Some(Ok(prepared))
    }

    async fn is_last(script: &mut S3Script) -> bool {
        script.statements.is_empty()
    }

    async fn execute(
        script: &mut S3Script,
        limits: Limits,
    ) -> Result<(Vec<ColumnInfo>, S3Stream), ExecuteError> {
        let (parsed, _text) = script.current.take().ok_or(ExecuteError::SessionClosed)?;
        match parsed {
            S3Command::Buckets => {
                let buckets =
                    list_buckets(&script.client, script.buckets.as_deref(), &script.shared).await?;
                let rows: Vec<Row> = buckets
                    .into_iter()
                    .map(|b| {
                        vec![
                            S3Cell::Text(b.name),
                            b.created.map_or(S3Cell::Null, S3Cell::Text),
                        ]
                    })
                    .collect();
                let columns = vec![
                    ColumnInfo::new("bucket", "text", None),
                    ColumnInfo::new("created", "timestamp", None),
                ];
                let stream =
                    S3Stream::buffered(columns.len(), rows, limits, Arc::clone(&script.shared));
                Ok((columns, stream))
            }
            S3Command::List { .. } | S3Command::Stat(_) | S3Command::Cat(_) => {
                Err(ExecuteError::Unsupported {
                    reason: "not available yet".to_owned(),
                })
            }
        }
    }

    async fn next_event(stream: &mut S3Stream) -> Result<StreamItem, ExecuteError> {
        stream.next_event()
    }

    fn extend(stream: &mut S3Stream, more: Option<u64>) -> Result<(), ExtendError> {
        stream.extend(more)
    }

    fn cancel_handle(&self) -> CancelHandle {
        CancelHandle::s3(Arc::clone(&self.shared.cancel))
    }

    async fn settle(&mut self) -> Result<(), ExecuteError> {
        if self.shared.closed.load(Ordering::Relaxed) {
            return Err(ExecuteError::SessionClosed);
        }
        self.shared.cancel.spend();
        Ok(())
    }

    async fn close(self) -> Result<(), ExecuteError> {
        Ok(())
    }

    async fn schemas(&mut self) -> Result<Vec<SchemaInfo>, IntrospectError> {
        let buckets = list_buckets(&self.client, self.buckets.as_deref(), &self.shared).await?;
        Ok(buckets
            .into_iter()
            .map(|b| SchemaInfo::new(b.name))
            .collect())
    }

    async fn relations(&mut self, _schema: &str) -> Result<RelationList, IntrospectError> {
        Err(IntrospectError::Unsupported { what: NO_TABLES })
    }

    async fn columns(&mut self, _rel: &RelationRef) -> Result<Vec<ColumnMeta>, IntrospectError> {
        Err(IntrospectError::Unsupported { what: NO_TABLES })
    }

    fn read_only_level(&self, requested: bool) -> Option<ReadOnlyEnforcement> {
        requested.then_some(ReadOnlyEnforcement::BestEffort)
    }
}

#[cfg(test)]
pub(crate) mod tests_support {
    use super::S3Session;
    use crate::session::{Driver, ExecuteError, Limits, StreamItem};
    use crate::value::CellRef;

    /// Runs the first statement of `text` and returns its rows as text: bytes as lossy UTF-8,
    /// NULL as `None`.
    pub(crate) async fn run_one(
        s: &mut S3Session,
        text: &str,
    ) -> Result<Vec<Vec<Option<String>>>, ExecuteError> {
        let mut script = s.start_script(text);
        let Some(prepared) = S3Session::prepare_next(&mut script).await else {
            return Ok(Vec::new());
        };
        prepared?;
        let (_columns, mut stream) = S3Session::execute(&mut script, Limits::default()).await?;
        let mut rows = Vec::new();
        while let StreamItem::Batch(batch) = S3Session::next_event(&mut stream).await? {
            for row in 0..batch.len() {
                rows.push(
                    (0..batch.column_count())
                        .map(|col| match batch.cell(row, col) {
                            CellRef::Null => None,
                            CellRef::Text(t) => Some(t.to_owned()),
                            CellRef::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
                        })
                        .collect(),
                );
            }
        }
        Ok(rows)
    }
}

#[cfg(test)]
#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "a test's catch-all arm only prints the unexpected error"
)]
mod tests;
