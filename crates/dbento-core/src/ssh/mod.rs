//! SSH tunnels: how a connection's `ssh` table is resolved before a tunnel opens.

mod resolve;
pub use resolve::{ResolvedSsh, SshHome, SshResolveError, resolve_ssh, ssh_warnings};
