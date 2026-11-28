//! Resolution of a connection's `ssh` table through the user's `~/.ssh/config`.
//!
//! `dbento.toml` fields win; the config fills the gaps. With an OpenSSH client set, `ssh -G`
//! resolves the alias; without one a parser does, refusing `Match` and `%` tokens. Constructs
//! dbento cannot honour (the proxies, host-key overrides) refuse rather than half-apply.

use super::{openssh, ssh_g};
use crate::workspace::{ConnectionConfig, SshAuth, SshConfig, Workspace};
use ssh2_config::{ParseRule, SshConfig as OpenSshConfig};
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufReader, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_PORT: u16 = 22;
/// How long one `ssh -G` call may take.
const OPENSSH_LIMIT: Duration = Duration::from_secs(10);
pub(super) const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// The keys OpenSSH tries when nothing names one, in its order.
pub(super) const DEFAULT_KEYS: [&str; 3] = ["id_ed25519", "id_ecdsa", "id_rsa"];

/// The home whose `.ssh/config` and default keys are read, the OS user name, and the
/// OpenSSH client that resolves aliases, if any.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshHome {
    pub(super) dir: PathBuf,
    pub(super) user: String,
    /// The `ssh` program and the `-F` config it reads (`None`: its own defaults).
    pub(super) openssh: Option<(PathBuf, Option<PathBuf>)>,
}

impl SshHome {
    /// A home at `dir` whose OS user is `user`, resolved by the parser alone.
    pub fn new(dir: impl Into<PathBuf>, user: impl Into<String>) -> SshHome {
        SshHome {
            dir: dir.into(),
            user: user.into(),
            openssh: None,
        }
    }

    /// This home resolved through `program -G`, reading `config` with `-F` when given.
    #[must_use]
    pub fn with_openssh(self, program: impl Into<PathBuf>, config: Option<PathBuf>) -> SshHome {
        SshHome {
            openssh: Some((program.into(), config)),
            ..self
        }
    }

    /// The running user's home and name (`$USER`, else `$USERNAME`), with the first `ssh` on
    /// PATH; `None` when the home or name is unknown.
    pub fn from_env() -> Option<SshHome> {
        let dir = dirs::home_dir()?;
        let user = ["USER", "USERNAME"]
            .into_iter()
            .find_map(|var| std::env::var(var).ok().filter(|u| !u.is_empty()))?;
        let home = SshHome::new(dir, user);
        Some(
            match openssh::discover(std::env::var_os("PATH").as_deref()) {
                Some(program) => home.with_openssh(program, None),
                None => home,
            },
        )
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
    /// The ssh config uses a construct dbento cannot honour yet.
    #[error(
        "ssh host `{alias}`: {construct} in {} (or a file it includes{}) is not supported yet; set host, port, user and identity_file in dbento.toml instead",
        file.display(),
        if *by_ssh { ", or the system ssh_config" } else { "" }
    )]
    Unsupported {
        /// `ssh.host` as written.
        alias: String,
        /// "Match", "ProxyJump", "ProxyCommand", "a % token", or with `ssh -G` a host-key
        /// override such as "HostKeyAlias".
        construct: &'static str,
        /// The main config file; the construct may be in a file it includes.
        file: PathBuf,
        /// Whether `ssh -G` resolved the host, so the system ssh_config may hold the construct.
        by_ssh: bool,
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
    /// The OpenSSH client failed or printed output that cannot be used; never retried with
    /// the parser.
    #[error("ssh host `{alias}`: {} -G failed: {message}", program.display())]
    OpenSsh {
        /// `ssh.host` as written.
        alias: String,
        /// The `ssh` program run.
        program: PathBuf,
        /// What went wrong, with ssh's stderr tail when it exited non-zero.
        message: String,
    },
    /// `ssh.host` holds characters ssh would read as an option, a user or a port.
    #[error(
        "ssh host `{alias}` may hold only letters, digits, `.`, `-`, `_` and `:`, and must not start with `-`"
    )]
    BadHost {
        /// `ssh.host` as written.
        alias: String,
    },
}

/// Merges `conn`'s `ssh` table with `home`'s `~/.ssh/config`, failing closed on any construct
/// dbento cannot honour. Relative key paths are read against `ws_dir`.
pub fn resolve_ssh(
    conn: &ConnectionConfig,
    ws_dir: &Path,
    home: &SshHome,
) -> Result<ResolvedSsh, SshResolveError> {
    let ssh = conn.ssh().ok_or(SshResolveError::NoSsh)?;
    if let Some((program, config)) = &home.openssh {
        return resolve_with_openssh(conn, ssh, ws_dir, home, program, config.as_deref());
    }
    let alias = ssh.host();
    let file = home.dir.join(".ssh").join("config");
    let config = read_config(&file, home)?;
    let unsupported = |construct| SshResolveError::Unsupported {
        alias: alias.to_owned(),
        construct,
        file: file.clone(),
        by_ssh: false,
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

/// `conn` (whose ssh table is `ssh`) resolved by `program -G` against a `-F none` baseline,
/// run in the SSH home so a relative key path means the same to ssh and to dbento. Every
/// failure refuses.
fn resolve_with_openssh(
    conn: &ConnectionConfig,
    ssh: &SshConfig,
    ws_dir: &Path,
    home: &SshHome,
    program: &Path,
    config: Option<&Path>,
) -> Result<ResolvedSsh, SshResolveError> {
    let alias = ssh.host();
    if !openssh::valid_alias(alias) {
        return Err(SshResolveError::BadHost {
            alias: alias.to_owned(),
        });
    }
    let failed = |message: String| SshResolveError::OpenSsh {
        alias: alias.to_owned(),
        program: program.to_path_buf(),
        message,
    };

    // dbento.toml's user and port go in as flags, so `Match user` and `%r`/`%p` see them.
    let mut overlay: Vec<OsString> = Vec::new();
    if let Some(user) = ssh.user() {
        overlay.extend(["-l".into(), user.into()]);
    }
    if let Some(port) = ssh.port() {
        overlay.extend(["-p".into(), port.to_string().into()]);
    }
    let call = |file: Option<&std::ffi::OsStr>| {
        let mut args: Vec<OsString> = vec!["-G".into()];
        if let Some(file) = file {
            args.extend(["-F".into(), file.to_owned()]);
        }
        args.extend(overlay.iter().cloned());
        args.extend(["--".into(), alias.into()]);
        let out = openssh::run(program, &args, &home.dir, OPENSSH_LIMIT).map_err(failed)?;
        String::from_utf8(out).map_err(|_| failed("printed output that is not UTF-8".to_owned()))
    };
    let out = call(config.map(Path::as_os_str))?;
    let baseline = call(Some("none".as_ref()))?;

    ssh_g::from_ssh_g(&out, &baseline, conn, ws_dir, home).map_err(|refusal| match refusal {
        ssh_g::Refusal::Unsupported(construct) => SshResolveError::Unsupported {
            alias: alias.to_owned(),
            construct,
            file: config.map_or_else(|| home.dir.join(".ssh").join("config"), Path::to_path_buf),
            by_ssh: true,
        },
        ssh_g::Refusal::Malformed(message) => failed(message),
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
