//! The account and folder flows: signing in to and out of a team server, listing its
//! workspaces, refreshing a bound folder's role, and joining, syncing and settling a folder.
//! Every call blocks; tokens stay in a [`TokenStore`].

use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{
    AccountRecord, ApiError, BindingRecord, ConflictStore, PotluckClient, RemoteWorkspace,
    ServerUrl, SyncOutcome, TeamAccount, TeamFile, TeamFileError, TeamToken, TokenStore,
    public_url_of, sync_once,
};
use crate::git::run::{self, Reach};
use crate::git::{CloneError, GitOptions, SyncError, clone, ensure_on_branch, set_up};
use crate::paths::StatePaths;
use crate::secrets::SecretError;
use crate::{SecretRename, Workspace, secret_renames};

/// How a login proves who the user is.
#[non_exhaustive]
pub enum Credential {
    /// A password, exchanged for a new session token.
    Password(String),
    /// A token minted elsewhere, checked against the server's idea of its user.
    Token(TeamToken),
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Credential::Password(_) => f.write_str("Password(…)"),
            Credential::Token(_) => f.write_str("Token(…)"),
        }
    }
}

/// How a logout went on the server; the local account and token are gone either way.
#[non_exhaustive]
#[derive(Debug)]
pub struct LogoutReport {
    /// Whether the server no longer accepts the token.
    pub revoked: bool,
    /// Why the token may still be live on the server, when it might be.
    pub note: Option<String>,
}

/// Why a login failed; nothing was kept.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    /// The server refused, could not be reached or cannot host Dbento workspaces.
    #[error(transparent)]
    Api(ApiError),
    /// The token belongs to another user than the one named.
    #[error("signed in as {got}, not {expected}")]
    UserMismatch {
        /// The user named at login.
        expected: String,
        /// The token's user.
        got: String,
    },
    /// The token store failed.
    #[error(transparent)]
    Tokens(SecretError),
    /// The team file failed.
    #[error(transparent)]
    File(TeamFileError),
}

/// Why a logout failed.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum LogoutError {
    /// There is no such account to sign out.
    #[error("not signed in to {server}; run `dbento team login {server} --user <name>`")]
    NotSignedIn {
        /// The server asked about.
        server: String,
    },
    /// The token store failed.
    #[error(transparent)]
    Tokens(SecretError),
    /// The team file failed.
    #[error(transparent)]
    File(TeamFileError),
}

/// Why the workspace list could not be read.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum WorkspacesError {
    /// There is no account on this server.
    #[error("not signed in to {server}; run `dbento team login {server} --user <name>`")]
    NotSignedIn {
        /// The server asked about.
        server: String,
    },
    /// The account has no token in the store.
    #[error("signed out of {server}; run `dbento team login`")]
    SignedOut {
        /// The account's server.
        server: String,
    },
    /// The server refused or could not be reached.
    #[error(transparent)]
    Api(ApiError),
    /// The token store failed.
    #[error(transparent)]
    Tokens(SecretError),
    /// The team file failed.
    #[error(transparent)]
    File(TeamFileError),
}

/// Why a bound folder's role could not be refreshed.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    /// The folder is not bound to a team workspace.
    #[error("not a team workspace; join it with `dbento team join`")]
    NotBound,
    /// The binding's account is not signed in.
    #[error("not signed in to {server}; run `dbento team login {server} --user <name>`")]
    NotSignedIn {
        /// The binding's server.
        server: String,
    },
    /// The binding's account has no token in the store.
    #[error("signed out of {server}; run `dbento team login`")]
    SignedOut {
        /// The binding's server.
        server: String,
    },
    /// The workspace is no longer in the account's list.
    #[error("you no longer have access to this workspace on the server")]
    NoAccess,
    /// The server refused or could not be reached.
    #[error(transparent)]
    Api(ApiError),
    /// The token store failed.
    #[error(transparent)]
    Tokens(SecretError),
    /// The team file failed.
    #[error(transparent)]
    File(TeamFileError),
}

/// Signs `username` in on `server` and keeps the session: the token in `tokens`, the account
/// in the team file. The server is checked before any credential is sent; a password mints a
/// new token, a token is checked with `me`. Any failure leaves the earlier session in place.
pub fn team_login(
    tokens: &dyn TokenStore,
    paths: &StatePaths,
    server: &ServerUrl,
    username: &str,
    credential: Credential,
    allow_insecure_http: bool,
) -> Result<AccountRecord, LoginError> {
    let client = PotluckClient::new(server, allow_insecure_http).map_err(LoginError::Api)?;
    client.server_info().map_err(LoginError::Api)?;
    let (token, token_id, username) = match credential {
        Credential::Password(password) => {
            let signed = client
                .sign_in(username, &password)
                .map_err(LoginError::Api)?;
            (signed.token, Some(signed.token_id), signed.username)
        }
        Credential::Token(token) => {
            let got = client.me(&token).map_err(LoginError::Api)?;
            if got != username {
                return Err(LoginError::UserMismatch {
                    expected: username.to_owned(),
                    got,
                });
            }
            (token, None, got)
        }
    };
    // Only a freshly minted token is ours to give back when keeping it fails.
    let give_back = || {
        if let Some(id) = &token_id {
            let _ = client.revoke(&token, id);
        }
    };

    let file = TeamFile::new(&paths.team);
    let account = TeamAccount::new(server.clone(), username.as_str());
    let previous = file
        .load()
        .ok()
        .and_then(|d| d.account(server, Some(&username)).cloned());
    let previous_token = tokens.get(&account).ok().flatten();
    if let Err(e) = tokens.set(&account, &token) {
        give_back();
        return Err(LoginError::Tokens(e));
    }
    let record = AccountRecord::new(server, &username, token_id.clone(), allow_insecure_http);
    if let Err(e) = file.update(|d| d.put_account(record.clone())) {
        // The file still names the earlier session, so the store must hold its token again.
        let _ = match &previous_token {
            Some(old) => tokens.set(&account, old),
            None => tokens.delete(&account),
        };
        give_back();
        return Err(LoginError::File(e));
    }
    // The new session is kept; retire the one it replaced with the new token. A pasted token's
    // id is unknown and may be the replaced one, so only a freshly minted token retires it.
    if let (Some(old_id), Some(new_id)) = (previous.and_then(|p| p.token_id), &token_id)
        && old_id != *new_id
    {
        let _ = client.revoke(&token, &old_id);
    }
    Ok(record)
}

/// Revokes the account's token on the server where it can, then forgets the token and the
/// account. Bindings stay, so signing in again resumes them.
pub fn team_logout(
    tokens: &dyn TokenStore,
    paths: &StatePaths,
    server: &ServerUrl,
    username: Option<&str>,
) -> Result<LogoutReport, LogoutError> {
    let file = TeamFile::new(&paths.team);
    let not_signed_in = || LogoutError::NotSignedIn {
        server: server.to_string(),
    };
    let record = file
        .load()
        .map_err(LogoutError::File)?
        .account(server, username)
        .cloned()
        .ok_or_else(not_signed_in)?;
    let account = TeamAccount::new(server.clone(), record.username.as_str());
    let token = tokens.get(&account).map_err(LogoutError::Tokens)?;
    let report = revoke(server, &record, token.as_ref());
    tokens.delete(&account).map_err(LogoutError::Tokens)?;
    file.update(|d| d.remove_account(server, &record.username))
        .map_err(LogoutError::File)?;
    Ok(report)
}

/// Asks the server to revoke the account's token. A server that no longer knows it counts as
/// revoked; anything else is reported in a note and never stops the logout.
fn revoke(server: &ServerUrl, record: &AccountRecord, token: Option<&TeamToken>) -> LogoutReport {
    let not_revoked = |note: String| LogoutReport {
        revoked: false,
        note: Some(note),
    };
    let Some(token_id) = &record.token_id else {
        return not_revoked("the token came from --token-stdin; revoke it on the server".into());
    };
    let Some(token) = token else {
        return not_revoked("no token was stored; revoke it on the server".into());
    };
    let outcome = PotluckClient::new(server, record.insecure_http)
        .and_then(|client| client.revoke(token, token_id));
    match outcome {
        Ok(()) | Err(ApiError::Unauthenticated | ApiError::NotFound(_)) => LogoutReport {
            revoked: true,
            note: None,
        },
        Err(e) => not_revoked(format!(
            "the token was not revoked ({e}); revoke it on the server"
        )),
    }
}

/// The account's workspaces on `server`, sorted by team then slug.
pub fn team_workspaces(
    tokens: &dyn TokenStore,
    paths: &StatePaths,
    server: &ServerUrl,
    username: Option<&str>,
) -> Result<Vec<RemoteWorkspace>, WorkspacesError> {
    let record = TeamFile::new(&paths.team)
        .load()
        .map_err(WorkspacesError::File)?
        .account(server, username)
        .cloned()
        .ok_or_else(|| WorkspacesError::NotSignedIn {
            server: server.to_string(),
        })?;
    let account = TeamAccount::new(server.clone(), record.username.as_str());
    let token = tokens
        .get(&account)
        .map_err(WorkspacesError::Tokens)?
        .ok_or_else(|| WorkspacesError::SignedOut {
            server: server.to_string(),
        })?;
    let client = PotluckClient::new(server, record.insecure_http).map_err(WorkspacesError::Api)?;
    let mut list = client.workspaces(&token).map_err(WorkspacesError::Api)?;
    list.sort_by(|a, b| (&a.team, &a.slug).cmp(&(&b.team, &b.slug)));
    Ok(list)
}

/// Asks the server for the role of the workspace `dir` is bound to and records it when it
/// changed. Returns the binding as it now stands.
pub fn refresh_role(
    tokens: &dyn TokenStore,
    paths: &StatePaths,
    dir: &Path,
) -> Result<BindingRecord, RefreshError> {
    let file = TeamFile::new(&paths.team);
    let data = file.load().map_err(RefreshError::File)?;
    let (folder, binding) = data.binding_for(dir).ok_or(RefreshError::NotBound)?;
    let not_signed_in = || RefreshError::NotSignedIn {
        server: binding.server.clone(),
    };
    let server = ServerUrl::parse(&binding.server).map_err(|_| not_signed_in())?;
    let record = data
        .account(&server, Some(&binding.username))
        .cloned()
        .ok_or_else(not_signed_in)?;
    let account = TeamAccount::new(server.clone(), binding.username.as_str());
    let token = tokens
        .get(&account)
        .map_err(RefreshError::Tokens)?
        .ok_or_else(|| RefreshError::SignedOut {
            server: binding.server.clone(),
        })?;
    let client = PotluckClient::new(&server, record.insecure_http).map_err(RefreshError::Api)?;
    let remote = client
        .workspaces(&token)
        .map_err(RefreshError::Api)?
        .into_iter()
        .find(|w| w.team == binding.team && w.slug == binding.workspace)
        .ok_or(RefreshError::NoAccess)?;
    if remote.role == binding.role {
        return Ok(binding);
    }
    let role = remote.role;
    let io_failed = |e: std::io::Error| {
        RefreshError::File(TeamFileError::Io {
            path: paths.team.clone(),
            message: e.to_string(),
        })
    };
    // Only the role changes, and only if the binding is still there: an unlink meanwhile wins.
    let key = folder.to_string_lossy().into_owned();
    let rewritten = file
        .update(|d| {
            let mut current = d.bindings().get(&key).cloned()?;
            current.role = role;
            Some(d.put_binding(&folder, current.clone()).map(|()| current))
        })
        .map_err(RefreshError::File)?;
    rewritten.ok_or(RefreshError::NotBound)?.map_err(io_failed)
}

/// One finished sync of a bound folder.
#[non_exhaustive]
#[derive(Debug)]
pub struct FolderSync {
    /// The bound folder that synced.
    pub dir: PathBuf,
    /// Its binding as it now stands.
    pub binding: BindingRecord,
    /// How the cycle ended.
    pub outcome: SyncOutcome,
    /// Connections the server's changes renamed, whose saved secrets can follow.
    pub renames: Vec<SecretRename>,
}

/// Which side of a conflicted path to keep.
#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
pub enum Resolution {
    /// Put the saved local side back and push it.
    KeepMine,
    /// Keep the server's side and drop the saved local side.
    TakeTheirs,
}

/// Why a folder could not join a team workspace.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum JoinError {
    /// There is no account on this server.
    #[error("not signed in to {server}; run `dbento team login {server} --user <name>`")]
    NotSignedIn {
        /// The server asked about.
        server: String,
    },
    /// The account has no token in the store.
    #[error("signed out of {server}; run `dbento team login`")]
    SignedOut {
        /// The account's server.
        server: String,
    },
    /// The account's list has no such Dbento workspace.
    #[error("no workspace {0} of kind dbento is open to you on the server")]
    NoSuchWorkspace(String),
    /// The folder holds files that are not this workspace's clone.
    #[error("{} is not empty; join into a new or empty folder", .0.display())]
    NotEmpty(PathBuf),
    /// The folder, or one around it, is bound already.
    #[error("{} is already joined to a team workspace", .0.display())]
    AlreadyBound(PathBuf),
    /// The server refused or could not be reached.
    #[error(transparent)]
    Api(ApiError),
    /// Cloning failed.
    #[error(transparent)]
    Clone(CloneError),
    /// The token store failed.
    #[error(transparent)]
    Tokens(SecretError),
    /// The team file failed.
    #[error(transparent)]
    File(TeamFileError),
}

/// Why a bound folder could not sync.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum FolderSyncError {
    /// The folder is not bound to a team workspace.
    #[error("not a team workspace; join it with `dbento team join`")]
    NotBound,
    /// The binding's account is not signed in.
    #[error("not signed in to {server}; run `dbento team login {server} --user <name>`")]
    NotSignedIn {
        /// The binding's server.
        server: String,
    },
    /// The binding's account has no token, or the server no longer accepts it.
    #[error("signed out of {server}; run `dbento team login`")]
    SignedOut {
        /// The binding's server.
        server: String,
    },
    /// The workspace is no longer in the account's list.
    #[error("you no longer have access to this workspace on the server")]
    NoAccess,
    /// The server refused or could not be reached.
    #[error(transparent)]
    Api(ApiError),
    /// The git cycle failed.
    #[error(transparent)]
    Sync(SyncError),
    /// The token store failed.
    #[error(transparent)]
    Tokens(SecretError),
    /// The team file failed.
    #[error(transparent)]
    File(TeamFileError),
}

/// Why a conflicted path could not be settled.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum TeamResolveError {
    /// The path is empty, absolute or climbs out of the folder.
    #[error("{0} is not a path inside the workspace")]
    BadPath(String),
    /// The folder is not bound to a team workspace.
    #[error("not a team workspace; join it with `dbento team join`")]
    NotBound,
    /// No conflict kept a local side of this path.
    #[error("no saved copy of {0}; nothing to resolve")]
    NoSavedCopy(String),
    /// Reading or writing the files failed.
    #[error("{0}")]
    Io(String),
    /// Keeping mine wrote the file back, but the sync after it failed.
    #[error(transparent)]
    Sync(FolderSyncError),
}

/// Clones workspace `full_name` (`team/slug`) of `server` into `dir` and binds the folder.
/// `dir` must be missing, empty, or a clone of that workspace left by an earlier join that
/// stopped before binding it. Returns the recorded binding.
pub fn team_join(
    tokens: &dyn TokenStore,
    paths: &StatePaths,
    server: &ServerUrl,
    username: Option<&str>,
    full_name: &str,
    dir: &Path,
    base: &GitOptions,
) -> Result<BindingRecord, JoinError> {
    let file = TeamFile::new(&paths.team);
    let data = file.load().map_err(JoinError::File)?;
    let record = data
        .account(server, username)
        .cloned()
        .ok_or_else(|| JoinError::NotSignedIn {
            server: server.to_string(),
        })?;
    let account = TeamAccount::new(server.clone(), record.username.as_str());
    let token = tokens
        .get(&account)
        .map_err(JoinError::Tokens)?
        .ok_or_else(|| JoinError::SignedOut {
            server: server.to_string(),
        })?;
    if let Some((bound, _)) = data.binding_for(dir) {
        return Err(JoinError::AlreadyBound(bound));
    }
    // A non-empty folder is only acceptable as this workspace's own clone, checked once the
    // workspace is known; anything else is refused before the network is asked.
    let occupied = match fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_some(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(JoinError::Clone(CloneError::Io(e))),
    };
    let is_clone = dir.join(".git").symlink_metadata().is_ok();
    if occupied && !is_clone {
        return Err(JoinError::NotEmpty(dir.to_owned()));
    }
    let created = fs::symlink_metadata(dir).is_err();

    let no_such = || JoinError::NoSuchWorkspace(full_name.to_owned());
    let (team, slug) = full_name.split_once('/').ok_or_else(no_such)?;
    let client = PotluckClient::new(server, record.insecure_http).map_err(JoinError::Api)?;
    let remote = client
        .workspaces(&token)
        .map_err(JoinError::Api)?
        .into_iter()
        .find(|w| w.team == team && w.slug == slug)
        .ok_or_else(no_such)?;
    let public_url = public_url_of(&remote.git_url).map_err(JoinError::Api)?;
    // Git carries the token to this address, so it meets the same plain-http rule as the API.
    let cleartext = ServerUrl::parse(&public_url).is_ok_and(|url| url.is_insecure_http());
    if cleartext && !record.insecure_http {
        return Err(JoinError::Api(ApiError::InsecureHttp));
    }
    let binding = BindingRecord::new(
        server.as_str(),
        &public_url,
        &record.username,
        &remote.team,
        &remote.slug,
        &remote.default_branch,
        &remote.role,
    );
    let opts = base.clone().team_env(&public_url, token.expose());
    let target = binding.to_binding(dir);
    if occupied {
        if !is_clone_of(&target, &remote.git_url, &opts) {
            return Err(JoinError::NotEmpty(dir.to_owned()));
        }
        set_up(&target, &opts).map_err(JoinError::Clone)?;
    } else if let Err(e) = clone(&remote.git_url, &target, &opts) {
        if created {
            let _ = fs::remove_dir_all(dir);
        }
        return Err(JoinError::Clone(e));
    }
    let io_failed = |e: io::Error| {
        JoinError::File(TeamFileError::Io {
            path: paths.team.clone(),
            message: e.to_string(),
        })
    };
    file.update(|d| d.put_binding(dir, binding.clone()))
        .map_err(JoinError::File)?
        .map_err(io_failed)?;
    Ok(binding)
}

/// Whether the clone at `binding.dir` fetches from `git_url` and sits on the bound branch.
fn is_clone_of(binding: &super::Binding, git_url: &str, opts: &GitOptions) -> bool {
    let args = ["remote", "get-url", "origin"];
    let origin = match run::git_ok(&args, &binding.dir, Reach::Local, opts) {
        Ok(out) => String::from_utf8_lossy(&out.stdout).trim().to_owned(),
        Err(_) => return false,
    };
    origin == git_url && ensure_on_branch(binding, opts).is_ok()
}

/// One sync of the folder bound at or around `dir`: refreshes the role, runs one git cycle
/// and reports the connections the server's changes renamed. A git failure the server
/// confirms as a rejected token is `SignedOut`.
pub fn team_sync(
    tokens: &dyn TokenStore,
    paths: &StatePaths,
    dir: &Path,
    base: &GitOptions,
) -> Result<FolderSync, FolderSyncError> {
    let file = TeamFile::new(&paths.team);
    let data = file.load().map_err(FolderSyncError::File)?;
    let (folder, stored) = data.binding_for(dir).ok_or(FolderSyncError::NotBound)?;
    let not_signed_in = || FolderSyncError::NotSignedIn {
        server: stored.server.clone(),
    };
    let signed_out = || FolderSyncError::SignedOut {
        server: stored.server.clone(),
    };
    let server = ServerUrl::parse(&stored.server).map_err(|_| not_signed_in())?;
    let record = data
        .account(&server, Some(&stored.username))
        .cloned()
        .ok_or_else(not_signed_in)?;
    let account = TeamAccount::new(server.clone(), stored.username.as_str());
    let token = tokens
        .get(&account)
        .map_err(FolderSyncError::Tokens)?
        .ok_or_else(signed_out)?;

    // Offline the stored role stands; git then reports whether the server is reachable.
    let binding = match refresh_role(tokens, paths, &folder) {
        Ok(b) => b,
        Err(RefreshError::Api(ApiError::Offline(_))) => stored.clone(),
        Err(RefreshError::Api(ApiError::Unauthenticated) | RefreshError::SignedOut { .. }) => {
            return Err(signed_out());
        }
        Err(RefreshError::NotBound) => return Err(FolderSyncError::NotBound),
        Err(RefreshError::NotSignedIn { .. }) => return Err(not_signed_in()),
        Err(RefreshError::NoAccess) => return Err(FolderSyncError::NoAccess),
        Err(RefreshError::Api(e)) => return Err(FolderSyncError::Api(e)),
        Err(RefreshError::Tokens(e)) => return Err(FolderSyncError::Tokens(e)),
        Err(RefreshError::File(e)) => return Err(FolderSyncError::File(e)),
    };

    let before = Workspace::open(&folder).ok();
    let outcome = match sync_once(&binding.to_binding(&folder), token.expose(), paths, base) {
        Ok(outcome) => outcome,
        Err(SyncError::Git(e)) => {
            // Git cannot tell a rejected token from other failures; the API can.
            let rejected = PotluckClient::new(&server, record.insecure_http)
                .and_then(|client| client.me(&token));
            return Err(match rejected {
                Err(ApiError::Unauthenticated) => signed_out(),
                Ok(_) | Err(_) => FolderSyncError::Sync(SyncError::Git(e)),
            });
        }
        Err(e) => return Err(FolderSyncError::Sync(e)),
    };
    let after = Workspace::open(&folder).ok();
    let renames = match (&before, &after) {
        (Some(old), Some(new)) => secret_renames(old, new),
        _ => Vec::new(),
    };

    let binding = match outcome {
        SyncOutcome::Synced | SyncOutcome::ReadOnly => stamp_synced(&file, &folder, binding)?,
        SyncOutcome::Conflicted(_) | SyncOutcome::Refused(_) | SyncOutcome::ReadOnlyBlocked(_) => {
            binding
        }
    };
    Ok(FolderSync {
        dir: folder,
        binding,
        outcome,
        renames,
    })
}

/// Records now as the folder's last sync, unless it was unbound meanwhile.
fn stamp_synced(
    file: &TeamFile,
    folder: &Path,
    mut binding: BindingRecord,
) -> Result<BindingRecord, FolderSyncError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    binding.last_synced = Some(now);
    let key = folder.to_string_lossy().into_owned();
    let stamped = file
        .update(|d| {
            let mut current = d.bindings().get(&key).cloned()?;
            current.last_synced = Some(now);
            Some(d.put_binding(folder, current.clone()).map(|()| current))
        })
        .map_err(FolderSyncError::File)?;
    match stamped {
        Some(Ok(current)) => Ok(current),
        Some(Err(e)) => Err(FolderSyncError::File(TeamFileError::Io {
            path: file.path().to_owned(),
            message: e.to_string(),
        })),
        None => Ok(binding),
    }
}

/// Settles conflicted path `rel` of the folder bound at or around `dir`. Keeping mine puts
/// the saved local side back (or deletes the file again for a conflicted delete) and syncs;
/// taking theirs drops the saved side. Returns the sync keeping mine ran.
pub fn team_resolve(
    tokens: &dyn TokenStore,
    paths: &StatePaths,
    dir: &Path,
    rel: &str,
    choice: Resolution,
    base: &GitOptions,
) -> Result<Option<FolderSync>, TeamResolveError> {
    let inside = !rel.is_empty()
        && Path::new(rel)
            .components()
            .all(|c| matches!(c, Component::Normal(_)));
    if !inside {
        return Err(TeamResolveError::BadPath(rel.to_owned()));
    }
    let io = |e: io::Error| TeamResolveError::Io(e.to_string());
    let data = TeamFile::new(&paths.team)
        .load()
        .map_err(|e| TeamResolveError::Io(e.to_string()))?;
    let (folder, _) = data.binding_for(dir).ok_or(TeamResolveError::NotBound)?;
    let store = ConflictStore::for_workspace(paths, &folder).map_err(io)?;
    let (copy, mark) = (store.copy_path(rel), store.mark_path(rel));
    // A path both copied and marked deleted was last a local delete, so the mark wins.
    let (has_copy, has_mark) = (copy.is_file(), mark.is_file());
    if !has_copy && !has_mark {
        return Err(TeamResolveError::NoSavedCopy(rel.to_owned()));
    }
    if let Resolution::KeepMine = choice {
        let target = folder.join(rel);
        if has_mark {
            match fs::remove_file(&target) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(io(e)),
                _ => {}
            }
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(io)?;
            }
            fs::copy(&copy, &target).map_err(io)?;
        }
    }
    forget(&store, rel).map_err(io)?;
    match choice {
        Resolution::KeepMine => team_sync(tokens, paths, &folder, base)
            .map(Some)
            .map_err(TeamResolveError::Sync),
        Resolution::TakeTheirs => Ok(None),
    }
}

/// Drops `rel`'s saved copy and delete mark, then the folders they leave empty.
fn forget(store: &ConflictStore, rel: &str) -> io::Result<()> {
    for path in [store.copy_path(rel), store.mark_path(rel)] {
        match fs::remove_file(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    // Every folder between rel and the store root, deepest first; non-empty ones stay.
    for parent in Path::new(rel).ancestors().skip(1) {
        let Some(parent) = parent.to_str().filter(|p| !p.is_empty()) else {
            continue;
        };
        let _ = fs::remove_dir(store.copy_path(parent));
        let _ = fs::remove_dir(store.mark_path(parent));
    }
    Ok(())
}
