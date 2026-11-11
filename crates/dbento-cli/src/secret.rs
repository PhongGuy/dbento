//! The keychain store and `dbento secret set|delete`. The secret never comes from argv.

use std::io::BufRead;
use std::sync::OnceLock;

use anyhow::{Context, bail};
use dbento_core::{
    ConnectError, ConnectionConfig, KeychainStore, Secret, SecretError, SecretKey, SecretSource,
    SecretStore, TimedStore, Workspace,
};

/// The platform keychain, opened on first use so a SQLite run never touches it.
/// If it cannot be opened, every call fails with that error.
#[derive(Default)]
pub(crate) struct LazyKeychain {
    store: OnceLock<Result<KeychainStore, SecretError>>,
}

impl LazyKeychain {
    fn store(&self) -> Result<&KeychainStore, SecretError> {
        // Debug-only seam so a subprocess test can see an unavailable keychain without the
        // platform store being called.
        if cfg!(debug_assertions) && std::env::var_os("DBENTO_TEST_NO_KEYCHAIN").is_some() {
            return Err(SecretError::Unavailable {
                message: "disabled by DBENTO_TEST_NO_KEYCHAIN".into(),
            });
        }
        self.store
            .get_or_init(KeychainStore::new)
            .as_ref()
            .map_err(Clone::clone)
    }
}

impl SecretStore for LazyKeychain {
    fn get(&self, key: &SecretKey) -> Result<Option<Secret>, SecretError> {
        self.store()?.get(key)
    }

    fn set(&self, key: &SecretKey, secret: &Secret) -> Result<(), SecretError> {
        self.store()?.set(key, secret)
    }

    fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        self.store()?.delete(key)
    }
}

/// The keychain with a time limit, so a locked keychain cannot hang the command.
pub(crate) fn store() -> TimedStore<LazyKeychain> {
    TimedStore::new(LazyKeychain::default())
}

/// The connection `name` when it keeps its secret in the keychain; otherwise an error that
/// says why not. Checked before any input is read, so a refused `set` never prompts.
pub(crate) fn keychain_connection<'w>(
    ws: &'w Workspace,
    name: &str,
) -> anyhow::Result<&'w ConnectionConfig> {
    // The name is not echoed: the user may have typed the secret in its place.
    let conn = ws.declared(name).ok_or_else(|| {
        anyhow::anyhow!("no such connection in dbento.toml; see `dbento connections`")
    })?;
    if !conn.engine().capabilities().password {
        let engine = conn.engine().name();
        bail!("connection {name:?} has no secret: a {engine} connection opens without one");
    }
    match conn.secret_source() {
        SecretSource::Keychain => Ok(conn),
        SecretSource::Env(variable) => {
            bail!("connection {name:?} reads its secret from ${variable}, not the keychain")
        }
        _ => bail!("connection {name:?} has secret = \"none\""),
    }
}

/// The connection `name` when it has a `tls_key`, whose passphrase can then be stored.
pub(crate) fn key_connection<'w>(
    ws: &'w Workspace,
    name: &str,
) -> anyhow::Result<&'w ConnectionConfig> {
    let conn = ws.declared(name).ok_or_else(|| {
        anyhow::anyhow!("no such connection in dbento.toml; see `dbento connections`")
    })?;
    if conn.tls_key().is_none() {
        bail!("connection {name:?} has no tls_key, so there is no key passphrase");
    }
    Ok(conn)
}

/// Stores `secret` as the `tls_key` passphrase of connection `name`.
pub(crate) fn set_key_passphrase(
    ws: &Workspace,
    name: &str,
    store: &dyn SecretStore,
    secret: &Secret,
) -> anyhow::Result<()> {
    store.set(&key_passphrase_target(ws, name)?, secret)?;
    Ok(())
}

/// Removes the stored `tls_key` passphrase of connection `name`.
pub(crate) fn delete_key_passphrase(
    ws: &Workspace,
    name: &str,
    store: &dyn SecretStore,
) -> anyhow::Result<()> {
    store.delete(&key_passphrase_target(ws, name)?)?;
    Ok(())
}

/// The password key of keychain connection `name`, from the declared-only target.
fn secret_target(ws: &Workspace, name: &str) -> anyhow::Result<SecretKey> {
    keychain_connection(ws, name)?;
    ws.secret_target(name)
        .context("no such connection in dbento.toml; see `dbento connections`")
}

/// The `tls_key` passphrase key of connection `name`, from the declared-only target.
fn key_passphrase_target(ws: &Workspace, name: &str) -> anyhow::Result<SecretKey> {
    key_connection(ws, name)?;
    ws.key_passphrase_target(name)
        .context("no such connection in dbento.toml; see `dbento connections`")
}

/// Where a missing secret can be supplied; see `dbento_core::secret_hint`.
pub(crate) fn connect_hint(
    ws: &Workspace,
    conn: &ConnectionConfig,
    store: &dyn SecretStore,
) -> Option<String> {
    dbento_core::secret_hint(ws, conn, store)
}

/// The connect error alone, or `"{e}\nhint: {hint}"`.
pub(crate) fn with_hint(e: ConnectError, hint: Option<String>) -> anyhow::Error {
    match hint {
        Some(h) => anyhow::anyhow!("{e}\nhint: {h}"),
        None => anyhow::Error::new(e),
    }
}

/// The first line of `input` without its line ending; an empty secret is refused.
pub(crate) fn read_secret(input: &mut dyn BufRead) -> anyhow::Result<Secret> {
    let mut line = String::new();
    input
        .read_line(&mut line)
        .context("cannot read the secret")?;
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() {
        bail!("the secret is empty");
    }
    Ok(Secret::new(line))
}

/// Stores `secret` for the keychain connection `name`.
pub(crate) fn set(
    ws: &Workspace,
    name: &str,
    store: &dyn SecretStore,
    secret: &Secret,
) -> anyhow::Result<()> {
    store.set(&secret_target(ws, name)?, secret)?;
    Ok(())
}

/// Removes the stored secret of the keychain connection `name`.
pub(crate) fn delete(ws: &Workspace, name: &str, store: &dyn SecretStore) -> anyhow::Result<()> {
    store.delete(&secret_target(ws, name)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbento_core::{MemoryStore, SecretKey};

    const TOML: &str = "dbento = 1\n[connections.kc]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\n[connections.none]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\nsecret = \"none\"\n[connections.lite]\nengine = \"sqlite\"\npath = \"x.db\"\n";

    fn ws() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("dbento.toml"), TOML).unwrap();
        let ws = Workspace::open(dir.path()).unwrap();
        (dir, ws)
    }

    #[test]
    fn set_and_delete_use_the_connection_key() {
        let (_d, ws) = ws();
        let store = MemoryStore::new();
        let secret = read_secret(&mut "s3cret\r\nignored\n".as_bytes()).unwrap();
        set(&ws, "kc", &store, &secret).unwrap();
        let key = SecretKey::for_connection(&ws, ws.declared("kc").unwrap());
        assert_eq!(store.get(&key).unwrap().unwrap().expose(), "s3cret");
        delete(&ws, "kc", &store).unwrap();
        assert!(store.get(&key).unwrap().is_none());
    }

    #[test]
    fn the_key_passphrase_has_its_own_slot_and_needs_a_tls_key() {
        let dir = tempfile::tempdir().unwrap();
        let toml = format!(
            "{TOML}[connections.cert]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\nsecret = \"none\"\ntls = \"verify\"\ntls_cert = \"c.crt\"\ntls_key = \"c.key\"\n"
        );
        std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
        let ws = Workspace::open(dir.path()).unwrap();
        let store = MemoryStore::new();
        let secret = read_secret(&mut "pp\n".as_bytes()).unwrap();
        set_key_passphrase(&ws, "cert", &store, &secret).unwrap();
        let conn = ws.declared("cert").unwrap();
        let key = SecretKey::key_passphrase_for(&ws, conn);
        assert_eq!(store.get(&key).unwrap().unwrap().expose(), "pp");
        assert!(
            store
                .get(&SecretKey::for_connection(&ws, conn))
                .unwrap()
                .is_none()
        );
        delete_key_passphrase(&ws, "cert", &store).unwrap();
        assert!(store.get(&key).unwrap().is_none());
        for name in ["kc", "lite", "missing"] {
            assert!(key_connection(&ws, name).is_err(), "{name}");
        }
    }

    #[test]
    fn empty_input_and_non_keychain_connections_are_refused() {
        let (_d, ws) = ws();
        assert!(read_secret(&mut "\n".as_bytes()).is_err());
        assert!(read_secret(&mut "".as_bytes()).is_err());
        for name in ["none", "lite", "missing"] {
            assert!(keychain_connection(&ws, name).is_err(), "{name}");
        }
    }

    struct Locked;
    impl SecretStore for Locked {
        fn get(&self, _: &SecretKey) -> Result<Option<Secret>, SecretError> {
            Err(SecretError::Unavailable {
                message: "locked".into(),
            })
        }
        fn set(&self, _: &SecretKey, _: &Secret) -> Result<(), SecretError> {
            unreachable!()
        }
        fn delete(&self, _: &SecretKey) -> Result<(), SecretError> {
            unreachable!()
        }
    }

    #[test]
    fn a_keychain_connection_hints_at_both_secret_places() {
        let (_d, ws) = ws();
        let hint =
            connect_hint(&ws, ws.declared("kc").unwrap(), &MemoryStore::new()).expect("a hint");
        assert!(hint.contains("dbento secret set kc"), "{hint}");
        assert!(hint.contains("DBENTO_SECRET_KC"), "{hint}");
        for name in ["none", "lite"] {
            assert!(
                connect_hint(&ws, ws.declared(name).unwrap(), &MemoryStore::new()).is_none(),
                "{name}"
            );
        }
    }

    #[test]
    fn with_hint_appends_the_hint_on_its_own_line() {
        let e = || dbento_core::ConnectError::Open {
            message: "refused".into(),
        };
        assert_eq!(
            with_hint(e(), Some("set it".into())).to_string(),
            "cannot open the database: refused\nhint: set it"
        );
        assert_eq!(
            with_hint(e(), None).to_string(),
            "cannot open the database: refused"
        );
    }

    #[test]
    fn the_hint_says_why_the_keychain_was_unavailable() {
        let (_d, ws) = ws();
        let hint = connect_hint(&ws, ws.declared("kc").unwrap(), &Locked).unwrap();
        assert!(
            hint.contains("the keychain is unavailable: locked"),
            "{hint}"
        );
    }

    #[test]
    fn only_an_engine_that_connects_with_a_password_keeps_a_keychain_secret() {
        let dir = tempfile::tempdir().unwrap();
        let toml =
            format!("{TOML}[connections.my]\nengine = \"mysql\"\nhost = \"h\"\nuser = \"u\"\n");
        std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
        let ws = Workspace::open(dir.path()).unwrap();
        let store = MemoryStore::new();
        let secret = read_secret(&mut "pw\n".as_bytes()).unwrap();
        set(&ws, "my", &store, &secret).unwrap();
        let key = SecretKey::for_connection(&ws, ws.declared("my").unwrap());
        assert_eq!(store.get(&key).unwrap().unwrap().expose(), "pw");
        assert_eq!(
            keychain_connection(&ws, "lite").unwrap_err().to_string(),
            "connection \"lite\" has no secret: a sqlite connection opens without one"
        );
    }

    const PARENT: &str = "dbento = 1\n[connections.kc]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\ntls = \"verify\"\ntls_cert = \"c.crt\"\ntls_key = \"c.key\"\ndatabases = [\"app\"]\n";

    #[test]
    fn a_child_never_reaches_the_keychain() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("dbento.toml"), PARENT).unwrap();
        let ws = Workspace::open(dir.path()).unwrap();
        let secret = read_secret(&mut "pw\n".as_bytes()).unwrap();
        // `Locked` panics on set and delete, so a refusal here provably never touched the store.
        for name in ["kc/app", "kc/other"] {
            assert!(keychain_connection(&ws, name).is_err(), "{name}");
            assert!(key_connection(&ws, name).is_err(), "{name}");
            assert!(set(&ws, name, &Locked, &secret).is_err(), "{name}");
            assert!(delete(&ws, name, &Locked).is_err(), "{name}");
            assert!(
                set_key_passphrase(&ws, name, &Locked, &secret).is_err(),
                "{name}"
            );
            assert!(delete_key_passphrase(&ws, name, &Locked).is_err(), "{name}");
        }
        assert!(keychain_connection(&ws, "kc").is_ok());
        assert!(key_connection(&ws, "kc").is_ok());
    }
}
