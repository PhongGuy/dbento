//! A connection's link state, as the explorer and the status bar show it.

use dbento_core::{ConnectError, ConnectionConfig, SecretError};

/// Whether `conn` shows as read-only, given what its session reported once connected: the
/// config's rule alone before a session, and a session can raise it, as a VIEWER login does.
pub fn shows_read_only(conn: &ConnectionConfig, session: Option<bool>) -> bool {
    conn.read_only() || session == Some(true)
}

/// Whether a connection has a live session: one per catalog or tab, merged per connection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum LinkState {
    /// No session is open or opening.
    #[default]
    Idle,
    /// A session is opening.
    Connecting,
    /// A session is open.
    Connected,
    /// A session failed, with its error.
    Failed(String),
}

impl LinkState {
    /// The stronger of the two: Failed, then Connecting, then Connected, then Idle; of two
    /// failures the first.
    pub fn merge(self, other: LinkState) -> LinkState {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }

    /// "not connected", "connecting", "connected" or "failed".
    pub fn word(&self) -> &'static str {
        match self {
            LinkState::Idle => "not connected",
            LinkState::Connecting => "connecting",
            LinkState::Connected => "connected",
            LinkState::Failed(_) => "failed",
        }
    }

    fn rank(&self) -> u8 {
        match self {
            LinkState::Idle => 0,
            LinkState::Connected => 1,
            LinkState::Connecting => 2,
            LinkState::Failed(_) => 3,
        }
    }
}

/// A failed connect as core typed it: the secret lookup or the connect itself.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum ConnectCause<'a> {
    /// The secret could not be read.
    Secret(&'a SecretError),
    /// The engine could not open a session.
    Connect(&'a ConnectError),
}

/// One sentence on how to fix `cause`, when the failure says what is wrong: a missing
/// variable to set, or a server that refused the connection.
pub fn fix_hint(cause: ConnectCause<'_>) -> Option<String> {
    match cause {
        ConnectCause::Secret(SecretError::Missing { variable }) => Some(format!(
            "Set {variable} or store the secret in the keychain from Edit connection."
        )),
        ConnectCause::Connect(ConnectError::Refused { address, .. }) => {
            Some(format!("Check that the server is running at {address}."))
        }
        ConnectCause::Secret(_) | ConnectCause::Connect(_) => None,
    }
}

/// A session that could not open: the error as the tab shows it and, when known, its fix.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConnectFailure {
    /// The error text.
    pub message: String,
    /// What `fix_hint` says about it.
    pub fix: Option<String>,
}

impl ConnectFailure {
    /// A failure with `message` and an optional `fix`.
    pub fn new(message: String, fix: Option<String>) -> Self {
        ConnectFailure { message, fix }
    }
}
