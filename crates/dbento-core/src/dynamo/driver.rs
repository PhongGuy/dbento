//! The DynamoDB session: a `Driver` on tokio that sends each PartiQL statement of a script as
//! its own range of the text, through the one allowlisted client.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use aws_sdk_dynamodb::config::SharedHttpClient;

use super::catalog::{self, TableKeys};
use super::classify::partiql_classification;
use super::client::{ClientSettings, DynamoClient, https_client};
use super::errors::connect_error;
use super::lex::SyntaxError;
use super::statement::{Kind, PartiqlStatement, parse_script};
use super::stream::{DynamoStream, Request, Shape, Shared};
use crate::introspect::{ColumnMeta, IntrospectError, RelationList, RelationRef, SchemaInfo};
use crate::objects::RelationObjects;
use crate::session::{
    CancelHandle, Classification, ConnectError, ConnectParams, Driver, ExecuteError, ExtendError,
    Flavor, Limits, Prepared, ReadOnlyEnforcement, ServerInfo, StreamItem, Writes,
};
use crate::value::ColumnInfo;

/// How long the connect ping may take before the connect fails.
pub(crate) const PING_BOUND: Duration = Duration::from_secs(30);

/// Each table's keys, shared by the session and its scripts; a failed lookup is never cached.
#[derive(Default)]
struct KeyCache(Mutex<HashMap<String, Arc<TableKeys>>>);

impl KeyCache {
    fn map(&self) -> MutexGuard<'_, HashMap<String, Arc<TableKeys>>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The table's keys, from the cache or one `DescribeTable`; `None` when it does not exist.
    async fn get(
        &self,
        client: &DynamoClient,
        table: &str,
    ) -> Result<Option<Arc<TableKeys>>, ExecuteError> {
        if let Some(keys) = self.map().get(table) {
            return Ok(Some(Arc::clone(keys)));
        }
        let Some(keys) = catalog::describe(client, table).await? else {
            return Ok(None);
        };
        let keys = Arc::new(keys);
        self.map().insert(table.to_owned(), Arc::clone(&keys));
        Ok(Some(keys))
    }
}

pub(crate) struct DynamoSession {
    client: Arc<DynamoClient>,
    region: String,
    consistent_read: bool,
    read_only: bool,
    shared: Arc<Shared>,
    keys: Arc<KeyCache>,
    columns: HashMap<String, Vec<ColumnMeta>>,
}

/// The parsed statements of one script; `current` is the one prepared and not yet run.
pub(crate) struct DynamoScript {
    client: Arc<DynamoClient>,
    consistent_read: bool,
    read_only: bool,
    shared: Arc<Shared>,
    keys: Arc<KeyCache>,
    src: String,
    statements: VecDeque<Result<PartiqlStatement, SyntaxError>>,
    current: Option<(PartiqlStatement, Classification)>,
}

impl DynamoSession {
    /// Builds the client and pings with a one-table `ListTables`. A denied ping still connects:
    /// the keys were accepted, and the user may hold only narrower rights.
    pub(crate) async fn open(
        settings: ClientSettings,
        target: String,
        consistent_read: bool,
        http: SharedHttpClient,
    ) -> Result<Self, ConnectError> {
        let region = settings.region.clone();
        let read_only = settings.read_only;
        let timeout = settings.operation_timeout;
        let client = DynamoClient::new(settings, http);
        match tokio::time::timeout(PING_BOUND, client.list_tables(None, Some(1))).await {
            Err(_) => {
                return Err(ConnectError::Open {
                    message: format!(
                        "{target}: the endpoint did not answer within {} s",
                        PING_BOUND.as_secs()
                    ),
                });
            }
            Ok(Err(e)) => {
                if let Some(failure) = connect_error(e, &target) {
                    return Err(failure);
                }
            }
            Ok(Ok(_)) => {}
        }
        Ok(DynamoSession {
            client: Arc::new(client),
            region,
            consistent_read,
            read_only,
            shared: Arc::new(Shared::new(timeout)),
            keys: Arc::default(),
            columns: HashMap::new(),
        })
    }

    /// DynamoDB has no server version to ask for, so this reports the API version.
    pub(crate) fn server(&self) -> ServerInfo {
        ServerInfo {
            flavor: Flavor::DynamoDb,
            version: (2012, 8, 10),
        }
    }

    /// The table's GSIs and LSIs; an unknown schema or table has none.
    pub(crate) async fn relation_objects(
        &mut self,
        rel: &RelationRef,
    ) -> Result<RelationObjects, IntrospectError> {
        match self.table_keys(rel).await? {
            Some(keys) => Ok(catalog::relation_objects(rel, &keys)),
            None => Ok(RelationObjects::new(Vec::new(), Vec::new(), Vec::new())),
        }
    }

    /// The table's declared key attributes from `DescribeTable` alone; none for an unknown
    /// schema or table.
    pub(crate) async fn key_columns(
        &mut self,
        rel: &RelationRef,
    ) -> Result<Vec<ColumnMeta>, IntrospectError> {
        Ok(self
            .table_keys(rel)
            .await?
            .map(|keys| catalog::key_columns(&keys))
            .unwrap_or_default())
    }

    async fn table_keys(
        &self,
        rel: &RelationRef,
    ) -> Result<Option<Arc<TableKeys>>, IntrospectError> {
        if rel.schema() != self.region {
            return Ok(None);
        }
        Ok(self.keys.get(&self.client, rel.name()).await?)
    }
}

impl Driver for DynamoSession {
    type Script = DynamoScript;
    type Stream = DynamoStream;

    async fn connect(params: ConnectParams<'_>) -> Result<Self, ConnectError> {
        let conn = params.conn;
        let settings = ClientSettings::from_connection(
            conn,
            params.secret,
            params.timeout,
            params.read_only,
            |k| std::env::var(k).ok(),
        )?;
        let target = conn.display_target();
        DynamoSession::open(settings, target, conn.consistent_read(), https_client()).await
    }

    fn start_script(&mut self, sql: &str) -> DynamoScript {
        DynamoScript {
            client: Arc::clone(&self.client),
            consistent_read: self.consistent_read,
            read_only: self.read_only,
            shared: Arc::clone(&self.shared),
            keys: Arc::clone(&self.keys),
            src: sql.to_owned(),
            statements: parse_script(sql).into(),
            current: None,
        }
    }

    async fn prepare_next(script: &mut DynamoScript) -> Option<Result<Prepared, ExecuteError>> {
        let next = script.statements.pop_front()?;
        let parsed = if script.shared.closed.load(Ordering::Relaxed) {
            Err(ExecuteError::SessionClosed)
        } else {
            next.map_err(|e| ExecuteError::Syntax {
                message: e.message,
                offset: e.offset,
            })
        };
        let statement = match parsed {
            Ok(statement) => statement,
            Err(failure) => {
                // A failure ends the script, so no later statement runs after it.
                script.statements.clear();
                return Some(Err(failure));
            }
        };
        let classification = partiql_classification(&statement);
        let prepared = Prepared {
            sql: script.src[statement.range.clone()].to_owned(),
            classification: classification.clone(),
        };
        script.current = Some((statement, classification));
        Some(Ok(prepared))
    }

    async fn is_last(script: &mut DynamoScript) -> bool {
        script.statements.is_empty()
    }

    async fn execute(
        script: &mut DynamoScript,
        limits: Limits,
    ) -> Result<(Vec<ColumnInfo>, DynamoStream), ExecuteError> {
        let (statement, classification) =
            script.current.take().ok_or(ExecuteError::SessionClosed)?;
        // The gate sits here, not in `prepare_next`, so a shell can read a refused statement's
        // classification. DynamoDB has no read-only session, so an unknown statement is refused.
        if script.read_only {
            let message = match classification.writes {
                Writes::No => None,
                Writes::Yes => Some("this statement writes"),
                Writes::Unknown => Some(
                    "Dbento cannot tell whether this statement writes, and DynamoDB has no \
                     read-only session",
                ),
            };
            if let Some(message) = message {
                return Err(ExecuteError::ReadOnlyViolation {
                    message: message.to_owned(),
                });
            }
        }
        // Without a projection the table's keys lead the columns. The lookup is only a hint:
        // a failure leaves the columns in name order and the statement still runs.
        let keys = match (&statement.projection, &statement.target) {
            (None, Some(target)) => {
                let lookup = script.keys.get(&script.client, &target.table);
                match script.shared.cancel.guard(lookup).await? {
                    Ok(Some(keys)) => keys.key_names(),
                    Ok(None) | Err(_) => Vec::new(),
                }
            }
            (Some(_), _) | (None, None) => Vec::new(),
        };
        let consistent_read = script.consistent_read
            && statement.kind == Kind::Select
            && statement.target.as_ref().is_some_and(|t| t.index.is_none());
        let request = Request {
            kind: statement.kind,
            text: script.src[statement.range.clone()].to_owned(),
            consistent_read,
        };
        let shape = Shape {
            projection: statement.projection.as_deref(),
            keys: &keys,
        };
        DynamoStream::open(
            Arc::clone(&script.client),
            request,
            shape,
            limits,
            Arc::clone(&script.shared),
        )
        .await
    }

    async fn next_event(stream: &mut DynamoStream) -> Result<StreamItem, ExecuteError> {
        stream.next_event().await
    }

    fn extend(stream: &mut DynamoStream, more: Option<u64>) -> Result<(), ExtendError> {
        stream.extend(more)
    }

    fn cancel_handle(&self) -> CancelHandle {
        CancelHandle::dynamo(Arc::clone(&self.shared.cancel))
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
        Ok(catalog::schemas(&self.region))
    }

    async fn relations(&mut self, schema: &str) -> Result<RelationList, IntrospectError> {
        catalog::relations(&self.client, &self.region, schema).await
    }

    async fn columns(&mut self, rel: &RelationRef) -> Result<Vec<ColumnMeta>, IntrospectError> {
        if rel.schema() == self.region
            && let Some(columns) = self.columns.get(rel.name())
        {
            return Ok(columns.clone());
        }
        let Some(keys) = self.table_keys(rel).await? else {
            return Ok(Vec::new());
        };
        let columns = catalog::columns(&self.client, rel.name(), &keys).await?;
        self.columns.insert(rel.name().to_owned(), columns.clone());
        Ok(columns)
    }

    fn read_only_level(&self, requested: bool) -> Option<ReadOnlyEnforcement> {
        requested.then_some(ReadOnlyEnforcement::BestEffort)
    }
}

#[cfg(test)]
mod tests;
