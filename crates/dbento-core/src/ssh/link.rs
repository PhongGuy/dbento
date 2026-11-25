//! The SSH link: one russh client connection whose host key passed Dbento's known_hosts
//! rules, signed in, from which `direct-tcpip` channels to the database are opened.

use super::known_hosts::{self, Identity, KnownFile, Verdict};
use super::{HostKey, ResolvedSsh, SshHome, SshResolveError, auth, resolve_ssh};
use crate::paths::StatePaths;
use crate::secrets::SshSecrets;
use crate::session::ConnectError;
use crate::workspace::{ConnectionConfig, Workspace};
use russh::client::{self, Handle, Msg};
use russh::keys::{Algorithm, HashAlg, PublicKey, PublicKeyOrCertificate};
use russh::{ChannelOpenFailure, Disconnect, Preferred};
use std::borrow::Cow;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const KEEPALIVE_MAX: usize = 3;

/// Where SSH reads from: the home holding `~/.ssh`, Dbento's own known_hosts file and the
/// ssh-agent socket, if any.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshEnv {
    home: SshHome,
    known_hosts: PathBuf,
    agent: Option<PathBuf>,
}

impl SshEnv {
    /// `home` with Dbento's known_hosts at `known_hosts` and no agent.
    pub fn new(home: SshHome, known_hosts: impl Into<PathBuf>) -> SshEnv {
        SshEnv {
            home,
            known_hosts: known_hosts.into(),
            agent: None,
        }
    }

    /// The same environment with the ssh-agent listening on `socket`.
    pub fn with_agent(self, socket: impl Into<PathBuf>) -> SshEnv {
        SshEnv {
            agent: Some(socket.into()),
            ..self
        }
    }

    /// The running user's home, `paths.ssh_known_hosts` and `$SSH_AUTH_SOCK` on Unix;
    /// `None` without a home.
    pub fn from_env(paths: &StatePaths) -> Option<SshEnv> {
        let env = SshEnv::new(SshHome::from_env()?, paths.ssh_known_hosts.clone());
        #[cfg(unix)]
        if let Some(socket) = std::env::var_os("SSH_AUTH_SOCK").filter(|s| !s.is_empty()) {
            return Some(env.with_agent(socket));
        }
        Some(env)
    }

    /// The home whose `~/.ssh` is read.
    pub fn home(&self) -> &SshHome {
        &self.home
    }
}

/// A channel to the database, carried by the SSH link.
pub(crate) type SshStream = russh::ChannelStream<Msg>;

/// One signed-in SSH connection. Dropping it asks the server to disconnect.
pub(crate) struct SshLink {
    handle: Option<Handle<Checker>>,
    runtime: tokio::runtime::Handle,
}

impl SshLink {
    /// Connects to the resolved SSH server, checks its host key and signs in, all within the
    /// resolved connect timeout.
    pub(crate) async fn open(
        conn: &ConnectionConfig,
        resolved: &ResolvedSsh,
        env: &SshEnv,
        secrets: &SshSecrets,
    ) -> Result<SshLink, ConnectError> {
        let address = format!("{}:{}", resolved.host_name, resolved.port);
        let budget = resolved.connect_timeout;
        let handle = tokio::time::timeout(budget, handshake(conn, resolved, env, secrets))
            .await
            .map_err(|_| ConnectError::SshConnect {
                address,
                message: format!("no answer within {} s", budget.as_secs()),
            })??;
        Ok(SshLink {
            handle: Some(handle),
            runtime: tokio::runtime::Handle::current(),
        })
    }

    /// Opens a `direct-tcpip` channel to `host`:`port`, which the SSH server resolves.
    pub(crate) async fn channel(&self, host: &str, port: u16) -> Result<SshStream, ConnectError> {
        let refused = |why: String| ConnectError::SshTunnel {
            message: format!(
                "the SSH server would not open a channel to {host}:{port} (resolved on the SSH server): {why}"
            ),
        };
        let Some(handle) = &self.handle else {
            return Err(refused("the SSH link is closed".to_owned()));
        };
        match handle
            .channel_open_direct_tcpip(host, u32::from(port), "127.0.0.1", 0)
            .await
        {
            Ok(channel) => Ok(channel.into_stream()),
            Err(russh::Error::ChannelOpenFailure(failure)) => Err(refused(open_failure(&failure))),
            Err(e) => Err(refused(e.to_string())),
        }
    }
}

impl Drop for SshLink {
    fn drop(&mut self) {
        // Dropping the handle alone may leave russh's session task running, so the
        // disconnect is sent from the runtime the connection lives on.
        if let Some(handle) = self.handle.take() {
            self.runtime.spawn(async move {
                let _ = handle.disconnect(Disconnect::ByApplication, "", "").await;
            });
        }
    }
}

/// Test seam: resolves `conn`'s ssh table, opens the link, opens one channel to
/// `host`:`port` and drops both.
#[doc(hidden)]
pub async fn ssh_channel_check(
    ws: &Workspace,
    conn: &ConnectionConfig,
    env: &SshEnv,
    secrets: &SshSecrets,
    host: &str,
    port: u16,
) -> Result<(), ConnectError> {
    let resolved = resolved(ws, conn, env)?;
    let link = SshLink::open(conn, &resolved, env, secrets).await?;
    link.channel(host, port).await.map(drop)
}

/// `conn`'s ssh table merged with `~/.ssh/config`, its failures worded as connect errors.
pub(crate) fn resolved(
    ws: &Workspace,
    conn: &ConnectionConfig,
    env: &SshEnv,
) -> Result<ResolvedSsh, ConnectError> {
    resolve_ssh(conn, ws.dir(), env.home()).map_err(|e| match e {
        SshResolveError::Unsupported { .. } | SshResolveError::NoSsh => {
            ConnectError::SshUnsupported {
                message: e.to_string(),
            }
        }
        SshResolveError::Config { .. } => ConnectError::SshConnect {
            address: conn.ssh().map(|s| s.host().to_owned()).unwrap_or_default(),
            message: e.to_string(),
        },
    })
}

/// The russh client settings: keepalives when asked, and the host-key algorithms already
/// known for the server first, so it presents a key known_hosts can vouch for.
pub(crate) fn client_config(keepalive: bool, known: &[Algorithm]) -> client::Config {
    let default = Preferred::DEFAULT;
    let known_family = |a: &Algorithm| known.iter().any(|k| same_family(a, k));
    let key: Vec<Algorithm> = default
        .key
        .iter()
        .filter(|a| known_family(a))
        .chain(default.key.iter().filter(|a| !known_family(a)))
        .cloned()
        .collect();
    client::Config {
        keepalive_interval: keepalive.then_some(KEEPALIVE_INTERVAL),
        keepalive_max: KEEPALIVE_MAX,
        preferred: Preferred {
            key: Cow::Owned(key),
            host_key_certificates: Cow::Borrowed(&[]),
            ..default
        },
        ..client::Config::default()
    }
}

/// Ed25519 with Ed25519, ECDSA of the same curve, and any RSA signature with any other.
fn same_family(a: &Algorithm, b: &Algorithm) -> bool {
    matches!(
        (a, b),
        (Algorithm::Ed25519, Algorithm::Ed25519) | (Algorithm::Rsa { .. }, Algorithm::Rsa { .. })
    ) || matches!((a, b), (Algorithm::Ecdsa { curve: x }, Algorithm::Ecdsa { curve: y }) if x == y)
}

/// TCP connect, SSH handshake under the host-key gate, then sign-in.
async fn handshake(
    conn: &ConnectionConfig,
    resolved: &ResolvedSsh,
    env: &SshEnv,
    secrets: &SshSecrets,
) -> Result<Handle<Checker>, ConnectError> {
    let address = format!("{}:{}", resolved.host_name, resolved.port);
    let unreachable = |message: String| ConnectError::SshConnect {
        address: address.clone(),
        message,
    };
    let files = known_files(env).map_err(unreachable)?;
    let id = Identity {
        host: &resolved.host_name,
        port: resolved.port,
    };
    let config = client_config(
        resolved.keepalive,
        &known_hosts::known_algorithms(&files, &id),
    );
    let stream = tokio::net::TcpStream::connect((resolved.host_name.as_str(), resolved.port))
        .await
        .map_err(|e| unreachable(e.to_string()))?;
    // Nagle only delays the small packets of a query round trip.
    let _ = stream.set_nodelay(true);
    let seen = Arc::new(Mutex::new(None));
    let checker = Checker {
        host: resolved.host_name.clone(),
        port: resolved.port,
        files,
        seen: Arc::clone(&seen),
    };
    let mut handle = match client::connect_stream(Arc::new(config), stream, checker).await {
        Ok(handle) => handle,
        Err(e) => {
            let verdict = seen.lock().unwrap_or_else(PoisonError::into_inner).take();
            return Err(refusal(resolved, verdict).unwrap_or_else(|| unreachable(e.to_string())));
        }
    };
    auth::authenticate(&mut handle, conn, resolved, env, secrets).await?;
    Ok(handle)
}

/// The host-key error a failed handshake stands for, when the key was what failed it.
fn refusal(resolved: &ResolvedSsh, seen: Option<(PublicKey, Verdict)>) -> Option<ConnectError> {
    let (key, verdict) = seen?;
    let (host, port) = (resolved.host_name.clone(), resolved.port);
    match verdict {
        Verdict::Trusted => None,
        Verdict::Unknown => Some(ConnectError::SshHostKeyUnknown {
            key: HostKey::new(&host, port, key),
            host,
            port,
        }),
        Verdict::Refused {
            file,
            line,
            problem,
        } => Some(ConnectError::SshHostKeyChanged {
            fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
            host,
            port,
            file,
            line,
            problem,
        }),
    }
}

/// The user's `~/.ssh/known_hosts`, then Dbento's own; a missing file is empty, an
/// unreadable one fails rather than being skipped.
fn known_files(env: &SshEnv) -> Result<Vec<KnownFile>, String> {
    let user = env.home.dir().join(".ssh").join("known_hosts");
    [user, env.known_hosts.clone()]
        .into_iter()
        .map(|path| match std::fs::read_to_string(&path) {
            Ok(text) => Ok(KnownFile { path, text }),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(KnownFile {
                path,
                text: String::new(),
            }),
            Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        })
        .collect()
}

/// Why the SSH server refused a channel, in words.
fn open_failure(failure: &ChannelOpenFailure) -> String {
    match failure {
        ChannelOpenFailure::AdministrativelyProhibited => {
            "forwarding is not allowed for this login".to_owned()
        }
        ChannelOpenFailure::ConnectFailed => "it could not connect there".to_owned(),
        ChannelOpenFailure::UnknownChannelType => "it does not forward TCP".to_owned(),
        ChannelOpenFailure::ResourceShortage => "it is out of resources".to_owned(),
        ChannelOpenFailure::Other { reason, .. } => reason.clone(),
    }
}

/// Judges the server's key by Dbento's rules and keeps the verdict, so a refused
/// handshake can say which rule refused it.
struct Checker {
    host: String,
    port: u16,
    files: Vec<KnownFile>,
    seen: Arc<Mutex<Option<(PublicKey, Verdict)>>>,
}

impl client::Handler for Checker {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, russh::Error> {
        // Certificates are never advertised (empty host_key_certificates), so none arrive.
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            return Ok(false);
        };
        let id = Identity {
            host: &self.host,
            port: self.port,
        };
        let verdict = known_hosts::check(&self.files, &id, key);
        let trusted = verdict == Verdict::Trusted;
        *self.seen.lock().unwrap_or_else(PoisonError::into_inner) = Some((key.clone(), verdict));
        Ok(trusted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::Preferred;
    use russh::keys::{Algorithm, EcdsaCurve, HashAlg};
    use std::time::Duration;

    #[test]
    fn the_client_config_keeps_alive_and_puts_known_algorithms_first() {
        let p256 = Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP256,
        };
        let c = client_config(true, std::slice::from_ref(&p256));
        assert_eq!(c.keepalive_interval, Some(Duration::from_secs(30)));
        assert_eq!(c.keepalive_max, 3);
        assert_eq!(c.preferred.key[0], p256);
        assert_eq!(c.preferred.key.len(), Preferred::DEFAULT.key.len());
        assert!(c.preferred.host_key_certificates.is_empty());
        // A known RSA key moves every RSA signature variant ahead of the rest.
        let c = client_config(true, &[Algorithm::Rsa { hash: None }]);
        assert!(
            c.preferred.key[..3]
                .iter()
                .all(|a| matches!(a, Algorithm::Rsa { .. }))
        );
        assert_eq!(
            c.preferred.key[0],
            Algorithm::Rsa {
                hash: Some(HashAlg::Sha512)
            }
        );
        let off = client_config(false, &[]);
        assert_eq!(off.keepalive_interval, None);
        assert_eq!(off.preferred.key, Preferred::DEFAULT.key);
    }
}
