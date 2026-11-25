//! The SSH context every connect carries: core's SSH environment, the state folder a trusted
//! host key is written through, and the app's one host-key gate.

use std::sync::Arc;

use dbento_core::{HostKey, SessionOptions, SshEnv, SshHome, SshSecrets, StatePaths};

use crate::host_keys::HostKeyGate;

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
