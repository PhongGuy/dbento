//! The connection form's SSH section as values: what its controls show for a loaded
//! `ssh` table, and the table Save writes back. Untouched keys keep their spelling.

use std::path::Path;

use dbento_core::{Engine, SshAuth, SshSpec};

use crate::connection_dialog::parse_port;

/// The note under the database host while SSH is on.
pub const SSH_HOST_NOTE: &str =
    "The database host is resolved on the SSH server: localhost means the SSH server itself.";
/// The note under the SSH section for MongoDB, whose hosts come from `uri`.
pub const SSH_MONGO_HOST_NOTE: &str = "Hosts in the URI, and the members a replica set reports, are resolved on the SSH server; mongodb+srv records are looked up on this machine.";
/// The note shown while SSH is on and TLS is off.
pub const SSH_PLAINTEXT_NOTE: &str =
    "TLS is off, so the leg from the SSH server to the database is not encrypted.";
/// The warning under an absolute identity file outside the home folder.
pub const SSH_IDENTITY_WARNING: &str = "This path is outside your home folder and may not exist on a teammate's machine; prefer a path under ~/.ssh.";

/// The note under the SSH section for `engine`.
pub fn ssh_host_note(engine: Engine) -> &'static str {
    if engine == Engine::Mongodb {
        SSH_MONGO_HOST_NOTE
    } else {
        SSH_HOST_NOTE
    }
}

/// The auth choice, in the order of the `conn-ssh-auth` segments: Default, Agent, Key, Password.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshAuthChoice {
    /// No `auth` key: agent, then keys, then password, as OpenSSH tries them.
    Default,
    /// `auth = "agent"`.
    Agent,
    /// `auth = "key"`.
    Key,
    /// `auth = "password"`.
    Password,
}

/// What the SSH controls hold, text untrimmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTyped {
    /// The "Connect through SSH" tick.
    pub on: bool,
    /// The SSH host or `~/.ssh/config` alias.
    pub host: String,
    /// The SSH port; empty means the default.
    pub port: String,
    /// The SSH user; empty means the default.
    pub user: String,
    /// The auth segment.
    pub auth: SshAuthChoice,
    /// The identity file as typed.
    pub identity_file: String,
    /// The keepalive tick.
    pub keepalive: bool,
}

/// Which SSH control a refusal is about; the form maps it to its `Field`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshField {
    /// The SSH host.
    Host,
    /// The SSH port.
    Port,
    /// The identity file.
    Identity,
}

/// Whether the form offers SSH for `engine`: exactly the engines core tunnels in this build.
pub fn takes_ssh(engine: Engine) -> bool {
    dbento_core::ssh_tunnels(engine)
}

/// What the SSH controls show for `loaded`; keepalive shows ticked when the key is absent.
pub fn ssh_shown(loaded: Option<&SshSpec>) -> SshTyped {
    let Some(spec) = loaded else {
        return SshTyped {
            on: false,
            host: String::new(),
            port: String::new(),
            user: String::new(),
            auth: SshAuthChoice::Default,
            identity_file: String::new(),
            keepalive: true,
        };
    };
    SshTyped {
        on: true,
        host: spec.host.clone(),
        port: spec.port.map(|p| p.to_string()).unwrap_or_default(),
        user: spec.user.clone().unwrap_or_default(),
        auth: match spec.auth {
            None => SshAuthChoice::Default,
            Some(SshAuth::Agent) => SshAuthChoice::Agent,
            Some(SshAuth::Key) => SshAuthChoice::Key,
            Some(SshAuth::Password) => SshAuthChoice::Password,
            Some(_) => SshAuthChoice::Default,
        },
        identity_file: spec.identity_file.clone().unwrap_or_default(),
        keepalive: spec.keepalive.unwrap_or(true),
    }
}

/// The table Save writes: None when unticked; an untouched value keeps the loaded spelling.
pub fn ssh_spec(
    loaded: Option<&SshSpec>,
    typed: &SshTyped,
) -> Result<Option<SshSpec>, (SshField, String)> {
    if !typed.on {
        return Ok(None);
    }
    // Each key compares the control against what it showed for the loaded table; only a
    // changed control is re-read, so an untouched key is written back exactly as it was.
    let shown = ssh_shown(loaded);
    let text = |s: &str| Some(s.trim().to_owned()).filter(|t| !t.is_empty());

    let host = match (loaded, typed.host == shown.host) {
        (Some(spec), true) => spec.host.clone(),
        _ => text(&typed.host).ok_or((SshField::Host, "Enter the SSH host".to_owned()))?,
    };
    let port = if typed.port == shown.port {
        loaded.and_then(|s| s.port)
    } else {
        match text(&typed.port) {
            None => None,
            Some(p) => Some(parse_port(&p).map_err(|says| (SshField::Port, says))?),
        }
    };
    let user = if typed.user == shown.user {
        loaded.and_then(|s| s.user.clone())
    } else {
        text(&typed.user)
    };
    let auth = if typed.auth == shown.auth {
        loaded.and_then(|s| s.auth)
    } else {
        match typed.auth {
            SshAuthChoice::Default => None,
            SshAuthChoice::Agent => Some(SshAuth::Agent),
            SshAuthChoice::Key => Some(SshAuth::Key),
            SshAuthChoice::Password => Some(SshAuth::Password),
        }
    };
    let identity_file = if typed.identity_file == shown.identity_file {
        loaded.and_then(|s| s.identity_file.clone())
    } else {
        text(&typed.identity_file)
    };
    // The loader refuses an identity file beside agent or password auth; never write one.
    if identity_file.is_some()
        && matches!(typed.auth, SshAuthChoice::Agent | SshAuthChoice::Password)
    {
        return Err((
            SshField::Identity,
            "An identity file needs auth Key or Default".to_owned(),
        ));
    }
    let keepalive = if typed.keepalive == shown.keepalive {
        loaded.and_then(|s| s.keepalive)
    } else {
        Some(typed.keepalive)
    };

    let mut spec = SshSpec::new(host);
    spec.port = port;
    spec.user = user;
    spec.auth = auth;
    spec.identity_file = identity_file;
    spec.keepalive = keepalive;
    Ok(Some(spec))
}

/// Whether `auth` may sign in with a password, so the form shows the SSH password row.
pub fn shows_password(auth: SshAuthChoice) -> bool {
    matches!(auth, SshAuthChoice::Default | SshAuthChoice::Password)
}

/// Whether `auth` may use a key file, so the form shows the key passphrase row.
pub fn shows_passphrase(auth: SshAuthChoice) -> bool {
    matches!(auth, SshAuthChoice::Default | SshAuthChoice::Key)
}

/// The warning for a typed identity file: an absolute path outside `home`, the rule core's
/// `ssh_warnings` applies to a loaded one. Without a known home every absolute path warns.
pub fn identity_warning(path: &str, home: Option<&Path>) -> Option<&'static str> {
    let path = Path::new(path.trim());
    (path.is_absolute() && !home.is_some_and(|h| path.starts_with(h)))
        .then_some(SSH_IDENTITY_WARNING)
}
