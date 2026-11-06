//! The SurrealDB driver: its async half talks by channel to the serving task, which owns the
//! scripts, and through it to the socket task.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use super::cancel::SurrealCancel;
use super::catalog::{Answer, Ask};
use super::login::login;
use super::rpc::{CALL_BOUND, Rpc};
use super::serve::{Ctl, Event, PARK_BOUND, Request, Rules, serve};
use crate::introspect::{ColumnMeta, IntrospectError, RelationList, RelationRef, SchemaInfo};
use crate::objects::RelationObjects;
use crate::session::{
    CancelHandle, ConnectError, ConnectParams, DRAIN_CAP, Driver, ExecuteError, ExtendError,
    Flavor, Limits, OnTruncate, Prepared, ReadOnlyEnforcement, ServerInfo, StreamItem,
};
use crate::value::ColumnInfo;

/// TCP, TLS, the upgrade and every step of login share this bound.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct SurrealDriver {
    requests: mpsc::UnboundedSender<Request>,
    server: ServerInfo,
    cancel: Arc<SurrealCancel>,
    read_only: Option<ReadOnlyEnforcement>,
    warning: Option<String>,
    /// Set once a `USE` is sent on this session; see `Session::settings_changed`.
    settings_changed: Arc<AtomicBool>,
    /// The namespace and database the login put the session in.
    opened_on: (String, String),
}

pub(crate) struct SurrealScript {
    ctl: mpsc::UnboundedSender<Ctl>,
}

pub(crate) struct SurrealStream {
    events: mpsc::Receiver<Event>,
    resume: Option<mpsc::UnboundedSender<Option<u64>>>,
    parked: bool,
}

impl SurrealDriver {
    pub(crate) fn server(&self) -> ServerInfo {
        self.server
    }

    pub(crate) fn settings_changed(&self) -> bool {
        self.settings_changed.load(Ordering::SeqCst)
    }

    /// What the shell should say about the server: set for a prerelease or a higher major.
    pub(crate) fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    /// The indexes and events of `rel`; empty for an unknown database or table.
    pub(crate) async fn relation_objects(
        &mut self,
        rel: &RelationRef,
    ) -> Result<RelationObjects, IntrospectError> {
        match self.ask(Ask::Objects(rel.clone())).await? {
            Answer::Objects(objects) => Ok(objects),
            _ => Err(wrong_answer("relation objects")),
        }
    }

    /// `ns/db` once RPC `use` has moved the session off where it opened; None on failure.
    pub(crate) async fn moved_database(&self) -> Option<String> {
        match self.ask(Ask::Place).await {
            Ok(Answer::Place(ns, db)) if (&ns, &db) != (&self.opened_on.0, &self.opened_on.1) => {
                Some(format!("{ns}/{db}"))
            }
            _ => None,
        }
    }

    // Catalog calls queue in the serving task behind any script, in the session's own place.
    async fn ask(&self, ask: Ask) -> Result<Answer, IntrospectError> {
        let closed = || IntrospectError::Execute(ExecuteError::SessionClosed);
        let (reply, wait) = oneshot::channel();
        if self.requests.send(Request::Catalog { ask, reply }).is_err() {
            return Err(closed());
        }
        wait.await.unwrap_or_else(|_| Err(closed()))
    }
}

fn wrong_answer(what: &str) -> IntrospectError {
    IntrospectError::Unexpected {
        message: format!("the session answered a request for {what} with something else"),
    }
}

impl Driver for SurrealDriver {
    type Script = SurrealScript;
    type Stream = SurrealStream;

    async fn connect(params: ConnectParams<'_>) -> Result<Self, ConnectError> {
        let conn = params.conn;
        // A login that fails leaves a socket no session will own, so it is closed here.
        let opened = tokio::time::timeout(CONNECT_TIMEOUT, async {
            let call_bound = params.bounds.call.unwrap_or(CALL_BOUND);
            let rpc = Rpc::open(conn, params.key_passphrase, call_bound).await?;
            let logged = login(&rpc, conn, params.secret, params.read_only).await;
            if logged.is_err() {
                rpc.close("connect failed");
            }
            logged.map(|logged| (rpc, logged))
        })
        .await
        .map_err(|_| ConnectError::Open {
            message: format!(
                "{}: the server did not answer within {} s",
                conn.display_target(),
                CONNECT_TIMEOUT.as_secs()
            ),
        })?;
        let (rpc, logged) = opened?;
        let drain_bound = params.bounds.drain.unwrap_or(DRAIN_CAP);
        let cancel = Arc::new(SurrealCancel::new(rpc.clone(), drain_bound));
        let rules = Rules {
            drain_bound,
            timeout: params.timeout,
            read_only: logged.read_only,
            viewer: logged.viewer,
            park_bound: params.bounds.park.unwrap_or(PARK_BOUND),
        };
        let (requests, rx) = mpsc::unbounded_channel();
        let settings_changed = Arc::new(AtomicBool::new(false));
        let opened_on = (
            logged.place.namespace.clone(),
            logged.place.database.clone(),
        );
        tokio::spawn(serve(
            rpc,
            rules,
            Arc::clone(&cancel),
            logged.place,
            Arc::clone(&settings_changed),
            rx,
        ));
        Ok(SurrealDriver {
            requests,
            server: ServerInfo {
                flavor: Flavor::SurrealDb,
                version: logged.version,
            },
            cancel,
            read_only: logged.read_only,
            warning: logged.warning,
            settings_changed,
            opened_on,
        })
    }

    fn start_script(&mut self, sql: &str) -> SurrealScript {
        let (ctl, rx) = mpsc::unbounded_channel();
        let _ = self.requests.send(Request::Script {
            text: sql.to_owned(),
            ctl: rx,
        });
        SurrealScript { ctl }
    }

    async fn prepare_next(script: &mut SurrealScript) -> Option<Result<Prepared, ExecuteError>> {
        let (reply, wait) = oneshot::channel();
        if script.ctl.send(Ctl::Next { reply }).is_err() {
            return Some(Err(ExecuteError::SessionClosed));
        }
        wait.await.unwrap_or(Some(Err(ExecuteError::SessionClosed)))
    }

    async fn is_last(script: &mut SurrealScript) -> bool {
        let (reply, wait) = oneshot::channel();
        if script.ctl.send(Ctl::IsLast { reply }).is_err() {
            return true;
        }
        wait.await.unwrap_or(true)
    }

    async fn execute(
        script: &mut SurrealScript,
        limits: Limits,
    ) -> Result<(Vec<ColumnInfo>, SurrealStream), ExecuteError> {
        let (reply, wait) = oneshot::channel();
        let (events, rx) = mpsc::channel(1);
        let (resume, resume_rx) = match limits.on_truncate {
            OnTruncate::Park => {
                let (tx, rx) = mpsc::unbounded_channel();
                (Some(tx), Some(rx))
            }
            _ => (None, None),
        };
        let sent = script.ctl.send(Ctl::Execute {
            reply,
            events,
            limits,
            resume: resume_rx,
        });
        if sent.is_err() {
            return Err(ExecuteError::SessionClosed);
        }
        let columns = wait.await.map_err(|_| ExecuteError::SessionClosed)??;
        Ok((
            columns,
            SurrealStream {
                events: rx,
                resume,
                parked: false,
            },
        ))
    }

    async fn next_event(stream: &mut SurrealStream) -> Result<StreamItem, ExecuteError> {
        match stream.events.recv().await {
            Some(Event::Batch(b)) => Ok(StreamItem::Batch(b)),
            Some(Event::Done(o)) => Ok(StreamItem::Done(o)),
            Some(Event::Parked(o)) => {
                stream.parked = true;
                Ok(StreamItem::Done(o))
            }
            Some(Event::Failed(e)) => Err(e),
            None => Err(ExecuteError::SessionClosed),
        }
    }

    fn extend(stream: &mut SurrealStream, more: Option<u64>) -> Result<(), ExtendError> {
        if !std::mem::take(&mut stream.parked) {
            return Err(ExtendError::NotParked);
        }
        let resume = stream.resume.as_ref().ok_or(ExtendError::NotParked)?;
        // A result the task already gave up on reports it from the next `next_event`.
        let _ = resume.send(more);
        Ok(())
    }

    fn cancel_handle(&self) -> CancelHandle {
        CancelHandle::surreal(Arc::clone(&self.cancel))
    }

    async fn settle(&mut self) -> Result<(), ExecuteError> {
        let (done, wait) = oneshot::channel();
        if self.requests.send(Request::Settle { done }).is_err() {
            return Err(ExecuteError::SessionClosed);
        }
        wait.await.unwrap_or(Err(ExecuteError::SessionClosed))
    }

    async fn close(self) -> Result<(), ExecuteError> {
        let (done, wait) = oneshot::channel();
        if self.requests.send(Request::Close { done }).is_err() {
            return Ok(());
        }
        wait.await.unwrap_or(Ok(()))
    }

    async fn schemas(&mut self) -> Result<Vec<SchemaInfo>, IntrospectError> {
        match self.ask(Ask::Schemas).await? {
            Answer::Schemas(schemas) => Ok(schemas),
            _ => Err(wrong_answer("schemas")),
        }
    }

    async fn relations(&mut self, schema: &str) -> Result<RelationList, IntrospectError> {
        match self.ask(Ask::Relations(schema.to_owned())).await? {
            Answer::Relations(relations) => Ok(relations),
            _ => Err(wrong_answer("relations")),
        }
    }

    async fn columns(&mut self, rel: &RelationRef) -> Result<Vec<ColumnMeta>, IntrospectError> {
        match self.ask(Ask::Columns(rel.clone())).await? {
            Answer::Columns(columns) => Ok(columns),
            _ => Err(wrong_answer("columns")),
        }
    }

    // The level is the login's, fixed at connect, whatever this call asks.
    fn read_only_level(&self, _requested: bool) -> Option<ReadOnlyEnforcement> {
        self.read_only
    }
}
