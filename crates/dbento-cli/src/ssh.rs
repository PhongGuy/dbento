//! `dbento ssh trust|forget`: Dbento's own record of SSH host keys, and the environment every
//! command reads SSH from.

use anyhow::Context;
use dbento_core::{
    ConnectError, ConnectionConfig, HostKeyProblem, HostKeyStatus, ResolvedSsh, SshEnv, StatePaths,
    Workspace, forget_host_key, probe_host_key, resolve_ssh, trust_host_key,
    user_known_hosts_lists,
};

use crate::Exit;
use crate::confirm::{confirm, stdin_is_terminal};
use crate::interrupt::Interrupt;

/// Where SSH reads from. Debug builds with `DBENTO_TEST_SSH_HOME` set use that home and never
/// an agent from `SSH_AUTH_SOCK`, so a test cannot reach the user's keys.
pub(crate) fn ssh_env() -> Option<SshEnv> {
    let paths = StatePaths::from_env()?;
    if cfg!(debug_assertions) && std::env::var_os("DBENTO_TEST_SSH_HOME").is_some() {
        return Some(SshEnv::new(
            crate::secret::ssh_home()?,
            paths.ssh_known_hosts,
        ));
    }
    SshEnv::from_env(&paths)
}

fn state_paths() -> anyhow::Result<StatePaths> {
    StatePaths::from_env().context("cannot find a state directory; set DBENTO_STATE_DIR")
}

fn require_env() -> anyhow::Result<SshEnv> {
    ssh_env().context("cannot find the home directory or user name for ~/.ssh; set $HOME and $USER")
}

/// The connection `name` and its SSH settings merged with `~/.ssh/config`.
fn resolved(
    ws: &Workspace,
    name: &str,
    env: &SshEnv,
) -> anyhow::Result<(ConnectionConfig, ResolvedSsh)> {
    let conn = ws.resolve(name).map_err(|e| anyhow::anyhow!("{e}"))?;
    if conn.ssh().is_none() {
        anyhow::bail!("connection {name:?} has no ssh table");
    }
    let ssh = resolve_ssh(&conn, ws.dir(), env.home())?;
    Ok((conn.into_owned(), ssh))
}

/// The name known_hosts files list `host`:`port` under.
fn known_hosts_name(host: &str, port: u16) -> String {
    let host = host.to_ascii_lowercase();
    if port == 22 {
        host
    } else {
        format!("[{host}]:{port}")
    }
}

/// Probes the SSH server's key and stores it in Dbento's known_hosts once it is checked: by a
/// matching `--fingerprint`, or by a yes at the terminal. A refused key is never stored.
pub(crate) async fn trust(
    ws: &Workspace,
    name: &str,
    fingerprint: Option<&str>,
) -> anyhow::Result<Exit> {
    let env = require_env()?;
    let (conn, ssh) = resolved(ws, name, &env)?;
    let paths = state_paths()?;
    let interrupt =
        Interrupt::install().map_err(|e| anyhow::Error::new(e).context("cannot handle Ctrl-C"))?;
    let status = tokio::select! {
        s = probe_host_key(ws, &conn, &env) => s?,
        () = interrupt.wait() => {
            eprintln!("interrupted");
            return Ok(Exit::Failed);
        }
    };
    let key = match status {
        HostKeyStatus::Trusted => {
            eprintln!("{}:{} already trusted", ssh.host_name, ssh.port);
            return Ok(Exit::Ok);
        }
        HostKeyStatus::Unknown(key) => key,
        HostKeyStatus::Changed {
            host,
            port,
            fingerprint,
            file,
            line,
            problem,
        } => {
            let e = ConnectError::SshHostKeyChanged {
                host,
                port,
                fingerprint,
                file,
                line,
                problem,
            };
            let hint = refusal_hint(name, &paths, &e);
            return Err(crate::secret::with_hint(e, hint));
        }
        _ => anyhow::bail!(
            "the SSH host key of {}:{} cannot be judged",
            ssh.host_name,
            ssh.port
        ),
    };
    let presented = key.fingerprint();
    eprintln!(
        "{}:{} {} {presented}",
        key.host(),
        key.port(),
        key.algorithm()
    );
    // A given fingerprint decides alone; without one only a person at a terminal can.
    match fingerprint {
        Some(given) if given != presented => {
            anyhow::bail!("the fingerprint {given} does not match the server's {presented}")
        }
        Some(_) => {}
        None if stdin_is_terminal() => {
            if !confirm(&format!("trust this key for '{name}'?"), false, &interrupt).await {
                return Ok(Exit::Refused);
            }
        }
        None => anyhow::bail!(
            "no terminal to confirm the key; check {presented} with the server's admin, then pass --fingerprint {presented}"
        ),
    }
    trust_host_key(&paths, &key)?;
    eprintln!(
        "trusted {presented} for {}",
        known_hosts_name(key.host(), key.port())
    );
    Ok(Exit::Ok)
}

/// The next step for a refused key, by the rule and the file that refused it. A revoked key
/// gets no forget advice, since forgetting must never be the way past a revocation.
fn refusal_hint(name: &str, paths: &StatePaths, refusal: &ConnectError) -> Option<String> {
    let ConnectError::SshHostKeyChanged {
        host,
        port,
        fingerprint,
        file,
        line,
        problem,
    } = refusal
    else {
        return None;
    };
    let retrust = format!("`dbento ssh trust {name}`");
    let rotated = format!("if the key was rotated, check {fingerprint} with the server's admin");
    match problem {
        HostKeyProblem::Changed if *file == paths.ssh_known_hosts => Some(format!(
            "{rotated}, run `dbento ssh forget {name}`, then {retrust}"
        )),
        HostKeyProblem::Changed => Some(format!(
            "{rotated}, run ssh-keygen -R '{}', then {retrust}",
            known_hosts_name(host, *port)
        )),
        HostKeyProblem::Revoked => Some(
            "a revoked key is never trusted; ask the server's admin why it was revoked".to_owned(),
        ),
        HostKeyProblem::Unreadable => Some(format!(
            "fix or delete line {line} of {}, then run {retrust}",
            file.display()
        )),
        _ => None,
    }
}

/// Removes the connection's host from Dbento's known_hosts, and says when the user's own
/// `~/.ssh/known_hosts` still lists it, since only that file's owner may edit it.
pub(crate) fn forget(ws: &Workspace, name: &str) -> anyhow::Result<Exit> {
    let env = require_env()?;
    let (_, ssh) = resolved(ws, name, &env)?;
    let paths = state_paths()?;
    let id = known_hosts_name(&ssh.host_name, ssh.port);
    match forget_host_key(&paths, &ssh.host_name, ssh.port)? {
        0 => eprintln!("no stored key for {id}"),
        n => eprintln!("removed {n} stored key(s) for {id}"),
    }
    if user_known_hosts_lists(env.home(), &ssh.host_name, ssh.port) {
        eprintln!("~/.ssh/known_hosts still lists {id}; remove it with ssh-keygen -R '{id}'");
    }
    Ok(Exit::Ok)
}
