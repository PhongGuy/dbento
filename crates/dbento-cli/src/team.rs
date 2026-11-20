//! `dbento team`: sign in to a Potluck team server, sign out, and list its workspaces.
//! The password and token come from a prompt or stdin, never argv, and the token is kept
//! only in the token store.

use std::fmt::Display;
use std::io::IsTerminal;
use std::path::Path;
use std::sync::OnceLock;

use dbento_core::paths::StatePaths;
use dbento_core::{
    AccountRecord, ApiError, Credential, KeychainTokenStore, LoginError, MemoryTokenStore,
    SecretError, ServerUrl, TeamAccount, TeamFile, TeamToken, TokenStore, WorkspacesError,
    team_login, team_logout, team_workspaces,
};

use crate::Exit;
use crate::secret::read_secret;

#[derive(clap::Subcommand)]
pub(crate) enum TeamCommand {
    /// Sign in; the password is read from the terminal prompt or the first line of stdin.
    Login {
        /// The team server's address.
        server: String,
        /// The account name.
        #[arg(long)]
        user: String,
        /// Read the password from the first line of stdin.
        #[arg(long, conflicts_with = "token_stdin")]
        password_stdin: bool,
        /// Read an existing access token from the first line of stdin instead of a password.
        #[arg(long)]
        token_stdin: bool,
        /// Allow plain http to a server that is not on this machine.
        #[arg(long)]
        insecure_http: bool,
    },
    /// Revoke the session token where possible and forget the account.
    Logout {
        /// The team server's address.
        server: String,
        /// The account, when several are signed in to the server.
        #[arg(long)]
        user: Option<String>,
    },
    /// List the workspaces open to the account.
    Workspaces {
        /// The team server's address.
        server: String,
        /// The account, when several are signed in to the server.
        #[arg(long)]
        user: Option<String>,
    },
}

/// Runs one team command. Every failure is printed here as `error: <message>`, so this never
/// hands an error back to `main`.
pub(crate) fn run(_workspace: Option<&Path>, cmd: TeamCommand) -> Exit {
    let Some(paths) = StatePaths::from_env() else {
        return failed("cannot find a state directory; set DBENTO_STATE_DIR");
    };
    let tokens = token_store(&paths);
    match cmd {
        TeamCommand::Login {
            server,
            user,
            password_stdin,
            token_stdin,
            insecure_http,
        } => login(
            tokens.as_ref(),
            &paths,
            &server,
            &user,
            password_stdin,
            token_stdin,
            insecure_http,
        ),
        TeamCommand::Logout { server, user } => {
            let server = match parse_server(&server) {
                Ok(server) => server,
                Err(exit) => return exit,
            };
            match team_logout(tokens.as_ref(), &paths, &server, user.as_deref()) {
                Ok(report) => {
                    eprintln!("signed out of {server}");
                    if let Some(note) = report.note {
                        eprintln!("note: {note}");
                    }
                    Exit::Ok
                }
                Err(e) => failed(e),
            }
        }
        TeamCommand::Workspaces { server, user } => {
            let server = match parse_server(&server) {
                Ok(server) => server,
                Err(exit) => return exit,
            };
            match team_workspaces(tokens.as_ref(), &paths, &server, user.as_deref()) {
                Ok(list) => {
                    for ws in list {
                        println!(
                            "{}/{}  {}  {}",
                            ws.team, ws.slug, ws.role, ws.default_branch
                        );
                    }
                    Exit::Ok
                }
                Err(e) => workspaces_failed(&e),
            }
        }
    }
}

fn login(
    tokens: &dyn TokenStore,
    paths: &StatePaths,
    server: &str,
    user: &str,
    password_stdin: bool,
    token_stdin: bool,
    insecure_http: bool,
) -> Exit {
    let server = match parse_server(server) {
        Ok(server) => server,
        Err(exit) => return exit,
    };
    // Refuse plain http before asking for anything, so no credential is ever read for it.
    if server.is_insecure_http() && !insecure_http {
        return failed(ApiError::InsecureHttp);
    }
    let credential = match read_credential(&server, user, password_stdin, token_stdin) {
        Ok(credential) => credential,
        Err(message) => return failed(message),
    };
    match team_login(tokens, paths, &server, user, credential, insecure_http) {
        Ok(record) => {
            eprintln!("signed in as {} on {server}", record.username);
            Exit::Ok
        }
        Err(LoginError::Api(e)) => api_failed(&e),
        Err(e) => failed(e),
    }
}

/// The password or token from stdin when a flag asks for it, else from a terminal prompt.
fn read_credential(
    server: &ServerUrl,
    user: &str,
    password_stdin: bool,
    token_stdin: bool,
) -> Result<Credential, String> {
    // The read error never quotes the input, which may be the secret itself.
    let read = |input: &mut dyn std::io::BufRead| {
        read_secret(input)
            .map(|s| s.expose().to_owned())
            .map_err(|e| format!("{e:#}"))
    };
    if token_stdin {
        let text = read(&mut std::io::stdin().lock())?;
        return TeamToken::parse(&text)
            .map(Credential::Token)
            .ok_or_else(|| "that is not a team access token (plk_...)".to_owned());
    }
    if password_stdin {
        return read(&mut std::io::stdin().lock()).map(Credential::Password);
    }
    if !std::io::stdin().is_terminal() {
        return Err(
            "no terminal to ask for the password; pass --password-stdin or --token-stdin".into(),
        );
    }
    let typed = rpassword::prompt_password(format!("Password for {user} on {server}: "))
        .map_err(|e| format!("cannot read the password: {e}"))?;
    read(&mut typed.as_bytes()).map(Credential::Password)
}

fn parse_server(text: &str) -> Result<ServerUrl, Exit> {
    ServerUrl::parse(text).map_err(failed)
}

fn failed(e: impl Display) -> Exit {
    eprintln!("error: {e}");
    Exit::Failed
}

/// A 401 means the session is gone, which callers tell apart from other failures by exit code.
fn api_failed(e: &ApiError) -> Exit {
    match e {
        ApiError::Unauthenticated => {
            eprintln!("error: {e}");
            Exit::SignedOut
        }
        ApiError::RateLimited { retry_after_secs } => {
            match retry_after_secs {
                Some(secs) => eprintln!("error: too many attempts; try again in {secs} s"),
                None => eprintln!("error: too many attempts; try again later"),
            }
            Exit::Failed
        }
        _ => failed(e),
    }
}

fn workspaces_failed(e: &WorkspacesError) -> Exit {
    match e {
        WorkspacesError::SignedOut { .. } => {
            eprintln!("error: {e}");
            Exit::SignedOut
        }
        WorkspacesError::Api(api) => api_failed(api),
        _ => failed(e),
    }
}

/// The platform keychain's token store, opened on first use. With `DBENTO_TEST_NO_KEYCHAIN`
/// set (debug builds) every call fails `SecretError::Unavailable` without opening it.
#[derive(Default)]
pub(crate) struct LazyTokenKeychain {
    store: OnceLock<Result<KeychainTokenStore, SecretError>>,
}

impl LazyTokenKeychain {
    fn store(&self) -> Result<&KeychainTokenStore, SecretError> {
        if cfg!(debug_assertions) && std::env::var_os("DBENTO_TEST_NO_KEYCHAIN").is_some() {
            return Err(SecretError::Unavailable {
                message: "disabled by DBENTO_TEST_NO_KEYCHAIN".into(),
            });
        }
        self.store
            .get_or_init(KeychainTokenStore::new)
            .as_ref()
            .map_err(Clone::clone)
    }
}

impl TokenStore for LazyTokenKeychain {
    fn get(&self, account: &TeamAccount) -> Result<Option<TeamToken>, SecretError> {
        self.store()?.get(account)
    }

    fn set(&self, account: &TeamAccount, token: &TeamToken) -> Result<(), SecretError> {
        self.store()?.set(account, token)
    }

    fn delete(&self, account: &TeamAccount) -> Result<(), SecretError> {
        self.store()?.delete(account)
    }
}

/// Where session tokens live: the keychain, or in debug builds with
/// `DBENTO_TEST_TOKEN_STORE=memory` a memory store, so tests never reach the real keychain.
pub(crate) fn token_store(paths: &StatePaths) -> Box<dyn TokenStore> {
    if cfg!(debug_assertions)
        && std::env::var_os("DBENTO_TEST_TOKEN_STORE").is_some_and(|v| v == "memory")
    {
        // Each test process starts from its parent's per-run token, for every recorded account.
        let store = MemoryTokenStore::new();
        if let Some(seed) = std::env::var("DBENTO_TEST_TEAM_TOKEN")
            .ok()
            .and_then(|t| TeamToken::parse(&t))
        {
            let accounts = TeamFile::new(&paths.team)
                .load()
                .map(|d| d.accounts().to_vec())
                .unwrap_or_default();
            for account in accounts.iter().filter_map(AccountRecord::account) {
                let _ = store.set(&account, &seed);
            }
        }
        return Box::new(store);
    }
    Box::new(LazyTokenKeychain::default())
}
