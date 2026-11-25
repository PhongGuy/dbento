//! SSH tunnels: how a connection's `ssh` table is resolved before a tunnel opens, and
//! which host keys are trusted.

use crate::{ConnectionConfig, SecretError, SecretStore, SshSecrets, Workspace};

mod auth;
mod known_hosts;
mod link;
mod resolve;
pub use known_hosts::{
    ForgetHostKeyError, HostKey, HostKeyProblem, HostKeyStatus, TrustHostKeyError, forget_host_key,
    trust_host_key, user_known_hosts_lists,
};
pub(crate) use link::{Dialer, SshLink, SshStream, resolved, tunnel_target};
pub use link::{SshEnv, probe_host_key, ssh_channel_check};
pub use resolve::{ResolvedSsh, SshHome, SshResolveError, resolve_ssh, ssh_warnings};

/// The SSH secrets of `conn`: none without an `ssh` table or when it does not resolve
/// against `env`'s home (connect then reports why); else [`crate::resolve_ssh_secrets`].
pub fn ssh_secrets_for(
    ws: &Workspace,
    conn: &ConnectionConfig,
    env: &SshEnv,
    store: &dyn SecretStore,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<SshSecrets, SecretError> {
    if conn.ssh().is_none() {
        return Ok(SshSecrets::default());
    }
    match resolve_ssh(conn, ws.dir(), env.home()) {
        Ok(resolved) => crate::resolve_ssh_secrets(ws, conn, &resolved, store, lookup),
        Err(_) => Ok(SshSecrets::default()),
    }
}
