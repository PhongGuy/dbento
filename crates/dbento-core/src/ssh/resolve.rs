//! Resolution of a connection's `ssh` table through the user's `~/.ssh/config`.
//!
//! `dbento.toml` fields win; the config fills the gaps. Constructs dbento cannot honour yet
//! (`Match`, the proxies, `%` tokens) refuse the connection rather than being half-applied.

use crate::workspace::{ConnectionConfig, SshAuth, Workspace};
use ssh2_config::{ParseRule, SshConfig as OpenSshConfig};
use std::fs::File;
use std::io::{BufReader, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_PORT: u16 = 22;
pub(super) const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// The keys OpenSSH tries when nothing names one, in its order.
pub(super) const DEFAULT_KEYS: [&str; 3] = ["id_ed25519", "id_ecdsa", "id_rsa"];

/// The home whose `.ssh/config` and default keys are read, and the OS user name.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshHome {
    pub(super) dir: PathBuf,
    pub(super) user: String,
}

impl SshHome {
    /// A home at `dir` whose OS user is `user`.
    pub fn new(dir: impl Into<PathBuf>, user: impl Into<String>) -> SshHome {
        SshHome {
            dir: dir.into(),
            user: user.into(),
        }
    }

    /// The running user's home and name (`$USER`, else `$USERNAME`); `None` when either is unknown.
    pub fn from_env() -> Option<SshHome> {
        let dir = dirs::home_dir()?;
        let user = ["USER", "USERNAME"]
            .into_iter()
            .find_map(|var| std::env::var(var).ok().filter(|u| !u.is_empty()))?;
        Some(SshHome::new(dir, user))
    }

    /// The home directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// A connection's SSH endpoint once `dbento.toml` and `~/.ssh/config` are merged.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSsh {
    /// `ssh.host` as written.
    pub alias: String,
    /// `HostName`, else the alias.
    pub host_name: String,
    /// `dbento.toml`, else `Port`, else 22.
    pub port: u16,
    /// `dbento.toml`, else `User`, else the home's OS user.
    pub user: String,
    /// The private keys to try, in order.
    pub identity_files: Vec<PathBuf>,
    /// How the tunnel signs in, if written.
    pub auth: Option<SshAuth>,
    /// Whether the tunnel sends keepalives.
    pub keepalive: bool,
    /// `ConnectTimeout`, else 15 seconds.
    pub connect_timeout: Duration,
}

impl ResolvedSsh {
    /// `user@host_name:port`, the login an SSH password is pinned to.
    pub fn login(&self) -> String {
        format!("{}@{}:{}", self.user, self.host_name, self.port)
    }
}

/// Why a connection's `ssh` table could not be resolved.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SshResolveError {
    /// `~/.ssh/config` uses a construct dbento cannot honour yet.
    #[error(
        "ssh host `{alias}`: {construct} in {} (or a file it includes) is not supported yet; set host, port, user and identity_file in dbento.toml instead",
        file.display()
    )]
    Unsupported {
        /// `ssh.host` as written.
        alias: String,
        /// "Match", "ProxyJump", "ProxyCommand" or "a % token".
        construct: &'static str,
        /// The main config file; the construct may be in a file it includes.
        file: PathBuf,
    },
    /// `~/.ssh/config` exists but could not be read or parsed.
    #[error("cannot read {}: {message}", path.display())]
    Config {
        /// The main config file.
        path: PathBuf,
        /// The reader's or parser's reason.
        message: String,
    },
    /// The connection is direct.
    #[error("the connection has no ssh table")]
    NoSsh,
}

/// Merges `conn`'s `ssh` table with `home`'s `~/.ssh/config`, failing closed on any construct
/// dbento cannot honour. Relative key paths are read against `ws_dir`.
pub fn resolve_ssh(
    conn: &ConnectionConfig,
    ws_dir: &Path,
    home: &SshHome,
) -> Result<ResolvedSsh, SshResolveError> {
    let ssh = conn.ssh().ok_or(SshResolveError::NoSsh)?;
    let alias = ssh.host();
    let file = home.dir.join(".ssh").join("config");
    let config = read_config(&file, home)?;
    let unsupported = |construct| SshResolveError::Unsupported {
        alias: alias.to_owned(),
        construct,
        file: file.clone(),
    };

    // ssh2-config skips a Match block's condition, so its lines would leak into whichever Host
    // block precedes it; any Match anywhere, included files too, refuses.
    let has_match = config.get_hosts().iter().any(|host| {
        host.params
            .ignored_fields
            .keys()
            .any(|key| key.eq_ignore_ascii_case("match"))
    });
    if has_match {
        return Err(unsupported("Match"));
    }
    let params = config.query(alias);
    if params
        .proxy_jump
        .as_deref()
        .is_some_and(|hops| !is_none(hops))
    {
        return Err(unsupported("ProxyJump"));
    }
    if params
        .unsupported_fields
        .get("proxycommand")
        .is_some_and(|args| !is_none(args))
    {
        return Err(unsupported("ProxyCommand"));
    }

    // A `%` token is refused only in a value actually taken; one dbento.toml overrides is unused.
    let config_user = params.user.filter(|_| ssh.user().is_none());
    let config_keys = params
        .identity_file
        .filter(|_| ssh.identity_file().is_none())
        .unwrap_or_default();
    let tokened = params
        .host_name
        .iter()
        .chain(&config_user)
        .any(|v| v.contains('%'))
        || config_keys
            .iter()
            .any(|key| key.to_string_lossy().contains('%'));
    if tokened {
        return Err(unsupported("a % token"));
    }

    let identity_files = match ssh.identity_file() {
        Some(written) => vec![expand_key(written, home, ws_dir)],
        None if !config_keys.is_empty() => config_keys,
        None => DEFAULT_KEYS
            .iter()
            .map(|name| home.dir.join(".ssh").join(name))
            .filter(|path| path.is_file())
            .collect(),
    };
    Ok(ResolvedSsh {
        alias: alias.to_owned(),
        host_name: params.host_name.unwrap_or_else(|| alias.to_owned()),
        port: ssh.port().or(params.port).unwrap_or(DEFAULT_PORT),
        user: ssh
            .user()
            .map(str::to_owned)
            .or(config_user)
            .unwrap_or_else(|| home.user.clone()),
        identity_files,
        auth: ssh.auth(),
        keepalive: ssh.keepalive(),
        connect_timeout: params.connect_timeout.unwrap_or(DEFAULT_CONNECT_TIMEOUT),
    })
}

/// One line per declared connection whose ssh identity_file is absolute and outside the home.
pub fn ssh_warnings(ws: &Workspace, home: &SshHome) -> Vec<String> {
    ws.connections()
        .iter()
        .filter_map(|conn| {
            let written = conn.ssh()?.identity_file()?;
            let path = Path::new(written);
            (path.is_absolute() && !path.starts_with(&home.dir)).then(|| {
                format!(
                    "connection `{}`: ssh identity_file `{written}` is an absolute path outside the home directory, so it will not resolve on another machine; use a path under ~/ instead",
                    conn.name()
                )
            })
        })
        .collect()
}

/// The parsed `~/.ssh/config`, or an empty one when the file does not exist.
fn read_config(file: &Path, home: &SshHome) -> Result<OpenSshConfig, SshResolveError> {
    let failed = |message: String| SshResolveError::Config {
        path: file.to_owned(),
        message,
    };
    let opened = match File::open(file) {
        Ok(opened) => opened,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(OpenSshConfig::default()),
        Err(e) => return Err(failed(e.to_string())),
    };
    OpenSshConfig::default()
        .home_dir(&home.dir)
        .parse(
            &mut BufReader::new(opened),
            ParseRule::ALLOW_UNKNOWN_FIELDS | ParseRule::ALLOW_UNSUPPORTED_FIELDS,
        )
        .map_err(|e| failed(e.to_string()))
}

/// Whether a proxy setting is the single word `none`, which switches it off.
fn is_none(args: &[String]) -> bool {
    matches!(args, [only] if only.eq_ignore_ascii_case("none"))
}

/// A dbento.toml key path: `~/` against the home, relative against the workspace.
pub(super) fn expand_key(written: &str, home: &SshHome, ws_dir: &Path) -> PathBuf {
    if written == "~" {
        return home.dir.clone();
    }
    match written.strip_prefix("~/") {
        Some(rest) => home.dir.join(rest),
        None => ws_dir.join(written),
    }
}
