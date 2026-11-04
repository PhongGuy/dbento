//! What every tool call shares: the session it opened, cancellation and the history.

use std::path::{Path, PathBuf};
use std::time::Duration;

use dbento_core::{CancelHandle, ConnectionConfig, ExecuteError, History, HistoryEntry, Session};
use rmcp::RoleServer;
use rmcp::service::RequestContext;

use crate::error::ToolError;

/// The server's timeout or the connection's own, whichever is shorter.
pub(crate) fn effective_timeout(flag: Duration, own: Option<Duration>) -> Duration {
    own.map_or(flag, |own| own.min(flag))
}

/// Runs `work` until it ends or the client cancels the request; a cancel fires `cancel` and
/// answers `cancelled`.
pub(crate) async fn until_cancelled<T>(
    context: &RequestContext<RoleServer>,
    cancel: &CancelHandle,
    work: impl Future<Output = Result<T, ToolError>>,
) -> Result<T, ToolError> {
    // rmcp keeps the handler running after a client cancel, so the statement is stopped in
    // the database here; dropping the work alone would leave it running server side.
    tokio::select! {
        r = work => r,
        () = context.ct.cancelled() => {
            cancel.cancel();
            Err(ToolError::from_execute(&ExecuteError::Cancelled))
        }
    }
}

/// A session on one exposed connection, with where it came from.
pub(crate) struct Opened {
    /// The workspace folder the connection was read from.
    pub dir: PathBuf,
    /// The connection as configured when the call started.
    pub conn: ConnectionConfig,
    /// The open session; the caller closes it.
    pub session: Session,
}

/// Appends `entry` off the async runtime; a failure goes to stderr, never to the client.
pub(crate) async fn record(path: &Path, entry: HistoryEntry) {
    let path = path.to_path_buf();
    let written = tokio::task::spawn_blocking(move || History::open(&path)?.append(&entry))
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r.map_err(|e| e.to_string()));
    if let Err(e) = written {
        eprintln!("dbento mcp: cannot write the query history: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shorter_timeout_wins() {
        let s = Duration::from_secs;
        assert_eq!(effective_timeout(s(30), None), s(30));
        assert_eq!(
            effective_timeout(s(30), Some(Duration::from_millis(300))),
            Duration::from_millis(300)
        );
        assert_eq!(effective_timeout(s(1), Some(s(10))), s(1));
    }
}
