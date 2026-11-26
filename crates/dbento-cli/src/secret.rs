//! The keychain store and `dbento secret set|delete|pin`. The secret never comes from argv.

use std::io::BufRead;
use std::sync::OnceLock;

use anyhow::{Context, bail};
use dbento_core::{
    Adopted, ConnectError, ConnectionConfig, KeychainStore, Secret, SecretError, SecretKey,
    SecretSource, SecretStore, SshHome, TimedStore, Workspace, resolve_ssh,
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

/// The home whose `~/.ssh` resolves tunnels. Debug builds take `DBENTO_TEST_SSH_HOME` (user
/// "tester") instead, so a subprocess test never reads the real `~/.ssh`.
pub(crate) fn ssh_home() -> Option<SshHome> {
    if cfg!(debug_assertions)
        && let Some(dir) = std::env::var_os("DBENTO_TEST_SSH_HOME")
    {
        return Some(SshHome::new(dir, "tester"));
    }
    SshHome::from_env()
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

/// The connection `name` when it has an `ssh` table, whose secrets can then be stored.
pub(crate) fn ssh_connection<'w>(
    ws: &'w Workspace,
    name: &str,
) -> anyhow::Result<&'w ConnectionConfig> {
    let conn = ws.declared(name).ok_or_else(|| {
        anyhow::anyhow!("no such connection in dbento.toml; see `dbento connections`")
    })?;
    if conn.ssh().is_none() {
        bail!("connection {name:?} has no ssh table, so there is no SSH secret");
    }
    Ok(conn)
}

/// The `user@host:port` the SSH password of `name` is pinned to, resolved through `home`'s
/// `~/.ssh/config`; a construct dbento cannot honour refuses, so nothing is stored blind.
pub(crate) fn ssh_login(ws: &Workspace, name: &str, home: &SshHome) -> anyhow::Result<String> {
    Ok(resolve_ssh(ssh_connection(ws, name)?, ws.dir(), home)?.login())
}

/// Stores `secret` as the SSH password of connection `name`, pinned to its resolved login.
pub(crate) fn set_ssh_password(
    ws: &Workspace,
    name: &str,
    home: &SshHome,
    store: &dyn SecretStore,
    secret: &Secret,
) -> anyhow::Result<()> {
    let login = ssh_login(ws, name, home)?;
    store.set(&ssh_password_target(ws, name, &login)?, secret)?;
    Ok(())
}

/// Removes the SSH password of connection `name`. When `~/.ssh/config` cannot be resolved, a
/// dbento.toml that writes both `user` and `port` still names the login; otherwise nothing is
/// touched and the error names the entry's account shape so it can be removed by hand.
pub(crate) fn delete_ssh_password(
    ws: &Workspace,
    name: &str,
    home: &SshHome,
    store: &dyn SecretStore,
) -> anyhow::Result<()> {
    let conn = ssh_connection(ws, name)?;
    let login = match resolve_ssh(conn, ws.dir(), home) {
        Ok(resolved) => resolved.login(),
        Err(e) => {
            let ssh = conn.ssh().context("the connection has no ssh table")?;
            match (ssh.user(), ssh.port()) {
                (Some(user), Some(port)) => format!("{user}@{}:{port}", ssh.host()),
                _ => bail!(
                    "cannot locate the SSH password of {name:?}: {e}\nIts keychain entry is service `dbento`, account [{:?}, {:?}, \"ssh_password\", \"<user>@<host>:<port>\"]; set user and port in the connection's ssh table so dbento can find it",
                    ws.dir().display().to_string(),
                    conn.secret_owner()
                ),
            }
        }
    };
    store.delete(&ssh_password_target(ws, name, &login)?)?;
    Ok(())
}

/// Stores `secret` as the passphrase of the SSH `identity_file` of connection `name`.
pub(crate) fn set_ssh_key_passphrase(
    ws: &Workspace,
    name: &str,
    store: &dyn SecretStore,
    secret: &Secret,
) -> anyhow::Result<()> {
    store.set(&ssh_key_passphrase_target(ws, name)?, secret)?;
    Ok(())
}

/// Removes the stored SSH `identity_file` passphrase of connection `name`.
pub(crate) fn delete_ssh_key_passphrase(
    ws: &Workspace,
    name: &str,
    store: &dyn SecretStore,
) -> anyhow::Result<()> {
    store.delete(&ssh_key_passphrase_target(ws, name)?)?;
    Ok(())
}

/// The SSH password key of connection `name` logging in as `login`.
fn ssh_password_target(ws: &Workspace, name: &str, login: &str) -> anyhow::Result<SecretKey> {
    ws.ssh_password_target(name, login)
        .context("no such connection in dbento.toml; see `dbento connections`")
}

/// The SSH key passphrase key of connection `name`, refused without an `ssh` table.
fn ssh_key_passphrase_target(ws: &Workspace, name: &str) -> anyhow::Result<SecretKey> {
    ssh_connection(ws, name)?;
    ws.ssh_key_passphrase_target(name)
        .context("no such connection in dbento.toml; see `dbento connections`")
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

/// Stores `secret` for the keychain connection `name`, pinned to its current target, then
/// drops any entry saved under the bare name so it can never be sent elsewhere.
pub(crate) fn set(
    ws: &Workspace,
    name: &str,
    store: &dyn SecretStore,
    secret: &Secret,
) -> anyhow::Result<()> {
    store.set(&secret_target(ws, name)?, secret)?;
    store.delete(&SecretKey::legacy_password(ws.dir(), name))?;
    Ok(())
}

/// Removes the stored secret of the keychain connection `name` for its current target and
/// under the bare name. Both deletes are tried; the first failure is the error.
pub(crate) fn delete(ws: &Workspace, name: &str, store: &dyn SecretStore) -> anyhow::Result<()> {
    let pinned = store.delete(&secret_target(ws, name)?);
    let legacy = store.delete(&SecretKey::legacy_password(ws.dir(), name));
    pinned?;
    legacy?;
    Ok(())
}

/// What `secret pin` shows, and whether a person must still answer.
#[derive(Debug)]
pub(crate) struct PinPlan {
    pub(crate) target: String,
    pub(crate) ask: bool,
}

/// Refuses a name without a keychain password, a missing legacy entry, and (when neither
/// `yes` nor `terminal`) any confirmation at all; never writes.
pub(crate) fn plan_pin(
    ws: &Workspace,
    name: &str,
    store: &dyn SecretStore,
    yes: bool,
    terminal: bool,
) -> anyhow::Result<PinPlan> {
    let conn = keychain_connection(ws, name)?;
    let target = pin_target(conn);
    if store
        .get(&SecretKey::legacy_password(ws.dir(), name))?
        .is_none()
    {
        bail!(
            "connection {name:?} has no password saved before pinning, so there is nothing to pin"
        );
    }
    if !yes && !terminal {
        bail!(
            "no terminal to confirm that the saved password of {name:?} may be sent to {target}; check the target, then pass --yes"
        );
    }
    Ok(PinPlan { target, ask: !yes })
}

/// Moves the legacy password of `name` to the key of its current target.
pub(crate) fn pin(ws: &Workspace, name: &str, store: &dyn SecretStore) -> anyhow::Result<Adopted> {
    keychain_connection(ws, name)?;
    Ok(dbento_core::adopt_legacy_password(store, ws, name)?)
}

/// `<engine> <target>`, plus ` via <ssh host>[:<port>]` with the port only when written.
fn pin_target(conn: &ConnectionConfig) -> String {
    let mut target = format!("{} {}", conn.engine_label(), conn.display_target());
    if let Some(ssh) = conn.ssh() {
        target.push_str(" via ");
        target.push_str(ssh.host());
        if let Some(port) = ssh.port() {
            target.push_str(&format!(":{port}"));
        }
    }
    target
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbento_core::Adopted;
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

    const TUN: &str = "[connections.tun]\nengine = \"postgres\"\nhost = \"db\"\ndatabase = \"d\"\nuser = \"u\"\nsecret = \"none\"\n[connections.tun.ssh]\nhost = \"bastion\"\nuser = \"ops\"\n";

    #[test]
    fn ssh_secrets_have_their_own_slots_and_need_an_ssh_table() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("dbento.toml"), format!("{TOML}{TUN}")).unwrap();
        let ws = Workspace::open(dir.path()).unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        let home = dbento_core::SshHome::new(home_dir.path(), "tester");
        let store = MemoryStore::new();
        let pw = read_secret(&mut "sshpw\n".as_bytes()).unwrap();
        let pp = read_secret(&mut "sshpp\n".as_bytes()).unwrap();
        set_ssh_password(&ws, "tun", &home, &store, &pw).unwrap();
        set_ssh_key_passphrase(&ws, "tun", &store, &pp).unwrap();
        let conn = ws.declared("tun").unwrap();
        let pw_key = SecretKey::ssh_password_for(&ws, conn, "ops@bastion:22");
        let pp_key = SecretKey::ssh_key_passphrase_for(&ws, conn);
        assert_eq!(store.get(&pw_key).unwrap().unwrap().expose(), "sshpw");
        assert_eq!(store.get(&pp_key).unwrap().unwrap().expose(), "sshpp");
        assert!(
            store
                .get(&SecretKey::for_connection(&ws, conn))
                .unwrap()
                .is_none()
        );
        delete_ssh_password(&ws, "tun", &home, &store).unwrap();
        delete_ssh_key_passphrase(&ws, "tun", &store).unwrap();
        assert!(store.get(&pw_key).unwrap().is_none());
        assert!(store.get(&pp_key).unwrap().is_none());
        // `Locked` panics on set and delete, so each refusal provably never touched the store.
        for name in ["kc", "lite", "missing"] {
            assert!(ssh_connection(&ws, name).is_err(), "{name}");
            assert!(
                set_ssh_password(&ws, name, &home, &Locked, &pw).is_err(),
                "{name}"
            );
            assert!(
                set_ssh_key_passphrase(&ws, name, &Locked, &pp).is_err(),
                "{name}"
            );
            assert!(
                delete_ssh_password(&ws, name, &home, &Locked).is_err(),
                "{name}"
            );
            assert!(
                delete_ssh_key_passphrase(&ws, name, &Locked).is_err(),
                "{name}"
            );
        }
        assert_eq!(
            ssh_connection(&ws, "kc").unwrap_err().to_string(),
            "connection \"kc\" has no ssh table, so there is no SSH secret"
        );
        assert_eq!(
            ssh_connection(&ws, "missing").unwrap_err().to_string(),
            "no such connection in dbento.toml; see `dbento connections`"
        );
    }

    /// A workspace holding `TOML` plus `tun_ssh` as tun's ssh table, and a home whose
    /// `.ssh/config` sends `bastion` through a ProxyJump, so resolution always fails.
    fn unresolvable(
        tun_ssh: &str,
    ) -> (
        tempfile::TempDir,
        Workspace,
        tempfile::TempDir,
        dbento_core::SshHome,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let tun = format!(
            "[connections.tun]\nengine = \"postgres\"\nhost = \"db\"\ndatabase = \"d\"\nuser = \"u\"\n[connections.tun.ssh]\n{tun_ssh}"
        );
        std::fs::write(dir.path().join("dbento.toml"), format!("{TOML}{tun}")).unwrap();
        let ws = Workspace::open(dir.path()).unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home_dir.path().join(".ssh")).unwrap();
        std::fs::write(
            home_dir.path().join(".ssh/config"),
            "Host bastion\n  ProxyJump hop\n",
        )
        .unwrap();
        let home = dbento_core::SshHome::new(home_dir.path(), "tester");
        (dir, ws, home_dir, home)
    }

    #[test]
    fn delete_falls_back_to_a_complete_dbento_toml_login() {
        let (_d, ws, _h, home) = unresolvable("host = \"bastion\"\nport = 2222\nuser = \"ops\"\n");
        let store = MemoryStore::new();
        let conn = ws.declared("tun").unwrap();
        let key = SecretKey::ssh_password_for(&ws, conn, "ops@bastion:2222");
        store.set(&key, &Secret::new("pw")).unwrap();
        delete_ssh_password(&ws, "tun", &home, &store).unwrap();
        assert!(store.get(&key).unwrap().is_none());
    }

    #[test]
    fn delete_without_a_complete_login_names_the_entry_it_cannot_find() {
        let (_d, ws, _h, home) = unresolvable("host = \"bastion\"\n");
        let e = delete_ssh_password(&ws, "tun", &home, &Locked)
            .unwrap_err()
            .to_string();
        assert!(e.contains("ProxyJump"), "{e}");
        assert!(
            e.contains("\"ssh_password\""),
            "the account shape is named: {e}"
        );
    }

    #[test]
    fn an_ssh_config_refusal_stops_the_password_before_the_store() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("dbento.toml"), format!("{TOML}{TUN}")).unwrap();
        let ws = Workspace::open(dir.path()).unwrap();
        let home_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home_dir.path().join(".ssh")).unwrap();
        std::fs::write(
            home_dir.path().join(".ssh/config"),
            "Host bastion\n  ProxyJump hop\n",
        )
        .unwrap();
        let home = dbento_core::SshHome::new(home_dir.path(), "tester");
        let pw = read_secret(&mut "sshpw\n".as_bytes()).unwrap();
        let e = set_ssh_password(&ws, "tun", &home, &Locked, &pw).unwrap_err();
        assert!(e.to_string().contains("ProxyJump"), "{e}");
    }

    fn legacy(ws: &Workspace, name: &str) -> SecretKey {
        SecretKey::legacy_password(ws.dir(), name)
    }

    fn pinned(ws: &Workspace, name: &str) -> SecretKey {
        SecretKey::for_connection(ws, ws.declared(name).unwrap())
    }

    #[test]
    fn set_replaces_the_legacy_entry_and_delete_removes_both() {
        let (_d, ws) = ws();
        let store = MemoryStore::new();
        store.set(&legacy(&ws, "kc"), &Secret::new("x")).unwrap();
        set(&ws, "kc", &store, &Secret::new("x")).unwrap();
        assert_eq!(
            store.get(&pinned(&ws, "kc")).unwrap().unwrap().expose(),
            "x"
        );
        assert!(
            store.get(&legacy(&ws, "kc")).unwrap().is_none(),
            "set drops the legacy entry"
        );
        store.set(&legacy(&ws, "kc"), &Secret::new("x")).unwrap();
        delete(&ws, "kc", &store).unwrap();
        assert!(
            store.get(&pinned(&ws, "kc")).unwrap().is_none(),
            "the pinned entry is gone"
        );
        assert!(
            store.get(&legacy(&ws, "kc")).unwrap().is_none(),
            "the legacy entry is gone"
        );
    }

    #[test]
    fn pin_without_a_terminal_or_yes_refuses_and_writes_nothing() {
        let (_d, ws) = ws();
        let store = MemoryStore::new();
        store.set(&legacy(&ws, "kc"), &Secret::new("x")).unwrap();
        let e = plan_pin(&ws, "kc", &store, false, false)
            .unwrap_err()
            .to_string();
        assert!(e.contains("--yes"), "{e}");
        assert!(
            e.contains("postgres u@h:5432/d"),
            "the refusal names the target: {e}"
        );
        assert!(
            store.get(&legacy(&ws, "kc")).unwrap().is_some(),
            "the legacy entry stays"
        );
        assert!(
            store.get(&pinned(&ws, "kc")).unwrap().is_none(),
            "nothing was pinned"
        );
    }

    #[test]
    fn pin_asks_at_a_terminal_and_not_with_yes() {
        let (_d, ws) = ws();
        let store = MemoryStore::new();
        store.set(&legacy(&ws, "kc"), &Secret::new("x")).unwrap();
        let asked = plan_pin(&ws, "kc", &store, false, true).unwrap();
        assert!(asked.ask, "a terminal is asked");
        assert_eq!(asked.target, "postgres u@h:5432/d");
        assert!(
            !plan_pin(&ws, "kc", &store, true, false).unwrap().ask,
            "--yes answers"
        );
    }

    #[test]
    fn pin_moves_the_legacy_entry_to_the_current_target() {
        let (_d, ws) = ws();
        let store = MemoryStore::new();
        store.set(&legacy(&ws, "kc"), &Secret::new("x")).unwrap();
        assert_eq!(pin(&ws, "kc", &store).unwrap(), Adopted::Moved);
        assert_eq!(
            store.get(&pinned(&ws, "kc")).unwrap().unwrap().expose(),
            "x"
        );
        assert!(store.get(&legacy(&ws, "kc")).unwrap().is_none());
    }

    #[test]
    fn pin_without_a_legacy_entry_says_so() {
        let (_d, ws) = ws();
        let e = plan_pin(&ws, "kc", &MemoryStore::new(), true, true)
            .unwrap_err()
            .to_string();
        assert!(e.contains("nothing to pin"), "{e}");
    }

    #[test]
    fn pin_refuses_connections_without_a_keychain_password() {
        let (_d, ws) = ws();
        // `Locked` cannot set or delete, so each refusal provably wrote nothing.
        for name in ["none", "lite", "missing"] {
            assert!(plan_pin(&ws, name, &Locked, true, true).is_err(), "{name}");
        }
    }

    #[test]
    fn the_pin_target_names_the_tunnel() {
        let dir = tempfile::tempdir().unwrap();
        let toml = "dbento = 1\n[connections.tun]\nengine = \"postgres\"\nhost = \"db\"\ndatabase = \"d\"\nuser = \"u\"\n[connections.tun.ssh]\nhost = \"bastion\"\nport = 2222\n";
        std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
        let ws = Workspace::open(dir.path()).unwrap();
        let store = MemoryStore::new();
        store.set(&legacy(&ws, "tun"), &Secret::new("x")).unwrap();
        let plan = plan_pin(&ws, "tun", &store, true, false).unwrap();
        assert_eq!(plan.target, "postgres u@db:5432/d via bastion:2222");
    }
}
