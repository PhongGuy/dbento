//! Secret values that refuse to print themselves.

use crate::workspace::{ConnectionConfig, Engine, SecretSource, Workspace};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use zeroize::Zeroizing;

/// A secret such as a database password. `Debug` and `Display` print `<redacted>`, and its
/// memory is wiped when it drops.
#[derive(Clone)]
pub struct Secret(Zeroizing<String>);

impl Secret {
    /// Wraps a secret value.
    pub fn new(value: impl Into<String>) -> Self {
        Secret(Zeroizing::new(value.into()))
    }

    /// The secret itself; the only way to read it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Which of a connection's secrets a key names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Slot {
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
    #[cfg(feature = "keychain")]
    pub(crate) fn slot(&self) -> Slot {
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

/// Why a secret could not be read or stored.
#[non_exhaustive]
#[derive(Debug, Clone, thiserror::Error)]
pub enum SecretError {
    /// The named environment variable is not set.
    #[error("the environment variable {variable} is not set")]
    Missing {
        /// The variable name.
        variable: String,
    },
    /// The keychain cannot be reached (absent, locked or not answering).
    #[error("the keychain is unavailable: {message}")]
    Unavailable {
        /// What went wrong.
        message: String,
    },
    /// The keychain answered with a failure.
    #[error("the keychain failed: {message}")]
    Failed {
        /// What went wrong.
        message: String,
    },
}

/// Somewhere secrets are kept, keyed by [`SecretKey`]. Deleting an absent entry succeeds.
pub trait SecretStore: Send + Sync {
    /// The stored secret, or `None` when there is no entry.
    fn get(&self, key: &SecretKey) -> Result<Option<Secret>, SecretError>;
    /// Stores or replaces the secret.
    fn set(&self, key: &SecretKey, secret: &Secret) -> Result<(), SecretError>;
    /// Removes the entry, if any.
    fn delete(&self, key: &SecretKey) -> Result<(), SecretError>;
}

/// A [`SecretStore`] in process memory, for tests and callers without a keychain.
#[derive(Debug, Default)]
pub struct MemoryStore {
    entries: Mutex<HashMap<SecretKey, String>>,
}

impl MemoryStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<SecretKey, String>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, key: &SecretKey) -> Result<Option<Secret>, SecretError> {
        Ok(self.entries().get(key).map(|v| Secret::new(v.clone())))
    }

    fn set(&self, key: &SecretKey, secret: &Secret) -> Result<(), SecretError> {
        self.entries()
            .insert(key.clone(), secret.expose().to_owned());
        Ok(())
    }

    fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        self.entries().remove(key);
        Ok(())
    }
}

/// Wraps a store so a call that never returns becomes `Unavailable` instead of a hang.
///
/// The first timeout marks the store stuck; later calls fail at once without spawning a thread.
pub struct TimedStore<S> {
    inner: Arc<S>,
    timeout: Duration,
    stuck: Arc<AtomicBool>,
}

impl<S: SecretStore + 'static> TimedStore<S> {
    /// Wraps `inner` with a 10 second timeout.
    pub fn new(inner: S) -> Self {
        Self::with_timeout(inner, Duration::from_secs(10))
    }

    /// Wraps `inner` with the given timeout.
    pub fn with_timeout(inner: S, timeout: Duration) -> Self {
        TimedStore {
            inner: Arc::new(inner),
            timeout,
            stuck: Arc::new(AtomicBool::new(false)),
        }
    }

    // Runs `call` on its own thread so a blocked keychain cannot block the caller.
    fn timed<T: Send + 'static>(
        &self,
        call: impl FnOnce(&S) -> Result<T, SecretError> + Send + 'static,
    ) -> Result<T, SecretError> {
        if self.stuck.load(Ordering::Relaxed) {
            return Err(self.timeout_error());
        }
        let (tx, rx) = mpsc::channel();
        let inner = Arc::clone(&self.inner);
        std::thread::spawn(move || {
            let _ = tx.send(call(&inner));
        });
        match rx.recv_timeout(self.timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.stuck.store(true, Ordering::Relaxed);
                Err(self.timeout_error())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(SecretError::Failed {
                message: "the keychain worker stopped".to_owned(),
            }),
        }
    }

    fn timeout_error(&self) -> SecretError {
        SecretError::Unavailable {
            message: format!(
                "the keychain did not answer within {}",
                humantime::format_duration(self.timeout)
            ),
        }
    }
}

impl<S: SecretStore + 'static> SecretStore for TimedStore<S> {
    fn get(&self, key: &SecretKey) -> Result<Option<Secret>, SecretError> {
        let key = key.clone();
        self.timed(move |s| s.get(&key))
    }

    fn set(&self, key: &SecretKey, secret: &Secret) -> Result<(), SecretError> {
        let (key, secret) = (key.clone(), secret.clone());
        self.timed(move |s| s.set(&key, &secret))
    }

    fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        let key = key.clone();
        self.timed(move |s| s.delete(&key))
    }
}

/// The environment fallback for a keychain connection: `DBENTO_SECRET_<NAME>`, uppercased,
/// `-` as `_`.
pub fn secret_env_var(connection: &str) -> String {
    format!(
        "DBENTO_SECRET_{}",
        connection.to_ascii_uppercase().replace('-', "_")
    )
}

/// The environment fallback for a `tls_key` passphrase: `DBENTO_KEY_PASSPHRASE_<NAME>`,
/// uppercased, `-` as `_`.
pub fn key_passphrase_env_var(connection: &str) -> String {
    format!(
        "DBENTO_KEY_PASSPHRASE_{}",
        connection.to_ascii_uppercase().replace('-', "_")
    )
}

/// Moves the stored secret; `Ok(false)` when `from` holds none. Deletes `from` only after
/// `to` is set, so a failure never loses the secret.
pub fn move_secret(
    store: &dyn SecretStore,
    from: &SecretKey,
    to: &SecretKey,
) -> Result<bool, SecretError> {
    // Deleting `from` after setting it to itself would lose the secret.
    if from == to {
        return Ok(store.get(from)?.is_some());
    }
    let Some(secret) = store.get(from)? else {
        return Ok(false);
    };
    store.set(to, &secret)?;
    store.delete(from)?;
    Ok(true)
}

/// The secret for a connection, keyed by its `secret_owner`: keychain first, then
/// `DBENTO_SECRET_<OWNER>`, or only the variable a connection names with `secret = { env = ... }`.
///
/// SQLite and `secret = "none"` connections resolve to `None` without touching the store. Only
/// an absent or `Unavailable` keychain falls through to the environment; a failing one is an error.
pub fn resolve_secret(
    ws: &Workspace,
    conn: &ConnectionConfig,
    store: &dyn SecretStore,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<Secret>, SecretError> {
    if conn.engine() == Engine::Sqlite {
        return Ok(None);
    }
    match conn.secret_source() {
        SecretSource::None => Ok(None),
        SecretSource::Env(variable) => {
            env(variable)
                .map(|v| Some(Secret::new(v)))
                .ok_or_else(|| SecretError::Missing {
                    variable: variable.clone(),
                })
        }
        SecretSource::Keychain => {
            match store.get(&SecretKey::for_connection(ws, conn)) {
                Ok(Some(secret)) => return Ok(Some(secret)),
                Ok(None) | Err(SecretError::Unavailable { .. }) => {}
                Err(e) => return Err(e),
            }
            Ok(env(&secret_env_var(conn.secret_owner())).map(Secret::new))
        }
    }
}

/// For a keychain connection with a password, where a missing secret can be supplied, plus the
/// keychain's reason when it is unavailable; `None` for `secret = "none"`, `{ env }` and SQLite.
pub fn secret_hint(
    ws: &Workspace,
    conn: &ConnectionConfig,
    store: &dyn SecretStore,
) -> Option<String> {
    if !conn.engine().capabilities().password
        || !matches!(conn.secret_source(), SecretSource::Keychain)
    {
        return None;
    }
    let name = conn.secret_owner();
    let variable = secret_env_var(name);
    let mut hint = format!(
        "no secret was found for '{name}': run `dbento secret set {name}` or set {variable}"
    );
    if let Err(e @ SecretError::Unavailable { .. }) =
        store.get(&SecretKey::for_connection(ws, conn))
    {
        hint.push_str(&format!("; {e}"));
    }
    Some(hint)
}

/// The passphrase for a connection's encrypted `tls_key`: keychain first, then
/// `DBENTO_KEY_PASSPHRASE_<NAME>`.
///
/// Neither is consulted unless the key file is an encrypted PKCS#8 PEM, so a plain key never
/// prompts a keychain; an unreadable key resolves to `None` and fails at connect, naming the
/// file. Only an absent or `Unavailable` keychain falls through to the environment.
pub fn resolve_key_passphrase(
    ws: &Workspace,
    conn: &ConnectionConfig,
    store: &dyn SecretStore,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<Secret>, SecretError> {
    if !tls_key_is_encrypted(conn) {
        return Ok(None);
    }
    match store.get(&SecretKey::key_passphrase_for(ws, conn)) {
        Ok(Some(secret)) => return Ok(Some(secret)),
        Ok(None) | Err(SecretError::Unavailable { .. }) => {}
        Err(e) => return Err(e),
    }
    Ok(env(&key_passphrase_env_var(conn.secret_owner())).map(Secret::new))
}

/// Whether the connection's `tls_key` file reads as an encrypted PKCS#8 PEM, so it needs a
/// passphrase; false without a key or when the file cannot be read.
pub fn tls_key_is_encrypted(conn: &ConnectionConfig) -> bool {
    conn.tls_key()
        .is_some_and(|path| std::fs::read(path).is_ok_and(|pem| is_encrypted_pem(&pem)))
}

/// Whether `pem` holds an encrypted PKCS#8 private key.
pub(crate) fn is_encrypted_pem(pem: &[u8]) -> bool {
    const LABEL: &[u8] = b"-----BEGIN ENCRYPTED PRIVATE KEY-----";
    pem.windows(LABEL.len()).any(|w| w == LABEL)
}
