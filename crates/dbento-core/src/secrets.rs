//! Secret values that refuse to print themselves.

use crate::ssh::ResolvedSsh;
use crate::workspace::{ConnectionConfig, Engine, SecretSource, Workspace, is_env_secret_name};
use std::collections::HashMap;
use std::fmt;
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

pub use dbento_workspace::SecretKey;

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
    /// `secret = { env }` names a variable outside `DBENTO_SECRET_*`; the name is not kept.
    #[error(
        "connection {connection:?}: secret = {{ env }} only reads variables named DBENTO_SECRET_*; export the password as {suggested} and write secret = {{ env = \"{suggested}\" }}"
    )]
    EnvNameRefused {
        /// The connection whose secret was refused.
        connection: String,
        /// The variable to use instead, named after the secret's owner.
        suggested: String,
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

    fn timed<T: Send + 'static>(
        &self,
        call: impl FnOnce(&S) -> Result<T, SecretError> + Send + 'static,
    ) -> Result<T, SecretError> {
        timed_call(&self.inner, self.timeout, &self.stuck, call)
    }
}

/// Runs `call` on its own thread so a blocked keychain cannot block the caller. The first
/// timeout sets `stuck`, after which calls fail at once without spawning a thread.
pub(crate) fn timed_call<S: ?Sized + Send + Sync + 'static, T: Send + 'static>(
    inner: &Arc<S>,
    timeout: Duration,
    stuck: &AtomicBool,
    call: impl FnOnce(&S) -> Result<T, SecretError> + Send + 'static,
) -> Result<T, SecretError> {
    let timed_out = || SecretError::Unavailable {
        message: format!(
            "the keychain did not answer within {}",
            crate::workspace::format_duration(timeout)
        ),
    };
    if stuck.load(Ordering::Relaxed) {
        return Err(timed_out());
    }
    let (tx, rx) = mpsc::channel();
    let inner = Arc::clone(inner);
    std::thread::spawn(move || {
        let _ = tx.send(call(&inner));
    });
    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            stuck.store(true, Ordering::Relaxed);
            Err(timed_out())
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(SecretError::Failed {
            message: "the keychain worker stopped".to_owned(),
        }),
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

/// The environment fallback for an SSH password: `DBENTO_SSH_PASSWORD_<NAME>`, uppercased, `-`
/// as `_`.
pub fn ssh_password_env_var(connection: &str) -> String {
    format!(
        "DBENTO_SSH_PASSWORD_{}",
        connection.to_ascii_uppercase().replace('-', "_")
    )
}

/// The environment fallback for an SSH `identity_file` passphrase:
/// `DBENTO_SSH_KEY_PASSPHRASE_<NAME>`, uppercased, `-` as `_`.
pub fn ssh_key_passphrase_env_var(connection: &str) -> String {
    format!(
        "DBENTO_SSH_KEY_PASSPHRASE_{}",
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
        // A synced file must not choose an arbitrary variable to send as a password, so a name
        // outside the reserved prefix fails before the environment is read.
        SecretSource::Env(variable) if !is_env_secret_name(variable) => {
            Err(SecretError::EnvNameRefused {
                connection: conn.name().to_owned(),
                suggested: secret_env_var(conn.secret_owner()),
            })
        }
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

/// Who reads a missing-secret hint, which decides how the hint says to supply the password.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintAudience {
    /// A terminal user, pointed at `dbento secret` commands.
    Cli,
    /// A desktop app user, pointed at the connection form.
    App,
}

/// [`secret_hint_for`] worded for the CLI.
pub fn secret_hint(
    ws: &Workspace,
    conn: &ConnectionConfig,
    store: &dyn SecretStore,
) -> Option<String> {
    secret_hint_for(ws, conn, store, HintAudience::Cli)
}

/// For a keychain connection with a password, where a missing secret can be supplied, worded
/// for `audience`, plus the keychain's reason when it is unavailable; `None` for
/// `secret = "none"`, `{ env }` and SQLite. A password saved before pinning changes the text.
pub fn secret_hint_for(
    ws: &Workspace,
    conn: &ConnectionConfig,
    store: &dyn SecretStore,
    audience: HintAudience,
) -> Option<String> {
    if !conn.engine().capabilities().password
        || !matches!(conn.secret_source(), SecretSource::Keychain)
    {
        return None;
    }
    let name = conn.secret_owner();
    let variable = secret_env_var(name);
    // An unavailable keychain cannot answer for the legacy entry either, so only a readable
    // one is asked whether an unpinned password is waiting for confirmation.
    let unavailable = match store.get(&SecretKey::for_connection(ws, conn)) {
        Err(e @ SecretError::Unavailable { .. }) => Some(e),
        Ok(_)
        | Err(
            SecretError::Missing { .. }
            | SecretError::Failed { .. }
            | SecretError::EnvNameRefused { .. },
        ) => None,
    };
    let legacy = unavailable.is_none()
        && matches!(
            store.get(&SecretKey::legacy_password(ws.dir(), name)),
            Ok(Some(_))
        );
    let mut hint = match (audience, legacy) {
        (HintAudience::Cli, false) => format!(
            "no secret was found for '{name}': run `dbento secret set {name}` or set {variable}"
        ),
        (HintAudience::Cli, true) => format!(
            "the saved password for '{name}' was stored before passwords were tied to their \
             server; run `dbento secret pin {name}` to confirm this server, or \
             `dbento secret set {name}`"
        ),
        (HintAudience::App, false) => format!(
            "no password is saved for '{name}': edit the connection to enter it, or set {variable}"
        ),
        (HintAudience::App, true) => format!(
            "the saved password for '{name}' is not sent until you confirm this server: edit \
             the connection to enter it"
        ),
    };
    if let Some(e) = unavailable {
        hint.push_str(&format!("; {e}"));
    }
    Some(hint)
}

/// The passphrase for a connection's encrypted `tls_key`: keychain first, then
/// `DBENTO_KEY_PASSPHRASE_<OWNER>`.
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

/// The SSH secrets a tunnelled connection may need; either is `None` when nothing supplies it.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct SshSecrets {
    /// The SSH password, read from the entry pinned to the resolved `user@host:port`.
    pub password: Option<Secret>,
    /// The passphrase of the SSH `identity_file`.
    pub key_passphrase: Option<Secret>,
}

/// The SSH password and key passphrase for a connection with an `ssh` table: each from the
/// keychain first, then `DBENTO_SSH_PASSWORD_<OWNER>` or `DBENTO_SSH_KEY_PASSPHRASE_<OWNER>`.
///
/// The password's entry is pinned to `resolved.login()`, so a repointed host misses it. A
/// connection without `ssh` resolves to nothing without touching the store. Only an absent or
/// `Unavailable` keychain falls through to the environment; a failing one is an error.
pub fn resolve_ssh_secrets(
    ws: &Workspace,
    conn: &ConnectionConfig,
    resolved: &ResolvedSsh,
    store: &dyn SecretStore,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<SshSecrets, SecretError> {
    if conn.ssh().is_none() {
        return Ok(SshSecrets::default());
    }
    let owner = conn.secret_owner();
    let slot = |key: SecretKey, variable: String| match store.get(&key) {
        Ok(Some(secret)) => Ok(Some(secret)),
        Ok(None) | Err(SecretError::Unavailable { .. }) => Ok(env(&variable).map(Secret::new)),
        Err(e) => Err(e),
    };
    Ok(SshSecrets {
        password: slot(
            SecretKey::ssh_password_for(ws, conn, &resolved.login()),
            ssh_password_env_var(owner),
        )?,
        key_passphrase: slot(
            SecretKey::ssh_key_passphrase_for(ws, conn),
            ssh_key_passphrase_env_var(owner),
        )?,
    })
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
