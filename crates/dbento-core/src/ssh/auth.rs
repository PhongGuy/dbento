//! Signing in to the SSH server once its host key is trusted.

use super::{ResolvedSsh, SshEnv};
use crate::secrets::SshSecrets;
use crate::session::ConnectError;
use crate::workspace::{ConnectionConfig, SshAuth};
use russh::MethodSet;
use russh::client::{AuthResult, Handle, Handler};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKey};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// An agent connection over whichever transport the platform offers.
type Agent = AgentClient<Box<dyn AgentStream + Send + Unpin + 'static>>;

/// Signs in as `resolved.user`: by the configured method alone, else the agent, then each
/// identity file, then the password. A failure lists every step with its outcome, the
/// secret to set when one would help, and the methods the server still accepts.
pub(crate) async fn authenticate<H: Handler>(
    handle: &mut Handle<H>,
    conn: &ConnectionConfig,
    resolved: &ResolvedSsh,
    env: &SshEnv,
    secrets: &SshSecrets,
) -> Result<(), ConnectError> {
    let order = match resolved.auth {
        Some(only) => vec![only],
        None => vec![SshAuth::Agent, SshAuth::Key, SshAuth::Password],
    };
    let mut tries = Tries::new(handle);
    for method in order {
        let signed_in = match method {
            SshAuth::Agent => tries.agent(resolved, env).await,
            SshAuth::Key => tries.keys(resolved, secrets).await,
            SshAuth::Password => tries.password(resolved, secrets).await,
            _ => false,
        };
        if signed_in {
            return Ok(());
        }
    }
    Err(ConnectError::SshAuth {
        message: tries.failure(resolved, conn),
    })
}

/// The sign-in attempts so far: each step's outcome, which secret would help, and the
/// server's last word on what it still accepts.
struct Tries<'h, H: Handler> {
    handle: &'h mut Handle<H>,
    steps: Vec<String>,
    remaining: Option<MethodSet>,
    want_password: bool,
    want_passphrase: bool,
    rsa_hash: Option<Option<HashAlg>>,
}

impl<'h, H: Handler> Tries<'h, H> {
    fn new(handle: &'h mut Handle<H>) -> Self {
        Tries {
            handle,
            steps: Vec::new(),
            remaining: None,
            want_password: false,
            want_passphrase: false,
            rsa_hash: None,
        }
    }

    /// Offers the agent's keys, the one matching the first identity file first, so a
    /// crowded agent does not spend the server's tries on strangers.
    async fn agent(&mut self, resolved: &ResolvedSsh, env: &SshEnv) -> bool {
        let Some(mut agent) = connect_agent(env).await else {
            self.steps.push("agent: none reachable".to_owned());
            return false;
        };
        let mut keys: Vec<PublicKey> = match agent.request_identities().await {
            Ok(ids) => ids
                .into_iter()
                .filter_map(|id| match id {
                    AgentIdentity::PublicKey { key, .. } => Some(key),
                    AgentIdentity::Certificate { .. } => None,
                })
                .collect(),
            Err(e) => {
                self.steps.push(format!("agent: {e}"));
                return false;
            }
        };
        if let Some(wanted) = resolved.identity_files.first().and_then(|p| public_half(p)) {
            keys.sort_by_key(|k| k.key_data() != wanted.key_data());
        }
        if keys.is_empty() {
            self.steps.push("agent: holds no keys".to_owned());
            return false;
        }
        let offered = keys.len();
        for key in keys {
            let hash = self.hash_for(&key).await;
            let user = resolved.user.clone();
            match self
                .handle
                .authenticate_publickey_with(user, key, hash, &mut agent)
                .await
            {
                Ok(result) => {
                    if self.outcome(result).is_none() {
                        return true;
                    }
                }
                Err(e) => {
                    self.steps.push(format!("agent: {e}"));
                    return false;
                }
            }
        }
        self.steps.push(format!("agent: {offered} key(s) rejected"));
        false
    }

    /// Tries each identity file in order with the stored passphrase, if any.
    async fn keys(&mut self, resolved: &ResolvedSsh, secrets: &SshSecrets) -> bool {
        if resolved.identity_files.is_empty() {
            self.steps.push("key: no identity file".to_owned());
        }
        let phrase = secrets.key_passphrase.as_ref().map(|s| s.expose());
        for path in &resolved.identity_files {
            let shown = path.display();
            if !path.is_file() {
                let note = if unexpanded(path) {
                    " (Dbento does not expand `${VAR}` or `~user`)"
                } else {
                    ""
                };
                self.steps.push(format!("key {shown}: missing{note}"));
                continue;
            }
            let key = match russh::keys::load_secret_key(path, phrase) {
                Ok(key) => key,
                Err(russh::keys::Error::KeyIsEncrypted) => {
                    self.want_passphrase = true;
                    self.steps
                        .push(format!("key {shown}: needs its passphrase"));
                    continue;
                }
                // With a passphrase stored, a key that will not open may be holding a stale one.
                Err(e) if phrase.is_some() => {
                    self.want_passphrase = true;
                    let why = format!("unreadable with the stored passphrase ({e})");
                    self.steps.push(format!("key {shown}: {why}"));
                    continue;
                }
                Err(e) => {
                    self.steps.push(format!("key {shown}: unreadable ({e})"));
                    continue;
                }
            };
            let hash = self.hash_for(key.public_key()).await;
            let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
            match self
                .handle
                .authenticate_publickey(&resolved.user, key)
                .await
            {
                Ok(result) => match self.outcome(result) {
                    None => return true,
                    Some(outcome) => self.steps.push(format!("key {shown}: {outcome}")),
                },
                Err(e) => self.steps.push(format!("key {shown}: {e}")),
            }
        }
        false
    }

    /// Sends the stored password, or records that none is stored or that it belongs to
    /// another login. Only the logins are named in the step; neither is secret.
    async fn password(&mut self, resolved: &ResolvedSsh, secrets: &SshSecrets) -> bool {
        let Some(password) = secrets.password_for(resolved) else {
            self.want_password = true;
            let step = match secrets.withheld_from(resolved) {
                Some(pinned) => format!(
                    "password: stored for {pinned}, not sent to {}",
                    resolved.login()
                ),
                None => "password: not stored".to_owned(),
            };
            self.steps.push(step);
            return false;
        };
        match self
            .handle
            .authenticate_password(&resolved.user, password.expose())
            .await
        {
            Ok(result) => match self.outcome(result) {
                None => true,
                Some(outcome) => {
                    self.want_password = true;
                    self.steps.push(format!("password: {outcome}"));
                    false
                }
            },
            Err(e) => {
                self.steps.push(format!("password: {e}"));
                false
            }
        }
    }

    /// `None` on success; otherwise the outcome in words, keeping the remaining methods.
    fn outcome(&mut self, result: AuthResult) -> Option<&'static str> {
        match result {
            AuthResult::Success => None,
            AuthResult::Failure {
                remaining_methods,
                partial_success,
            } => {
                self.remaining = Some(remaining_methods);
                Some(if partial_success {
                    "accepted, but the server wants a second method"
                } else {
                    "rejected"
                })
            }
        }
    }

    /// The RSA signature hash the server takes, asked once; other keys need none.
    async fn hash_for(&mut self, key: &PublicKey) -> Option<HashAlg> {
        if !key.algorithm().is_rsa() {
            return None;
        }
        if self.rsa_hash.is_none() {
            let best = self.handle.best_supported_rsa_hash().await;
            self.rsa_hash = Some(best.ok().flatten().flatten());
        }
        self.rsa_hash.flatten()
    }

    /// The failure text: the login, every step, the secret hint and the server's methods.
    fn failure(&self, resolved: &ResolvedSsh, conn: &ConnectionConfig) -> String {
        let mut text = format!(
            "could not sign in as {}; tried {}",
            resolved.login(),
            self.steps.join("; ")
        );
        let owner = conn.secret_owner();
        for (wanted, flag) in [
            (self.want_password, "--ssh-password"),
            (self.want_passphrase, "--ssh-key-passphrase"),
        ] {
            if wanted {
                text.push_str(&format!(". Set it with `dbento secret set {owner} {flag}`"));
            }
        }
        if let Some(set) = &self.remaining {
            text.push_str(&format!(". The server accepts {}", methods(set)));
        }
        text
    }
}

/// The ssh-agent: the configured socket on Unix, OpenSSH's pipe then Pageant on Windows.
async fn connect_agent(env: &SshEnv) -> Option<Agent> {
    #[cfg(unix)]
    {
        let socket = env.agent()?;
        AgentClient::connect_uds(socket)
            .await
            .ok()
            .map(AgentClient::dynamic)
    }
    #[cfg(windows)]
    {
        let pipe = env.agent().map_or_else(
            || r"\\.\pipe\openssh-ssh-agent".into(),
            |p| p.as_os_str().to_owned(),
        );
        if let Ok(pipe) = AgentClient::connect_named_pipe(pipe).await {
            return Some(pipe.dynamic());
        }
        AgentClient::connect_pageant()
            .await
            .ok()
            .map(AgentClient::dynamic)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = env;
        None
    }
}

/// An identity file's public key: `<path>.pub`, else the private key's own when it opens
/// without a passphrase.
fn public_half(path: &Path) -> Option<PublicKey> {
    let mut pub_path = path.as_os_str().to_owned();
    pub_path.push(".pub");
    russh::keys::load_public_key(PathBuf::from(pub_path))
        .ok()
        .or_else(|| {
            russh::keys::load_secret_key(path, None)
                .ok()
                .map(|k| k.public_key().clone())
        })
}

/// Whether a key path still holds an OpenSSH `${VAR}` or `~user` that was taken literally.
fn unexpanded(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.contains("${")
        || text
            .split(['/', '\\'])
            .any(|part| part.len() > 1 && part.starts_with('~'))
}

/// The server's remaining methods as a comma list, or a word when there are none.
fn methods(set: &MethodSet) -> String {
    if set.is_empty() {
        return "no other method".to_owned();
    }
    set.iter().map(<&str>::from).collect::<Vec<_>>().join(", ")
}
