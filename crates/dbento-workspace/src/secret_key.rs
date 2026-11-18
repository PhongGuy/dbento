//! The key that names one connection's secret in a secret store.

use crate::workspace::{ConnectionConfig, Workspace};
use std::path::{Path, PathBuf};

/// Which of a connection's secrets a key names.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    /// The database password.
    Password,
    /// The passphrase of the `tls_key` file.
    KeyPassphrase,
}

/// Identifies one connection's secret: the canonical workspace directory, the connection name,
/// and whether it is the password or the client key's passphrase.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SecretKey {
    workspace_dir: PathBuf,
    connection: String,
    slot: Slot,
}

// Keychain writes take their key only from these targets, so a name that is not written in
// dbento.toml (a derived child, a typo) can never create or remove a keychain entry.
impl Workspace {
    /// The keychain entry of declared connection `name`'s password; None for any other name.
    pub fn secret_target(&self, name: &str) -> Option<SecretKey> {
        self.declared(name)
            .map(|c| SecretKey::for_connection(self, c))
    }

    /// As `secret_target`, for the `tls_key` passphrase.
    pub fn key_passphrase_target(&self, name: &str) -> Option<SecretKey> {
        self.declared(name)
            .map(|c| SecretKey::key_passphrase_for(self, c))
    }
}

impl SecretKey {
    /// The password key for `connection` in the workspace at `workspace_dir`.
    pub fn new(workspace_dir: impl Into<PathBuf>, connection: impl Into<String>) -> Self {
        SecretKey {
            workspace_dir: workspace_dir.into(),
            connection: connection.into(),
            slot: Slot::Password,
        }
    }

    /// The password key for a connection of an opened workspace.
    pub fn for_connection(ws: &Workspace, conn: &ConnectionConfig) -> Self {
        SecretKey::new(ws.dir(), conn.secret_owner())
    }

    /// The key of the `tls_key` passphrase for `connection` in the workspace at `workspace_dir`.
    pub fn key_passphrase(
        workspace_dir: impl Into<PathBuf>,
        connection: impl Into<String>,
    ) -> Self {
        SecretKey {
            slot: Slot::KeyPassphrase,
            ..SecretKey::new(workspace_dir, connection)
        }
    }

    /// The key of the `tls_key` passphrase for a connection of an opened workspace.
    pub fn key_passphrase_for(ws: &Workspace, conn: &ConnectionConfig) -> Self {
        SecretKey::key_passphrase(ws.dir(), conn.secret_owner())
    }

    /// The same secret for the connection renamed to `connection`.
    pub fn renamed(&self, connection: impl Into<String>) -> Self {
        SecretKey {
            connection: connection.into(),
            ..self.clone()
        }
    }

    /// Which keychain entry of the connection this names; only the keychain store reads it.
    #[doc(hidden)]
    pub fn slot(&self) -> Slot {
        self.slot
    }

    /// The workspace directory.
    pub fn workspace_dir(&self) -> &Path {
        &self.workspace_dir
    }

    /// The connection name.
    pub fn connection(&self) -> &str {
        &self.connection
    }
}
