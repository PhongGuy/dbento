//! The Potluck server's JSON API over blocking HTTP: one agent per server, bounded in time and
//! size, that follows no redirect and uses no proxy, so the bearer token only reaches the
//! server the user typed.

use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use ureq::http::{HeaderValue, Response, StatusCode};
use ureq::tls::{RootCerts, TlsConfig};
use ureq::{Agent, Body};
use url::Url;

use super::account::{ServerUrl, TeamToken};
use crate::git::redact_token;

/// The name every token this client asks for carries on the server.
const TOKEN_NAME: &str = "Dbento";

/// The workspace kind this client lists and requires the server to offer.
const KIND: &str = "dbento";

/// The only API generation this client speaks.
const API_VERSION: u32 = 1;

/// The largest answer body read, in bytes.
const BODY_CAP: u64 = 1024 * 1024;

/// Why a call to the team server failed. Server text in it never carries a token.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
    /// The server is plain http on a remote host and the user did not allow that.
    #[error("plain http to a remote server; pass --insecure-http to allow it")]
    InsecureHttp,
    /// The server speaks another API generation or does not host Dbento workspaces.
    #[error("this server cannot host Dbento workspaces: {0}")]
    Incompatible(String),
    /// The token is missing, expired or revoked (401).
    #[error("not signed in to the team server")]
    Unauthenticated,
    /// The account may not do this (403).
    #[error("the team server refused: {0}")]
    Forbidden(String),
    /// The thing asked for does not exist (404).
    #[error("not found on the team server: {0}")]
    NotFound(String),
    /// Too many requests (429), with the server's wait in seconds when it gave one.
    #[error("the team server asks to slow down")]
    RateLimited {
        /// The `Retry-After` header as whole seconds.
        retry_after_secs: Option<u64>,
    },
    /// The server could not be reached or did not answer in time.
    #[error("the team server cannot be reached: {0}")]
    Offline(String),
    /// Any other error status; the strings are empty when the body had no error envelope.
    #[error("the team server failed: {code} {message}")]
    Server {
        /// The envelope's machine-readable code.
        code: String,
        /// The envelope's message.
        message: String,
    },
    /// An answer this client cannot use: bad JSON, a redirect, an oversized body, or a
    /// malformed token or git URL.
    #[error("the team server sent an unusable answer: {0}")]
    Protocol(String),
}

/// What the server says about itself before sign-in.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct PotluckInfo {
    /// The server's release.
    pub version: String,
    /// The API generation.
    pub api: u32,
}

/// A password sign-in's result: a fresh token and the id that revokes it.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct SignIn {
    /// The session token.
    pub token: TeamToken,
    /// The token's id on the server, for revoking it.
    pub token_id: String,
    /// The signed-in user's name.
    pub username: String,
}

/// A Dbento workspace the account can see.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize)]
pub struct RemoteWorkspace {
    /// The team's slug.
    pub team: String,
    /// The workspace's slug within the team.
    pub slug: String,
    /// The display name.
    pub name: String,
    /// The branch a new folder follows.
    pub default_branch: String,
    /// The account's role text: `read`, `write` or `admin`.
    pub role: String,
    /// The repository's address, `{public_url}/git/{team}/{slug}.git`.
    pub git_url: String,
    /// The workspace's change notice stream.
    pub events_url: String,
}

/// A blocking client for one Potluck server.
#[non_exhaustive]
#[derive(Debug)]
pub struct PotluckClient {
    server: ServerUrl,
    agent: Agent,
}

impl PotluckClient {
    /// A client for `server`: no redirects, no proxy, 10 s to connect and 30 s per call.
    /// Plain http to a remote host is refused unless `allow_insecure_http`.
    pub fn new(server: &ServerUrl, allow_insecure_http: bool) -> Result<PotluckClient, ApiError> {
        if server.is_insecure_http() && !allow_insecure_http {
            return Err(ApiError::InsecureHttp);
        }
        let config = Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None)
            .user_agent(concat!("dbento/", env!("CARGO_PKG_VERSION")))
            .accept("application/json")
            .accept_encoding("identity")
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(30)))
            .build();
        Ok(PotluckClient {
            server: server.clone(),
            agent: Agent::new_with_config(config),
        })
    }

    /// The server this talks to.
    pub fn server(&self) -> &ServerUrl {
        &self.server
    }

    /// `GET /api/v1/server`, refused as `Incompatible` unless it speaks API 1 and offers the
    /// `dbento` kind.
    pub fn server_info(&self) -> Result<PotluckInfo, ApiError> {
        let response = self.agent.get(self.url(&["server"])?.as_str()).call();
        let wire: ServerInfoWire = parse(&answer(response)?)?;
        if wire.api != API_VERSION {
            return Err(ApiError::Incompatible(format!(
                "it speaks API {}, Dbento needs {API_VERSION}",
                wire.api
            )));
        }
        if !wire.kinds.contains_key(KIND) {
            return Err(ApiError::Incompatible(
                "it does not offer Dbento workspaces".to_owned(),
            ));
        }
        Ok(PotluckInfo {
            version: wire.version,
            api: wire.api,
        })
    }

    /// `POST /api/v1/session`: a password sign-in that mints a token named `Dbento`.
    pub fn sign_in(&self, username: &str, password: &str) -> Result<SignIn, ApiError> {
        let body = json!({"username": username, "password": password, "token_name": TOKEN_NAME});
        let response = self
            .agent
            .post(self.url(&["session"])?.as_str())
            .header("content-type", "application/json")
            .send(body.to_string());
        let wire: SignInWire = parse(&answer(response)?)?;
        let token = TeamToken::parse(&wire.token)
            .ok_or_else(|| ApiError::Protocol("the session token is malformed".to_owned()))?;
        Ok(SignIn {
            token,
            token_id: wire.token_meta.id.to_string(),
            username: wire.user.username,
        })
    }

    /// `GET /api/v1/me`: the token's user name.
    pub fn me(&self, token: &TeamToken) -> Result<String, ApiError> {
        let response = self
            .agent
            .get(self.url(&["me"])?.as_str())
            .header("authorization", bearer(token)?)
            .call();
        let wire: MeWire = parse(&answer(response)?)?;
        Ok(wire.user.username)
    }

    /// `GET /api/v1/workspaces?kind=dbento`; every `git_url` must name a Potluck repository.
    pub fn workspaces(&self, token: &TeamToken) -> Result<Vec<RemoteWorkspace>, ApiError> {
        let mut url = self.url(&["workspaces"])?;
        url.query_pairs_mut().append_pair("kind", KIND);
        let response = self
            .agent
            .get(url.as_str())
            .header("authorization", bearer(token)?)
            .call();
        let list: Vec<RemoteWorkspace> = parse(&answer(response)?)?;
        for workspace in &list {
            public_url_of(&workspace.git_url)?;
        }
        Ok(list)
    }

    /// `DELETE /api/v1/tokens/{id}`: revokes a token.
    pub fn revoke(&self, token: &TeamToken, token_id: &str) -> Result<(), ApiError> {
        let response = self
            .agent
            .delete(self.url(&["tokens", token_id])?.as_str())
            .header("authorization", bearer(token)?)
            .call();
        answer(response).map(drop)
    }

    /// `{server}/api/v1/<segments>`, each segment percent-encoded, keeping any path prefix the
    /// server address carries.
    fn url(&self, segments: &[&str]) -> Result<Url, ApiError> {
        let bad = || ApiError::Protocol(format!("{} is not a usable address", self.server));
        let mut url = Url::parse(self.server.as_str()).map_err(|_| bad())?;
        let base: Vec<String> = url
            .path_segments()
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        url.path_segments_mut()
            .map_err(|_| bad())?
            .clear()
            .extend(&base)
            .extend(["api", "v1"])
            .extend(segments);
        Ok(url)
    }
}

/// `Bearer <token>`, marked sensitive so the http stack never prints it.
fn bearer(token: &TeamToken) -> Result<HeaderValue, ApiError> {
    let mut value = HeaderValue::from_str(&format!("Bearer {}", token.expose()))
        .map_err(|_| ApiError::Protocol("the session token cannot be sent".to_owned()))?;
    value.set_sensitive(true);
    Ok(value)
}

/// The 2xx body, or the error the status and its envelope stand for. A redirect is never
/// followed, so a 3xx is an answer this client cannot use.
fn answer(response: Result<Response<Body>, ureq::Error>) -> Result<Vec<u8>, ApiError> {
    let response = response.map_err(transport)?;
    let status = response.status();
    if status.is_success() {
        return read_capped(response);
    }
    if status.is_redirection() {
        return Err(ApiError::Protocol(format!(
            "the server answered {status} instead of the API"
        )));
    }
    let retry_after_secs = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    // An unreadable or oversized error body still reports the status.
    let body = read_capped(response).unwrap_or_default();
    let envelope = serde_json::from_slice::<EnvelopeWire>(&body)
        .map(|e| e.error)
        .unwrap_or_default();
    let (code, message) = (
        redact_token(&envelope.code),
        redact_token(&envelope.message),
    );
    Err(match status {
        StatusCode::UNAUTHORIZED => ApiError::Unauthenticated,
        StatusCode::FORBIDDEN => ApiError::Forbidden(message),
        StatusCode::NOT_FOUND => ApiError::NotFound(message),
        StatusCode::TOO_MANY_REQUESTS => ApiError::RateLimited { retry_after_secs },
        _ => ApiError::Server { code, message },
    })
}

/// A send or body error means the server could not be reached, a time limit included.
fn transport(e: ureq::Error) -> ApiError {
    ApiError::Offline(redact_token(&e.to_string()))
}

/// The whole body, refused as soon as it outgrows `BODY_CAP` bytes.
fn read_capped(response: Response<Body>) -> Result<Vec<u8>, ApiError> {
    response
        .into_body()
        .into_with_config()
        .limit(BODY_CAP)
        .read_to_vec()
        .map_err(|e| {
            if matches!(e, ureq::Error::BodyExceedsLimit(_)) {
                ApiError::Protocol(format!("the answer is larger than {BODY_CAP} bytes"))
            } else {
                transport(e)
            }
        })
}

fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|e| ApiError::Protocol(redact_token(&e.to_string())))
}

/// The address git and the API share for a repository's `git_url`: the text before the last
/// `/git/` that a `<team>/<workspace>.git` remainder follows, returned byte for byte. Takes
/// http or https, and `file` in debug builds so tests can use bare repositories.
pub fn public_url_of(git_url: &str) -> Result<String, ApiError> {
    let bad = || ApiError::Protocol(format!("{git_url} is not a team repository address"));
    let url = Url::parse(git_url).map_err(|_| bad())?;
    let scheme_ok = match url.scheme() {
        "http" | "https" => url.host().is_some(),
        "file" => cfg!(debug_assertions),
        _ => false,
    };
    if !scheme_ok || url.query().is_some() || url.fragment().is_some() {
        return Err(bad());
    }
    git_url
        .rmatch_indices("/git/")
        .map(|(at, sep)| (&git_url[..at], &git_url[at + sep.len()..]))
        .find(|(_, rest)| repository_path(rest))
        .map(|(public, _)| public.to_owned())
        .ok_or_else(bad)
}

/// `<team>/<workspace>.git` with both names non-empty.
fn repository_path(rest: &str) -> bool {
    let Some((team, file)) = rest.split_once('/') else {
        return false;
    };
    let workspace = file.strip_suffix(".git").unwrap_or_default();
    !team.is_empty() && !workspace.is_empty() && !workspace.contains('/')
}

#[derive(Deserialize)]
struct ServerInfoWire {
    version: String,
    api: u32,
    #[serde(default)]
    kinds: Map<String, Value>,
}

#[derive(Deserialize)]
struct UserWire {
    username: String,
}

#[derive(Deserialize)]
struct SignInWire {
    token: String,
    token_meta: TokenMetaWire,
    user: UserWire,
}

// The server numbers its tokens; the id only ever goes back into a `/tokens/{id}` path.
#[derive(Deserialize)]
struct TokenMetaWire {
    id: i64,
}

#[derive(Deserialize)]
struct MeWire {
    user: UserWire,
}

#[derive(Deserialize)]
struct EnvelopeWire {
    error: ErrorWire,
}

#[derive(Deserialize, Default)]
struct ErrorWire {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}
