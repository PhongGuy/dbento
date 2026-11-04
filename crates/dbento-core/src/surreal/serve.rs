//! The session's serving task: it owns the scripts and runs them over the socket task, so what
//! a script needs happens whether or not the caller is polling. The driver talks to it only by
//! channel. For now it answers settle and close and refuses every statement.

use tokio::sync::{mpsc, oneshot};

use super::rpc::Rpc;
use crate::session::{ExecuteError, Limits, Outcome, Prepared};
use crate::value::{ColumnInfo, ResultBatch};

pub(crate) enum Request {
    Script {
        text: String,
        ctl: mpsc::UnboundedReceiver<Ctl>,
    },
    Settle {
        done: oneshot::Sender<Result<(), ExecuteError>>,
    },
    Close {
        done: oneshot::Sender<Result<(), ExecuteError>>,
    },
}

type NextReply = oneshot::Sender<Option<Result<Prepared, ExecuteError>>>;
type ColumnsReply = oneshot::Sender<Result<Vec<ColumnInfo>, ExecuteError>>;

pub(crate) enum Ctl {
    Next {
        reply: NextReply,
    },
    IsLast {
        reply: oneshot::Sender<bool>,
    },
    Execute {
        reply: ColumnsReply,
        events: mpsc::Sender<Event>,
        limits: Limits,
        resume: Option<mpsc::UnboundedReceiver<Option<u64>>>,
    },
}

pub(crate) enum Event {
    Batch(ResultBatch),
    Done(Outcome),
    Parked(Outcome),
    Failed(ExecuteError),
}

fn refused() -> ExecuteError {
    ExecuteError::Unsupported {
        reason: "SurrealDB statements are not available yet".to_owned(),
    }
}

/// Serves `requests` until the session drops them, then ends the socket. A `CancelHandle` keeps
/// its own `Rpc` clone, so dropping this task's copy alone would leave the socket open.
pub(crate) async fn serve(rpc: Rpc, mut requests: mpsc::UnboundedReceiver<Request>) {
    while let Some(request) = requests.recv().await {
        match request {
            Request::Script { ctl, .. } => script(ctl).await,
            Request::Settle { done } => drop(done.send(Ok(()))),
            Request::Close { done } => {
                rpc.close("the session was closed");
                let _ = done.send(Ok(()));
                return;
            }
        }
    }
    rpc.close("the session was dropped");
}

// A failed prepare ends a script, so the first statement's refusal is the script's last word.
async fn script(mut ctl: mpsc::UnboundedReceiver<Ctl>) {
    let mut refused_once = false;
    while let Some(message) = ctl.recv().await {
        match message {
            Ctl::Next { reply } => {
                let answer = (!refused_once).then(|| Err(refused()));
                refused_once = true;
                let _ = reply.send(answer);
            }
            Ctl::IsLast { reply } => drop(reply.send(true)),
            Ctl::Execute { reply, .. } => drop(reply.send(Err(refused()))),
        }
    }
}
