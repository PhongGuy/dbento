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
    /// The SSH password, pinned to the `user@host:port` it logs in to.
    SshPassword,
    /// The passphrase of the SSH `identity_file`.
    SshKeyPassphrase,
}

/// Identifies one connection's secret: the canonical workspace directory, the connection name,
/// and which of its secrets: the password, the client key's passphrase, or an SSH one.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SecretKey {
    workspace_dir: PathBuf,
    connection: String,
    slot: Slot,
    /// The `user@host:port` an SSH password belongs to, so a repointed host names another entry.
    ssh_login: Option<String>,
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

    /// As `secret_target`, for the SSH password that logs in as `login` (`user@host:port`).
    pub fn ssh_password_target(&self, name: &str, login: &str) -> Option<SecretKey> {
        self.declared(name)
            .map(|c| SecretKey::ssh_password_for(self, c, login))
    }

    /// As `secret_target`, for the SSH `identity_file` passphrase.
    pub fn ssh_key_passphrase_target(&self, name: &str) -> Option<SecretKey> {
        self.declared(name)
            .map(|c| SecretKey::ssh_key_passphrase_for(self, c))
    }
}

impl SecretKey {
    /// The password key for `connection` in the workspace at `workspace_dir`.
    pub fn new(workspace_dir: impl Into<PathBuf>, connection: impl Into<String>) -> Self {
        SecretKey {
            workspace_dir: workspace_dir.into(),
            connection: connection.into(),
            slot: Slot::Password,
            ssh_login: None,
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

    /// The SSH password key for `connection`; `login` is the resolved `user@host:port`, so a
    /// repointed host names another entry.
    pub fn ssh_password(
        workspace_dir: impl Into<PathBuf>,
        connection: impl Into<String>,
        login: impl Into<String>,
    ) -> Self {
        SecretKey {
            slot: Slot::SshPassword,
            ssh_login: Some(login.into()),
            ..SecretKey::new(workspace_dir, connection)
        }
    }

    /// The SSH password key for a connection of an opened workspace, logging in as `login`.
    pub fn ssh_password_for(ws: &Workspace, conn: &ConnectionConfig, login: &str) -> Self {
        SecretKey::ssh_password(ws.dir(), conn.secret_owner(), login)
    }

    /// The key of the SSH `identity_file` passphrase for `connection`.
    pub fn ssh_key_passphrase(
        workspace_dir: impl Into<PathBuf>,
        connection: impl Into<String>,
    ) -> Self {
        SecretKey {
            slot: Slot::SshKeyPassphrase,
            ..SecretKey::new(workspace_dir, connection)
        }
    }

    /// The key of the SSH `identity_file` passphrase for a connection of an opened workspace.
    pub fn ssh_key_passphrase_for(ws: &Workspace, conn: &ConnectionConfig) -> Self {
        SecretKey::ssh_key_passphrase(ws.dir(), conn.secret_owner())
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

    /// The `user@host:port` an SSH password key is pinned to; only the keychain store reads it.
    #[doc(hidden)]
    pub fn ssh_login(&self) -> Option<&str> {
        self.ssh_login.as_deref()
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
