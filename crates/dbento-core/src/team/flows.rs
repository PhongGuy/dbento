//! The account flows: signing in to and out of a team server, listing its workspaces and
//! refreshing a bound folder's role. Every call blocks; tokens stay in a [`TokenStore`].

use std::fmt;
use std::path::Path;

use super::{
    AccountRecord, ApiError, BindingRecord, PotluckClient, RemoteWorkspace, ServerUrl, TeamAccount,
    TeamFile, TeamFileError, TeamToken, TokenStore,
};
use crate::paths::StatePaths;
use crate::secrets::SecretError;

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
    // The new session is kept; retire the one it replaced, using the new token, unless the
    // user pasted that very token back.
    let replaced = previous_token.is_none_or(|old| old.expose() != token.expose());
    if let Some(old_id) = previous.and_then(|p| p.token_id)
        && replaced
        && token_id.as_ref() != Some(&old_id)
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
    // Only the role changes; anything else written to the binding meanwhile is kept.
    let role = remote.role;
    let io_failed = |e: std::io::Error| {
        RefreshError::File(TeamFileError::Io {
            path: paths.team.clone(),
            message: e.to_string(),
        })
    };
    file.update(|d| {
        let mut current = d.binding_for(&folder).map_or(binding, |(_, b)| b);
        current.role = role;
        d.put_binding(&folder, current.clone()).map(|()| current)
    })
    .map_err(RefreshError::File)?
    .map_err(io_failed)
}
