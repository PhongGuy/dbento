//! [`KeychainStore`]: secrets in the platform keychain through `keyring-core`, one entry per
//! connection under service `dbento`, the account being [`SecretKey`]'s two parts.

use crate::secrets::{Secret, SecretError, SecretKey, SecretStore, Slot};
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
        other => other.to_string(),
    };
    match error {
        Error::NoStorageAccess(_) | Error::PlatformFailure(_) | Error::NoDefaultStore => {
            SecretError::Unavailable { message }
        }
        _ => SecretError::Failed { message },
    }
}

impl fmt::Debug for KeychainStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("KeychainStore { .. }")
    }
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
}
