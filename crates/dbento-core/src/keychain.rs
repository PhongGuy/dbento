//! [`KeychainStore`]: secrets in the platform keychain through `keyring-core`, one entry per
//! connection under service `dbento`, the account being [`SecretKey`]'s two parts. With the
//! `team` feature, `KeychainTokenStore` keeps team session tokens under the same service.

use crate::secrets::{Secret, SecretError, SecretKey, SecretStore};
#[cfg(feature = "team")]
use crate::team::{TeamAccount, TeamToken, TokenStore};
use dbento_workspace::Slot;
use keyring_core::{CredentialStore, Entry, Error};
use std::fmt;
use std::sync::Arc;

const SERVICE: &str = "dbento";

/// The platform keychain: Secret Service on Linux, the login keychain on macOS and the
/// Credential Manager on Windows.
#[non_exhaustive]
pub struct KeychainStore {
    store: Arc<CredentialStore>,
}

impl KeychainStore {
    /// Open the platform keychain. Fails with [`SecretError::Unavailable`] on a platform without
    /// one or when it cannot be reached.
    pub fn new() -> Result<KeychainStore, SecretError> {
        let store = platform_store().map_err(map_error)?;
        Ok(KeychainStore { store })
    }

    fn entry(&self, key: &SecretKey) -> Result<Entry, SecretError> {
        self.store
            .build(SERVICE, &account(key), None)
            .map_err(map_error)
    }
}

#[cfg(target_os = "linux")]
fn platform_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Ok(zbus_secret_service_keyring_store::Store::new()?)
}

#[cfg(target_os = "macos")]
fn platform_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Ok(apple_native_keyring_store::keychain::Store::new()?)
}

#[cfg(windows)]
fn platform_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Ok(windows_native_keyring_store::Store::new()?)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn platform_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Err(Error::NoDefaultStore)
}

impl SecretStore for KeychainStore {
    fn get(&self, key: &SecretKey) -> Result<Option<Secret>, SecretError> {
        match self.entry(key)?.get_password() {
            Ok(value) => Ok(Some(Secret::new(value))),
            Err(Error::NoEntry) => Ok(None),
            Err(e) => Err(map_error(e)),
        }
    }

    fn set(&self, key: &SecretKey, secret: &Secret) -> Result<(), SecretError> {
        self.entry(key)?
            .set_password(secret.expose())
            .map_err(map_error)
    }

    fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(e) => Err(map_error(e)),
        }
    }
}

/// The keychain account for `key`: a JSON array of workspace directory and connection, the path
/// with forward slashes so one workspace has one account on every OS.
fn account(key: &SecretKey) -> String {
    let path = key.workspace_dir().to_string_lossy().replace('\\', "/");
    match key.slot() {
        Slot::Password => serde_json::json!([path, key.connection()]).to_string(),
        Slot::KeyPassphrase => serde_json::json!([path, key.connection(), "tls_key"]).to_string(),
    }
}

/// Some `keyring_core` errors carry the stored bytes, so those get a fixed message instead of
/// their own text.
fn map_error(error: Error) -> SecretError {
    let message = match &error {
        Error::BadEncoding(_) => "the stored value is not UTF-8".to_owned(),
        Error::BadDataFormat(..) => "the stored value is malformed".to_owned(),
        Error::Ambiguous(items) => format!("{} entries match", items.len()),
        other @ (Error::PlatformFailure(_)
        | Error::NoStorageAccess(_)
        | Error::NoEntry
        | Error::BadStoreFormat(_)
        | Error::TooLong(..)
        | Error::Invalid(..)
        | Error::NoDefaultStore
        | Error::NotSupportedByStore(_)
        | _) => other.to_string(),
    };
    match error {
        Error::NoStorageAccess(_) | Error::PlatformFailure(_) | Error::NoDefaultStore => {
            SecretError::Unavailable { message }
        }
        Error::NoEntry
        | Error::BadEncoding(_)
        | Error::BadDataFormat(..)
        | Error::BadStoreFormat(_)
        | Error::TooLong(..)
        | Error::Invalid(..)
        | Error::Ambiguous(_)
        | Error::NotSupportedByStore(_)
        | _ => SecretError::Failed { message },
    }
}

impl fmt::Debug for KeychainStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KeychainStore { .. }")
    }
}

/// Team session tokens in the platform keychain, one entry per account under service `dbento`.
#[cfg(feature = "team")]
#[non_exhaustive]
pub struct KeychainTokenStore {
    store: Arc<CredentialStore>,
}

#[cfg(feature = "team")]
impl KeychainTokenStore {
    /// Open the platform keychain. Fails with [`SecretError::Unavailable`] on a platform without
    /// one or when it cannot be reached.
    pub fn new() -> Result<KeychainTokenStore, SecretError> {
        let store = platform_store().map_err(map_error)?;
        Ok(KeychainTokenStore { store })
    }

    fn entry(&self, account: &TeamAccount) -> Result<Entry, SecretError> {
        self.store
            .build(SERVICE, &token_account(account), None)
            .map_err(map_error)
    }
}

#[cfg(feature = "team")]
impl TokenStore for KeychainTokenStore {
    fn get(&self, account: &TeamAccount) -> Result<Option<TeamToken>, SecretError> {
        match self.entry(account)?.get_password() {
            Ok(value) => Ok(TeamToken::parse(&value)),
            Err(Error::NoEntry) => Ok(None),
            Err(e) => Err(map_error(e)),
        }
    }

    fn set(&self, account: &TeamAccount, token: &TeamToken) -> Result<(), SecretError> {
        self.entry(account)?
            .set_password(token.expose())
            .map_err(map_error)
    }

    fn delete(&self, account: &TeamAccount) -> Result<(), SecretError> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(e) => Err(map_error(e)),
        }
    }
}

#[cfg(feature = "team")]
impl fmt::Debug for KeychainTokenStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KeychainTokenStore { .. }")
    }
}

/// The keychain account for a team token: `team`, the server and the user as a JSON array, so
/// it can never collide with a connection's two-part account.
#[cfg(feature = "team")]
fn token_account(account: &TeamAccount) -> String {
    serde_json::json!(["team", account.server.as_str(), account.username]).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_account_is_a_json_array_with_forward_slashes() {
        assert_eq!(
            account(&SecretKey::new(r"C:\work\db", "prod")),
            r#"["C:/work/db","prod"]"#
        );
        assert_eq!(
            account(&SecretKey::new("/home/u/w", "a-b")),
            r#"["/home/u/w","a-b"]"#
        );
        assert_eq!(
            account(&SecretKey::key_passphrase("/home/u/w", "a-b")),
            r#"["/home/u/w","a-b","tls_key"]"#
        );
    }

    #[cfg(feature = "team")]
    #[test]
    fn a_team_token_account_is_a_json_array_tagged_team() {
        let a = crate::TeamAccount::new(
            crate::ServerUrl::parse("https://t.example/p").unwrap(),
            "ada",
        );
        assert_eq!(token_account(&a), r#"["team","https://t.example/p","ada"]"#);
    }
}
