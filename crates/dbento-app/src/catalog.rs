//! One connection's catalog session, owned by a tokio task. The schema tree sends loads and
//! reads replies in request order; the session is read-only and separate from any query tab.

use std::panic::AssertUnwindSafe;
use std::pin::pin;
use std::time::Duration;

use async_channel::{Receiver, Sender};
use dbento_core::{
    CellRef, ColumnMeta, Definition, DefinitionError, ExecuteError, IntrospectError,
    KeyBrowseError, KeyCursor, KeyPage, KeyScan, KeyTtl, ObjectRef, RelationList, RelationObjects,
    RelationRef, SchemaInfo, SchemaObjects, Session, SessionOptions, quote_ident,
};
use futures_util::FutureExt as _;
use futures_util::future::{Either, select};

use crate::keys::{KEY_PAGE, KeyBatch, KeyFilter, KeyItem, KeyToken, kind_of};
use crate::runner::{self, RunnerSpec};

/// The longest a catalog query may run; a shorter connection timeout wins.
pub const CATALOG_TIMEOUT: Duration = Duration::from_secs(30);

/// How long past the statement timeout a load waits for the server's own error before the
/// client gives up on the connection, as on a network partition.
pub const CATALOG_GRACE: Duration = Duration::from_secs(5);

// A session given up on closes in the background, and is dropped if even that stalls.
const ABANDON_CAP: Duration = Duration::from_secs(10);

/// One level of the catalog to load.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CatalogQuery {
    /// The connection's schemas.
    Schemas,
    /// The tables and views of `schema`.
    Relations {
        /// The schema to list.
        schema: String,
    },
    /// The columns of `rel`.
    Columns {
        /// The relation to describe.
        rel: RelationRef,
    },
    /// The row count of `rel`, by a read-only `SELECT count(*)`; never a tree level.
    Count {
        /// The relation to count.
        rel: RelationRef,
    },
    /// Keys, indexes, checks and triggers of `rel`.
    RelationObjects {
        /// The relation to describe.
        rel: RelationRef,
    },
    /// Types and routines of `schema`.
    SchemaObjects {
        /// The schema to list.
        schema: String,
    },
    /// The current definition of `object`; never a tree level.
    Definition {
        /// The object to show.
        object: ObjectRef,
    },
    /// One page of the keys of Redis DB `db`.
    Keys {
        /// The DB index.
        db: u32,
        /// Which keys the scan matches.
        filter: KeyFilter,
        /// Where the scan resumes; None for its first page.
        after: Option<KeyToken>,
    },
}

/// A loaded catalog level, matching its [`CatalogQuery`].
#[derive(Debug, Clone, PartialEq)]
pub enum CatalogData {
    /// The answer to [`CatalogQuery::Schemas`].
    Schemas(Vec<SchemaInfo>),
    /// The answer to [`CatalogQuery::Relations`].
    Relations(RelationList),
    /// The answer to [`CatalogQuery::Columns`].
    Columns(Vec<ColumnMeta>),
    /// The answer to [`CatalogQuery::Count`].
    Count(u64),
    /// The answer to [`CatalogQuery::RelationObjects`].
    RelationObjects(RelationObjects),
    /// The answer to [`CatalogQuery::SchemaObjects`].
    SchemaObjects(SchemaObjects),
    /// The answer to [`CatalogQuery::Definition`].
    Definition(Definition),
    /// The answer to [`CatalogQuery::Keys`].
    Keys(KeyBatch),
}

/// One answer, in request order; `generation` is echoed from the request.
#[derive(Debug)]
pub struct CatalogReply {
    /// The generation the load was asked under.
    pub generation: u64,
    /// The query this answers.
    pub query: CatalogQuery,
    /// The data, or a message to show in an error row.
    pub result: Result<CatalogData, String>,
}

struct Load {
    generation: u64,
    query: CatalogQuery,
}

/// The view's end of a catalog runner. Dropping it ends the task, which closes its session.
pub struct CatalogRunner {
    commands: Sender<Load>,
    replies: Receiver<CatalogReply>,
    // Never sent on: closing it (or dropping it) is the stop signal.
    stop: Sender<()>,
}

impl CatalogRunner {
    /// Queues a load of `query`; its reply carries `generation`.
    pub fn load(&self, generation: u64, query: CatalogQuery) {
        let _ = self.commands.try_send(Load { generation, query });
    }

    /// The reply stream; it closes when the runner ends.
    pub fn replies(&self) -> Receiver<CatalogReply> {
        self.replies.clone()
    }

    /// Asks the runner to close its session and end, abandoning queued loads and cancelling
    /// the one in flight; the result resolves once the session has closed.
    pub fn close(self) -> CatalogClosing {
        self.stop.close();
        CatalogClosing(self.replies)
    }
}

/// A closing catalog runner.
pub struct CatalogClosing(Receiver<CatalogReply>);

impl CatalogClosing {
    /// Resolves once the runner's task has closed its session and ended.
    pub async fn wait(self) {
        // The task holds the only sender, so the channel closes exactly when it ends.
        while self.0.recv().await.is_ok() {}
    }
}

/// Starts a catalog runner on `handle`. It connects on its first load, not before.
pub fn spawn_catalog(handle: &tokio::runtime::Handle, spec: RunnerSpec) -> CatalogRunner {
    // Loads come from clicks, so neither channel can run away.
    let (commands, inbox) = async_channel::unbounded();
    let (outbox, replies) = async_channel::unbounded();
    let (stop, stopped) = async_channel::bounded(1);
    handle.spawn(serve(spec, inbox, stopped, outbox));
    CatalogRunner {
        commands,
        replies,
        stop,
    }
}

/// Read-only always and trusted, since the catalog runs only SQL Dbento wrote, with the
/// statement timeout capped at [`CATALOG_TIMEOUT`].
pub fn catalog_options(conn_timeout: Option<Duration>) -> SessionOptions {
    let mut options = SessionOptions::default();
    options.force_read_only = true;
    options.trusted_sql = true;
    options.statement_timeout =
        Some(conn_timeout.map_or(CATALOG_TIMEOUT, |t| t.min(CATALOG_TIMEOUT)));
    options
}

// Each wait races the stop signal, so a close never queues behind a slow catalog query:
// the load in flight is dropped and its statement cancelled before the session closes.
async fn serve(
    spec: RunnerSpec,
    inbox: Receiver<Load>,
    stop: Receiver<()>,
    replies: Sender<CatalogReply>,
) {
    let mut session: Option<(Session, Duration)> = None;
    let mut abandoned = false;
    loop {
        let (recv, stopped) = (pin!(inbox.recv()), pin!(stop.recv()));
        let next = select(recv, stopped).await;
        let Either::Left((Ok(Load { generation, query }), _)) = next else {
            break;
        };
        let finished = {
            let (work, stopped) = (pin!(load(&spec, &mut session, &query)), pin!(stop.recv()));
            match select(work, stopped).await {
                Either::Left((result, _)) => Some(result),
                Either::Right(_) => None,
            }
        };
        let Some(result) = finished else {
            abandoned = true;
            break;
        };
        let reply = CatalogReply {
            generation,
            query,
            result,
        };
        if replies.send(reply).await.is_err() {
            break;
        }
    }
    // Close, a dropped CatalogRunner, or a gone view: the session never drops unclosed.
    if let Some((s, _)) = session.take() {
        if abandoned {
            s.cancel_handle().cancel();
        }
        // Catalog reads leave no write for close to report.
        let _ = s.close().await;
    }
}

// Listings fail as IntrospectError, definitions as DefinitionError and key pages as
// KeyBrowseError; each says when the session behind it is gone.
enum Asked {
    Listing(Result<CatalogData, IntrospectError>),
    Definition(Result<Definition, DefinitionError>),
    Keys(Result<KeyPage, KeyBrowseError>),
}

// Connects on demand, so a failed connect is retried by the next load. A closed session,
// a panic, or a load that outlives its statement timeout plus [`CATALOG_GRACE`] drops the
// session, and the next load reconnects instead of queueing behind a dead request.
async fn load(
    spec: &RunnerSpec,
    session: &mut Option<(Session, Duration)>,
    query: &CatalogQuery,
) -> Result<CatalogData, String> {
    if session.is_none() {
        let mut limit = CATALOG_TIMEOUT;
        let s = runner::open(spec, |c| {
            let options = catalog_options(c.statement_timeout());
            limit = options.statement_timeout.unwrap_or(CATALOG_TIMEOUT);
            options
        })
        .await?;
        *session = Some((s, limit + CATALOG_GRACE));
    }
    let (s, deadline) = session.as_mut().expect("opened above");
    let work = async {
        match query {
            CatalogQuery::Definition { object } => Asked::Definition(s.definition(object).await),
            CatalogQuery::Keys { db, filter, after } => {
                Asked::Keys(s.scan_keys(&key_scan(*db, filter, after.as_ref())).await)
            }
            q => Asked::Listing(ask(s, q).await),
        }
    };
    let asked = AssertUnwindSafe(work).catch_unwind();
    let Ok(answer) = tokio::time::timeout(*deadline, asked).await else {
        let (s, deadline) = session.take().expect("opened above");
        abandon(s);
        return Err(format!(
            "the catalog load got no answer within {}s; the connection may be lost",
            deadline.as_secs()
        ));
    };
    let lost = matches!(
        answer,
        Err(_)
            | Ok(Asked::Listing(Err(IntrospectError::Execute(
                ExecuteError::SessionClosed | ExecuteError::SessionEnded { .. }
            ))))
            | Ok(Asked::Definition(Err(DefinitionError::Execute(
                ExecuteError::SessionClosed | ExecuteError::SessionEnded { .. }
            ))))
            // A Redis browse that timed out has already ended its session.
            | Ok(Asked::Keys(Err(
                KeyBrowseError::SessionEnded { .. } | KeyBrowseError::Timeout
            )))
    );
    if lost && let Some((s, _)) = session.take() {
        let _ = s.close().await;
    }
    match answer {
        Ok(Asked::Listing(r)) => r.map_err(|e| e.to_string()),
        Ok(Asked::Definition(r)) => r.map(CatalogData::Definition).map_err(definition_message),
        Ok(Asked::Keys(r)) => r.map(key_batch).map_err(|e| e.to_string()),
        Err(_) => Err("the catalog session stopped".into()),
    }
}

// One page of `filter`'s keys in `db`, resuming after `after` or from the start.
fn key_scan(db: u32, filter: &KeyFilter, after: Option<&KeyToken>) -> KeyScan {
    let mut scan = KeyScan::new(db);
    scan.pattern = filter.pattern.clone();
    scan.kind = filter.kind.as_deref().map(kind_of);
    scan.cursor = after.map_or_else(KeyCursor::start, |t| t.cursor().clone());
    scan.want = KEY_PAGE;
    scan
}

// A key that vanished between the scan and its TYPE read is left out of the tree.
fn key_batch(page: KeyPage) -> CatalogData {
    let items = (page.keys.into_iter())
        .filter(|k| k.ttl != KeyTtl::Gone)
        .map(|k| KeyItem {
            name: k.name,
            kind: k.kind,
            ttl: k.ttl,
        })
        .collect();
    let next = page.next.map(KeyToken::new);
    CatalogData::Keys(KeyBatch { items, next })
}

// A missing object points at the explorer's refresh; any other error reads as itself.
fn definition_message(e: DefinitionError) -> String {
    match e {
        DefinitionError::Missing { .. } => {
            format!("{e}; refresh the explorer to see the current objects")
        }
        e => e.to_string(),
    }
}

// Cancels the timed-out statement in case the server can still hear us, then closes the
// session off the runner's path; a close stuck on the dead socket is dropped at the cap.
fn abandon(s: Session) {
    s.cancel_handle().cancel();
    tokio::spawn(async move {
        let _ = tokio::time::timeout(ABANDON_CAP, s.close()).await;
    });
}

async fn ask(s: &mut Session, query: &CatalogQuery) -> Result<CatalogData, IntrospectError> {
    Ok(match query {
        CatalogQuery::Schemas => CatalogData::Schemas(s.schemas().await?),
        CatalogQuery::Relations { schema } => CatalogData::Relations(s.relations(schema).await?),
        CatalogQuery::Columns { rel } => CatalogData::Columns(s.columns(rel).await?),
        CatalogQuery::Count { rel } => CatalogData::Count(count(s, rel).await?),
        CatalogQuery::RelationObjects { rel } => {
            CatalogData::RelationObjects(s.relation_objects(rel).await?)
        }
        // An engine that keeps no schema objects lists none, so the explorer draws no group
        // for them rather than an error row.
        CatalogQuery::SchemaObjects { schema } => match s.schema_objects(schema).await {
            Err(IntrospectError::Unsupported { .. }) => {
                CatalogData::SchemaObjects(SchemaObjects::new(Vec::new(), Vec::new(), false))
            }
            r => CatalogData::SchemaObjects(r?),
        },
        // `load` sends definitions and key pages down their own arms, never here.
        CatalogQuery::Definition { .. } | CatalogQuery::Keys { .. } => {
            return Err(IntrospectError::Unexpected {
                message: "not a catalog listing".into(),
            });
        }
    })
}

// Runs `SELECT count(*)` through the session's own script path, so the read-only guard and
// statement timeout apply, and reads the single cell back as text.
async fn count(s: &mut Session, rel: &RelationRef) -> Result<u64, IntrospectError> {
    let engine = s.engine();
    let sql = format!(
        "SELECT count(*) FROM {}.{}",
        quote_ident(engine, rel.schema()),
        quote_ident(engine, rel.name())
    );
    let unexpected = |message: &str| IntrospectError::Unexpected {
        message: message.into(),
    };
    let mut script = s.script(&sql);
    let pending = script
        .next()
        .await
        .ok_or_else(|| unexpected("no statement"))??;
    let mut stream = pending.execute(Default::default()).await?;
    let mut n = None;
    while let Some(batch) = stream.next_batch().await? {
        if n.is_none() && !batch.is_empty() && batch.column_count() > 0 {
            n = match batch.cell(0, 0) {
                CellRef::Text(t) => t.parse().ok(),
                _ => None,
            };
        }
    }
    n.ok_or_else(|| unexpected("count(*) gave no number"))
}
