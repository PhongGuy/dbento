//! A Potluck account: the server's address, the session token and where tokens are kept.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use url::{Host, Url};

use crate::secrets::{SecretError, timed_call};

/// A team server's base address: http or https, lowercase scheme and host, no trailing slash.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ServerUrl(String);

impl ServerUrl {
    /// Trims; adds `https://` when there is no `://`; requires http or https, a host, and no
    /// userinfo, query or fragment; lowercases scheme and host; drops a trailing `/`.
    pub fn parse(input: &str) -> Result<ServerUrl, ServerUrlError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(ServerUrlError("Enter the server address.".to_owned()));
        }
        let bad = || ServerUrlError("Use an address like https://team.example.com.".to_owned());
        let full = if trimmed.contains("://") {
            trimmed.to_owned()
        } else {
            format!("https://{trimmed}")
        };
        // The url crate already lowercases the scheme and a domain host and drops default ports.
        let url = Url::parse(&full).map_err(|_| bad())?;
        let usable = matches!(url.scheme(), "http" | "https")
            && url.host().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none();
        if !usable {
            return Err(bad());
        }
        let text = url.as_str();
        Ok(ServerUrl(text.strip_suffix('/').unwrap_or(text).to_owned()))
    }

    /// The normalised address.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Plain http to a host that is not localhost, 127.0.0.0/8 or ::1.
    pub fn is_insecure_http(&self) -> bool {
        let Ok(url) = Url::parse(&self.0) else {
            return true;
        };
        let loopback = match url.host() {
            Some(Host::Domain(name)) => name == "localhost",
            Some(Host::Ipv4(ip)) => ip.is_loopback(),
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        };
        url.scheme() == "http" && !loopback
    }
}

impl fmt::Display for ServerUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a server address was refused, worded for the user.
#[non_exhaustive]
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct ServerUrlError(pub String);

/// A session token for one server. Its value shows only through `expose`, never in Debug.
#[non_exhaustive]
#[derive(Clone)]
pub struct TeamToken(String);

impl TeamToken {
    /// `plk_` plus exactly 43 base64url characters, else `None`.
    pub fn parse(text: &str) -> Option<TeamToken> {
        let rest = text.strip_prefix("plk_")?;
        let shaped = rest.len() == 43
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        shaped.then(|| TeamToken(text.to_owned()))
    }

    /// The token text, for the keychain, the Authorization header and git's env only.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for TeamToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TeamToken(plk_…)")
    }
}

/// One user on one team server: the key a token is stored under.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TeamAccount {
    /// The server the account lives on.
    pub server: ServerUrl,
    /// The sign-in name.
    pub username: String,
}

impl TeamAccount {
    /// `username` on `server`.
    pub fn new(server: ServerUrl, username: impl Into<String>) -> TeamAccount {
        TeamAccount {
            server,
            username: username.into(),
        }
    }
}

/// Somewhere session tokens are kept, one per account. A stored value that is not a token
/// reads as `None`, and deleting an absent entry succeeds.
pub trait TokenStore: Send + Sync {
    /// The account's token, if one is stored.
    fn get(&self, account: &TeamAccount) -> Result<Option<TeamToken>, SecretError>;
    /// Stores or replaces the account's token.
    fn set(&self, account: &TeamAccount, token: &TeamToken) -> Result<(), SecretError>;
    /// Removes the account's token, if any.
    fn delete(&self, account: &TeamAccount) -> Result<(), SecretError>;
}

/// A [`TokenStore`] in process memory, for tests and callers without a keychain.
#[derive(Debug, Default)]
pub struct MemoryTokenStore {
    tokens: Mutex<HashMap<TeamAccount, TeamToken>>,
}

impl MemoryTokenStore {
    /// An empty store.
    pub fn new() -> MemoryTokenStore {
        MemoryTokenStore::default()
    }

    fn tokens(&self) -> MutexGuard<'_, HashMap<TeamAccount, TeamToken>> {
        self.tokens.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl TokenStore for MemoryTokenStore {
    fn get(&self, account: &TeamAccount) -> Result<Option<TeamToken>, SecretError> {
        Ok(self.tokens().get(account).cloned())
    }

    fn set(&self, account: &TeamAccount, token: &TeamToken) -> Result<(), SecretError> {
        self.tokens().insert(account.clone(), token.clone());
        Ok(())
    }

    fn delete(&self, account: &TeamAccount) -> Result<(), SecretError> {
        self.tokens().remove(account);
        Ok(())
    }
}

/// Bounds each call of `inner` to `limit`. The first timeout marks the store stuck, and later
/// calls fail at once without a thread.
pub struct TimedTokenStore {
    inner: Arc<dyn TokenStore>,
    limit: Duration,
    stuck: Arc<AtomicBool>,
}

impl TimedTokenStore {
    /// Wraps `inner`, giving each call `limit` to answer.
    pub fn new(inner: Arc<dyn TokenStore>, limit: Duration) -> TimedTokenStore {
        TimedTokenStore {
            inner,
            limit,
            stuck: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl fmt::Debug for TimedTokenStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TimedTokenStore")
            .field("limit", &self.limit)
            .field("stuck", &self.stuck)
            .finish_non_exhaustive()
    }
}

impl TokenStore for TimedTokenStore {
    fn get(&self, account: &TeamAccount) -> Result<Option<TeamToken>, SecretError> {
        let account = account.clone();
        timed_call(&self.inner, self.limit, &self.stuck, move |s| {
            s.get(&account)
        })
    }

    fn set(&self, account: &TeamAccount, token: &TeamToken) -> Result<(), SecretError> {
        let (account, token) = (account.clone(), token.clone());
        timed_call(&self.inner, self.limit, &self.stuck, move |s| {
            s.set(&account, &token)
        })
    }

    fn delete(&self, account: &TeamAccount) -> Result<(), SecretError> {
        let account = account.clone();
        timed_call(&self.inner, self.limit, &self.stuck, move |s| {
            s.delete(&account)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(fill: char) -> TeamToken {
        TeamToken::parse(&format!("plk_{}", fill.to_string().repeat(43))).unwrap()
    }

    #[test]
    fn a_server_url_is_normalised() {
        let p = |s: &str| ServerUrl::parse(s).unwrap().as_str().to_owned();
        assert_eq!(p(" HTTPS://Team.Example.com/ "), "https://team.example.com");
        assert_eq!(p("team.example.com"), "https://team.example.com");
        assert_eq!(
            p("http://127.0.0.1:8080/potluck/"),
            "http://127.0.0.1:8080/potluck"
        );
        for bad in [
            "",
            "ftp://x.example",
            "https://u:p@x.example",
            "https://x.example/?q=1",
            "https://x.example/#f",
        ] {
            assert!(ServerUrl::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_plain_http_to_a_remote_host_is_insecure() {
        for (url, insecure) in [
            ("http://team.example.com", true),
            ("http://10.0.0.5:8080", true),
            ("http://localhost:9000", false),
            ("http://127.0.0.1:9000", false),
            ("http://[::1]:9000", false),
            ("https://team.example.com", false),
        ] {
            assert_eq!(
                ServerUrl::parse(url).unwrap().is_insecure_http(),
                insecure,
                "{url}"
            );
        }
    }

    #[test]
    fn a_token_has_the_potluck_shape_and_never_shows_in_debug() {
        let text = format!("plk_{}abc", "A1-_".repeat(10));
        let t = TeamToken::parse(&text).unwrap();
        assert_eq!(t.expose(), text);
        assert!(!format!("{t:?}").contains("A1-_"), "debug shows the token");
        assert!(TeamToken::parse("plk_short").is_none(), "short token");
        assert!(
            TeamToken::parse(&format!("xyz_{}", "A".repeat(43))).is_none(),
            "wrong prefix"
        );
        assert!(
            TeamToken::parse(&format!("plk_{}=", "A".repeat(42))).is_none(),
            "padding character"
        );
    }

    #[test]
    fn the_memory_store_keeps_one_token_per_server_and_user() {
        let store = MemoryTokenStore::new();
        let server = ServerUrl::parse("https://t.example").unwrap();
        let (ada, bob) = (
            TeamAccount::new(server.clone(), "ada"),
            TeamAccount::new(server, "bob"),
        );
        assert!(store.get(&ada).unwrap().is_none(), "empty store");
        store.set(&ada, &token('B')).unwrap();
        assert_eq!(
            store.get(&ada).unwrap().unwrap().expose(),
            token('B').expose()
        );
        assert!(store.get(&bob).unwrap().is_none(), "bob has no token");
        store.delete(&ada).unwrap();
        store.delete(&ada).unwrap();
        assert!(store.get(&ada).unwrap().is_none(), "deleted");
    }

    struct Stuck;
    impl TokenStore for Stuck {
        fn get(&self, _: &TeamAccount) -> Result<Option<TeamToken>, SecretError> {
            loop {
                std::thread::park();
            }
        }
        fn set(&self, _: &TeamAccount, _: &TeamToken) -> Result<(), SecretError> {
            loop {
                std::thread::park();
            }
        }
        fn delete(&self, _: &TeamAccount) -> Result<(), SecretError> {
            loop {
                std::thread::park();
            }
        }
    }

    #[test]
    fn a_store_that_never_answers_fails_within_the_limit_then_at_once() {
        use std::time::{Duration, Instant};
        let store = TimedTokenStore::new(std::sync::Arc::new(Stuck), Duration::from_millis(200));
        let ada = TeamAccount::new(ServerUrl::parse("https://t.example").unwrap(), "ada");
        let started = Instant::now();
        assert!(matches!(
            store.get(&ada),
            Err(SecretError::Unavailable { .. })
        ));
        assert!(started.elapsed() < Duration::from_secs(5), "bounded");
        let again = Instant::now();
        assert!(matches!(
            store.delete(&ada),
            Err(SecretError::Unavailable { .. })
        ));
        assert!(
            again.elapsed() < Duration::from_millis(100),
            "a stuck store fails at once"
        );
    }

    #[test]
    fn a_timed_store_passes_answers_through() {
        let store = TimedTokenStore::new(
            std::sync::Arc::new(MemoryTokenStore::new()),
            std::time::Duration::from_secs(5),
        );
        let ada = TeamAccount::new(ServerUrl::parse("https://t.example").unwrap(), "ada");
        store.set(&ada, &token('C')).unwrap();
        assert_eq!(
            store.get(&ada).unwrap().unwrap().expose(),
            token('C').expose()
        );
        store.delete(&ada).unwrap();
        assert!(store.get(&ada).unwrap().is_none());
    }
}
