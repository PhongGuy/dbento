//! The SSH context every connect carries: core's SSH environment, the state folder a trusted
//! host key is written through, and the app's one host-key gate.

use std::sync::Arc;

use dbento_core::{
    ConnectError, ConnectionConfig, HostKey, HostKeyProblem, Secret, SecretError, SecretKey,
    SecretStore, SessionOptions, SshEnv, SshHome, SshSecrets, StatePaths, Workspace, move_secret,
    resolve_ssh, ssh_secrets_for,
};

use crate::host_keys::{HostKeyAnswer, HostKeyGate, HostKeyPrompt};
use crate::link::ConnectFailure;

/// What a connect needs to reach a database through SSH. Without an env or state folder, SSH
/// connections are refused rather than handed to core, which would read the real home.
#[derive(Clone)]
pub struct SshContext {
    /// Core's SSH home, known_hosts and agent; None refuses SSH.
    pub env: Option<SshEnv>,
    /// What `trust_host_key` writes through; None refuses SSH.
    pub state: Option<StatePaths>,
    /// The app's one gate for unknown host keys.
    pub gate: Arc<HostKeyGate<HostKey>>,
}

impl SshContext {
    /// A context over `env` and `state` with a fresh gate that has no prompt handler yet.
    pub fn new(env: Option<SshEnv>, state: Option<StatePaths>) -> SshContext {
        SshContext {
            env,
            state,
            gate: HostKeyGate::new(),
        }
    }

    /// No env, no state: SSH connections are refused. Its gate has no handler, so nothing is
    /// asked.
    pub fn none() -> SshContext {
        SshContext::new(None, None)
    }

    /// The SSH home resolution reads, when there is an env.
    pub fn home(&self) -> Option<&SshHome> {
        self.env.as_ref().map(SshEnv::home)
    }

    /// Moves `secrets` and a clone of the env into `options`.
    pub fn apply(&self, options: &mut SessionOptions, secrets: SshSecrets) {
        options.ssh_secrets = secrets;
        options.ssh_env.clone_from(&self.env);
    }
}

/// The same words core's SshUnsupported uses for a missing home.
pub const NO_HOME: &str = "no home directory to read SSH settings from";

/// The fix for an SSH server that cannot be reached.
pub const CONNECT_HINT: &str =
    "Check that the SSH server is running and reachable at the SSH host and port.";

/// The fix for an SSH login that every method failed.
pub const AUTH_HINT: &str = "Store the SSH password or key passphrase from Edit connection, or check that your SSH agent holds the key.";

/// The fix for a tunnel the SSH server would not open to the database.
pub const TUNNEL_HINT: &str = "The SSH server could not reach the database. Check the database host as the SSH server sees it (localhost means the SSH server itself) and that the server allows TCP forwarding.";

/// The fix for an unknown host key nobody could be asked about; `{conn}` is the connection.
pub const UNKNOWN_KEY_HINT: &str = "Dbento has not seen this SSH server's host key. Run `dbento ssh trust {conn}` to check its fingerprint and trust it, then connect again.";

/// `conn`'s SSH secrets through core's `ssh_secrets_for`: a connection without an ssh table
/// never touches `store`; a tunnelled one is refused with NO_HOME when `ctx.env` is None.
pub fn ssh_secrets(
    ws: &Workspace,
    conn: &ConnectionConfig,
    ctx: &SshContext,
    store: &dyn SecretStore,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<SshSecrets, String> {
    if conn.ssh().is_none() {
        return Ok(SshSecrets::default());
    }
    let Some(env) = ctx.env.as_ref() else {
        return Err(NO_HOME.to_owned());
    };
    ssh_secrets_for(ws, conn, env, store, lookup).map_err(|e| e.to_string())
}

/// The prompt an SshHostKeyUnknown carries, for `with_trust`.
pub fn unknown_key(conn: &str, e: &ConnectError) -> Option<HostKeyPrompt<HostKey>> {
    let ConnectError::SshHostKeyUnknown { host, port, key } = e else {
        return None;
    };
    Some(HostKeyPrompt {
        connection: conn.to_owned(),
        host: host.clone(),
        port: *port,
        fingerprint: key.fingerprint(),
        key: key.clone(),
    })
}

/// The fix for an SSH failure, or None when `e` is not one or its message says it all. A
/// changed key points at the command that clears it from the file that holds it; a revoked or
/// unreadable one never does, since forgetting would hide the problem.
pub fn ssh_hint(conn: &str, e: &ConnectError, ctx: &SshContext) -> Option<String> {
    if let ConnectError::SshConnect { .. } = e {
        Some(CONNECT_HINT.to_owned())
    } else if let ConnectError::SshAuth { .. } = e {
        Some(AUTH_HINT.to_owned())
    } else if let ConnectError::SshTunnel { .. } = e {
        Some(TUNNEL_HINT.to_owned())
    } else if let ConnectError::SshHostKeyUnknown { .. } = e {
        Some(UNKNOWN_KEY_HINT.replace("{conn}", conn))
    } else if let ConnectError::SshHostKeyChanged {
        host,
        port,
        file,
        line,
        problem,
        ..
    } = e
    {
        changed_hint(conn, host, *port, file, *line, *problem, ctx)
    } else {
        None
    }
}

/// The fix for a refused known_hosts entry, by what is wrong with it.
fn changed_hint(
    conn: &str,
    host: &str,
    port: u16,
    file: &std::path::Path,
    line: usize,
    problem: HostKeyProblem,
    ctx: &SshContext,
) -> Option<String> {
    let path = file.display();
    if problem == HostKeyProblem::Changed {
        let ours = ctx.state.as_ref().map(|s| s.ssh_known_hosts.as_path()) == Some(file);
        // ssh-keygen names a non-default port in OpenSSH's bracketed form, quoted for the shell.
        let ident = if port == 22 {
            host.to_owned()
        } else {
            format!("'[{host}]:{port}'")
        };
        Some(if ours {
            format!(
                "The SSH server's host key changed. If it was rotated on purpose, run `dbento ssh forget {conn}`, then connect again to check the new fingerprint."
            )
        } else {
            format!(
                "The SSH server's host key changed in {path}. If it was rotated on purpose, remove the old entry with `ssh-keygen -R {ident} -f {path}`, then connect again."
            )
        })
    } else if problem == HostKeyProblem::Revoked {
        Some(format!(
            "The SSH server's host key is revoked in {path}. Do not connect until the server's administrator replaces it."
        ))
    } else if problem == HostKeyProblem::Unreadable {
        Some(format!(
            "Line {line} of {path} cannot be read. Fix or remove that line, then connect again."
        ))
    } else {
        None
    }
}

/// The failure a declined or conflicting question leaves (Trusted and Unasked never reach it;
/// they read as Declined).
pub fn not_trusted(conn: &str, host: &str, port: u16, answer: HostKeyAnswer) -> ConnectFailure {
    match answer {
        HostKeyAnswer::Conflicting => ConnectFailure::new(
            format!("the SSH server {host}:{port} showed two different host keys while Dbento was asking, so nothing was trusted"),
            Some("This can mean someone is intercepting the connection. Check with the server's administrator before connecting again.".to_owned()),
        ),
        HostKeyAnswer::Declined | HostKeyAnswer::Trusted | HostKeyAnswer::Unasked => {
            ConnectFailure::new(
                format!("the SSH host key of {host}:{port} was not trusted"),
                Some(format!(
                    "Connect again to check the fingerprint, or run `dbento ssh trust {conn}`."
                )),
            )
        }
    }
}

/// `e`'s text, with `ssh_hint`'s fix as a `\nhint: ` line when there is one; for the paths that
/// show a message without a separate fix (Test connection, a save's banner).
pub fn connect_message(conn: &str, e: &ConnectError, ctx: &SshContext) -> String {
    match ssh_hint(conn, e, ctx) {
        Some(h) => format!("{e}\nhint: {h}"),
        None => e.to_string(),
    }
}

/// Whether `e` is an SSH failure (every Ssh* variant, SshUnsupported included).
pub fn is_ssh_failure(e: &ConnectError) -> bool {
    matches!(
        e,
        ConnectError::SshConnect { .. }
            | ConnectError::SshAuth { .. }
            | ConnectError::SshUnsupported { .. }
            | ConnectError::SshHostKeyUnknown { .. }
            | ConnectError::SshHostKeyChanged { .. }
            | ConnectError::SshTunnel { .. }
    )
}

/// A connect failure as one message: an SSH failure is `connect_message` (its fix, never the
/// database-secret `db_hint`); any other keeps `db_hint` as a `\nhint: ` line.
pub fn failure_text(
    conn: &str,
    e: &ConnectError,
    db_hint: Option<&str>,
    ctx: &SshContext,
) -> String {
    match db_hint {
        Some(h) if !is_ssh_failure(e) => format!("{e}\nhint: {h}"),
        Some(_) | None => connect_message(conn, e, ctx),
    }
}

/// `not_trusted`'s message and fix as one text: "{message}\nhint: {fix}".
pub fn not_trusted_text(conn: &str, host: &str, port: u16, answer: HostKeyAnswer) -> String {
    let f = not_trusted(conn, host, port, answer);
    match f.fix {
        Some(fix) => format!("{}\nhint: {fix}", f.message),
        None => f.message,
    }
}

/// What a written connection edit does to the SSH keychain entries, in order: moves, drops,
/// then the typed secrets land at `password_to` / `passphrase_to`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SshFollow {
    /// Entries carried to the saved connection: the password pair first, then the passphrase.
    pub moves: Vec<(SecretKey, SecretKey)>,
    /// Entries nothing reads any more: the password first, then the passphrase.
    pub drops: Vec<SecretKey>,
    /// The saved connection's pinned password entry.
    pub password_to: Option<SecretKey>,
    /// The saved connection's passphrase entry.
    pub passphrase_to: Option<SecretKey>,
    /// Why the saved connection has no password entry.
    pub problem: Option<String>,
}

/// `old_name` in `base` became `name` in `now` (equal names for an edit, absent in `base` for a
/// new one). Same SSH login on a rename moves; a gone table drops both; another login drops
/// the old password; an unresolvable `now` keeps the old entry and says why. Reads
/// `~/.ssh/config` through `resolve_ssh`, so callers run it off the UI thread.
pub fn ssh_follow(
    base: &Workspace,
    old_name: &str,
    now: &Workspace,
    name: &str,
    home: Option<&SshHome>,
) -> SshFollow {
    let mut follow = SshFollow::default();
    let was = base.declared(old_name).filter(|c| c.ssh().is_some());
    let is = now.declared(name).filter(|c| c.ssh().is_some());
    let login = |ws: &Workspace, conn: &ConnectionConfig| -> Result<String, String> {
        let home = home.ok_or_else(|| NO_HOME.to_owned())?;
        resolve_ssh(conn, ws.dir(), home)
            .map(|r| r.login())
            .map_err(|e| {
                format!(
                    "the SSH settings of {} cannot be resolved: {e}",
                    conn.name()
                )
            })
    };
    // The old password entry is only reachable through the login it was pinned to.
    let old_password = was
        .and_then(|c| login(base, c).ok())
        .and_then(|l| base.ssh_password_target(old_name, &l));
    let old_passphrase = was.and_then(|_| base.ssh_key_passphrase_target(old_name));
    let Some(is) = is else {
        follow.drops.extend(old_password);
        follow.drops.extend(old_passphrase);
        return follow;
    };
    match login(now, is) {
        Ok(l) => follow.password_to = now.ssh_password_target(name, &l),
        Err(problem) => follow.problem = Some(problem),
    }
    follow.passphrase_to = now.ssh_key_passphrase_target(name);
    // Another login drops the old pinned password; the same one moves it on a rename. An
    // unresolvable new login leaves the old entry where it is.
    if let (Some(from), Some(to)) = (old_password, follow.password_to.clone()) {
        if from.ssh_login() != to.ssh_login() {
            follow.drops.push(from);
        } else if from != to {
            follow.moves.push((from, to));
        }
    }
    if let (Some(from), Some(to)) = (old_passphrase, follow.passphrase_to.clone())
        && from != to
    {
        follow.moves.push((from, to));
    }
    follow
}

/// Carries out `follow` on `store`: moves, drops, then stores the typed `password` and
/// `passphrase` at their entries (a typed secret without an entry is not stored).
pub fn apply_follow(
    store: &dyn SecretStore,
    follow: &SshFollow,
    password: Option<&Secret>,
    passphrase: Option<&Secret>,
) -> Result<(), SecretError> {
    for (from, to) in &follow.moves {
        move_secret(store, from, to)?;
    }
    for key in &follow.drops {
        store.delete(key)?;
    }
    for (key, secret) in [
        (&follow.password_to, password),
        (&follow.passphrase_to, passphrase),
    ] {
        if let (Some(key), Some(secret)) = (key, secret) {
            store.set(key, secret)?;
        }
    }
    Ok(())
}
