//! SSH tunnels: how a connection's `ssh` table is resolved before a tunnel opens, and
//! which host keys are trusted.

mod auth;
mod known_hosts;
mod link;
mod resolve;
pub use known_hosts::{
    ForgetHostKeyError, HostKey, HostKeyProblem, HostKeyStatus, TrustHostKeyError, forget_host_key,
    trust_host_key, user_known_hosts_lists,
};
pub(crate) use link::{Dialer, SshLink, SshStream, resolved, tunnel_target};
pub use link::{SshEnv, ssh_channel_check};
pub use resolve::{ResolvedSsh, SshHome, SshResolveError, resolve_ssh, ssh_warnings};
