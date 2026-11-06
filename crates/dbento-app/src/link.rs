//! A connection's link state, as the explorer and the status bar show it.

use dbento_core::ConnectionConfig;

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
