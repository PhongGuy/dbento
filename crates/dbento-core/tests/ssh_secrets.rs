//! The SSH password and key passphrase: pinned keychain slots, env fallbacks and resolution.
use dbento_core::{
    ConnectionConfig, MemoryStore, ResolvedSsh, Secret, SecretError, SecretKey, SecretStore,
    SessionOptions, SshHome, Workspace, resolve_ssh, resolve_ssh_secrets,
    ssh_key_passphrase_env_var, ssh_password_env_var,
};
use std::path::{Path, PathBuf};

fn ws_with(jump: &str) -> Workspace {
    let pg = |name: &str| {
        format!(
            "[connections.{name}]\nengine = \"postgres\"\nhost = \"db\"\ndatabase = \"d\"\nuser = \"u\"\n"
        )
    };
    let text = format!(
        "dbento = 1\n{}[connections.tun.ssh]\nhost = \"{jump}\"\nuser = \"ops\"\n{}{}databases = [\"app\"]\n[connections.kids.ssh]\nhost = \"jump\"\nuser = \"ops\"\n",
        pg("tun"),
        pg("plain"),
        pg("kids"),
    );
    Workspace::from_text(PathBuf::from("/w"), Path::new("/w/dbento.toml"), &text).unwrap()
}

fn ws() -> Workspace {
    ws_with("jump")
}

/// `conn`'s ssh table resolved against an empty temporary home.
fn resolved(conn: &ConnectionConfig) -> ResolvedSsh {
    let home = tempfile::tempdir().unwrap();
    resolve_ssh(conn, Path::new("/w"), &SshHome::new(home.path(), "tester")).unwrap()
}

/// A store that fails the test when anything asks it.
struct Untouchable;
impl SecretStore for Untouchable {
    fn get(&self, key: &SecretKey) -> Result<Option<Secret>, SecretError> {
        panic!("the store was asked for {}", key.connection())
    }
    fn set(&self, _: &SecretKey, _: &Secret) -> Result<(), SecretError> {
        unreachable!()
    }
    fn delete(&self, _: &SecretKey) -> Result<(), SecretError> {
        unreachable!()
    }
}

/// A store that answers every read with the same error.
struct Answering(SecretError);
impl SecretStore for Answering {
    fn get(&self, _: &SecretKey) -> Result<Option<Secret>, SecretError> {
        Err(self.0.clone())
    }
    fn set(&self, _: &SecretKey, _: &Secret) -> Result<(), SecretError> {
        unreachable!()
    }
    fn delete(&self, _: &SecretKey) -> Result<(), SecretError> {
        unreachable!()
    }
}

fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
    move |name| {
        pairs
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| (*v).to_owned())
    }
}

fn no_env(_: &str) -> Option<String> {
    None
}

#[test]
fn the_env_fallbacks_are_named_like_the_others() {
    assert_eq!(
        ssh_password_env_var("prod-db"),
        "DBENTO_SSH_PASSWORD_PROD_DB"
    );
    assert_eq!(
        ssh_key_passphrase_env_var("prod-db"),
        "DBENTO_SSH_KEY_PASSPHRASE_PROD_DB"
    );
}

#[test]
fn the_slots_are_distinct_and_a_rename_keeps_the_login() {
    let keys = [
        SecretKey::new("/w", "tun"),
        SecretKey::key_passphrase("/w", "tun"),
        SecretKey::ssh_password("/w", "tun", "ops@jump:22"),
        SecretKey::ssh_password("/w", "tun", "ops@jump2:22"),
        SecretKey::ssh_key_passphrase("/w", "tun"),
    ];
    for (i, a) in keys.iter().enumerate() {
        for b in &keys[i + 1..] {
            assert_ne!(a, b);
        }
    }
    assert_eq!(
        keys[2].renamed("t2"),
        SecretKey::ssh_password("/w", "t2", "ops@jump:22")
    );
    assert_eq!(keys[2].ssh_login(), Some("ops@jump:22"));
    assert_eq!(
        keys[4].renamed("t2"),
        SecretKey::ssh_key_passphrase("/w", "t2")
    );
}

#[test]
fn ssh_targets_exist_only_for_declared_connections() {
    let ws = ws();
    let conn = ws.declared("tun").unwrap();
    assert_eq!(
        ws.ssh_password_target("tun", "ops@jump:22"),
        Some(SecretKey::ssh_password_for(&ws, conn, "ops@jump:22"))
    );
    assert_eq!(
        ws.ssh_key_passphrase_target("tun"),
        Some(SecretKey::ssh_key_passphrase_for(&ws, conn))
    );
    for name in ["kids/app", "missing"] {
        assert!(
            ws.ssh_password_target(name, "ops@jump:22").is_none(),
            "{name}"
        );
        assert!(ws.ssh_key_passphrase_target(name).is_none(), "{name}");
    }
}

#[test]
fn a_connection_without_ssh_resolves_nothing_and_asks_no_store() {
    let ws = ws();
    let tun = resolved(ws.declared("tun").unwrap());
    let found = resolve_ssh_secrets(
        &ws,
        ws.declared("plain").unwrap(),
        &tun,
        &Untouchable,
        &no_env,
    )
    .unwrap();
    assert!(found.password.is_none() && found.key_passphrase.is_none());
}

#[test]
fn each_slot_reads_the_keychain_first_then_its_variable() {
    let ws = ws();
    let conn = ws.declared("tun").unwrap();
    let r = resolved(conn);
    let store = MemoryStore::new();
    store
        .set(
            &SecretKey::ssh_password_for(&ws, conn, &r.login()),
            &Secret::new("kc-pw"),
        )
        .unwrap();
    let env = env_of(&[
        ("DBENTO_SSH_PASSWORD_TUN", "env-pw"),
        ("DBENTO_SSH_KEY_PASSPHRASE_TUN", "env-pp"),
    ]);
    let found = resolve_ssh_secrets(&ws, conn, &r, &store, &env).unwrap();
    assert_eq!(found.password.unwrap().expose(), "kc-pw");
    assert_eq!(found.key_passphrase.unwrap().expose(), "env-pp");
}

#[test]
fn a_repointed_host_misses_the_pinned_password() {
    let before = ws_with("jump");
    let conn = before.declared("tun").unwrap();
    let store = MemoryStore::new();
    store
        .set(
            &SecretKey::ssh_password_for(&before, conn, &resolved(conn).login()),
            &Secret::new("kc-pw"),
        )
        .unwrap();
    let after = ws_with("evil.example");
    let moved = after.declared("tun").unwrap();
    let found = resolve_ssh_secrets(&after, moved, &resolved(moved), &store, &no_env).unwrap();
    assert!(
        found.password.is_none(),
        "the old host's password must not follow the new host"
    );
}

#[test]
fn an_absent_slot_is_not_an_error() {
    let ws = ws();
    let conn = ws.declared("tun").unwrap();
    let found =
        resolve_ssh_secrets(&ws, conn, &resolved(conn), &MemoryStore::new(), &no_env).unwrap();
    assert!(found.password.is_none() && found.key_passphrase.is_none());
}

#[test]
fn an_unavailable_keychain_falls_through_and_a_failing_one_is_an_error() {
    let ws = ws();
    let conn = ws.declared("tun").unwrap();
    let r = resolved(conn);
    let env = env_of(&[("DBENTO_SSH_PASSWORD_TUN", "env-pw")]);
    let locked = Answering(SecretError::Unavailable {
        message: "locked".into(),
    });
    let found = resolve_ssh_secrets(&ws, conn, &r, &locked, &env).unwrap();
    assert_eq!(found.password.unwrap().expose(), "env-pw");
    let broken = Answering(SecretError::Failed {
        message: "boom".into(),
    });
    assert!(matches!(
        resolve_ssh_secrets(&ws, conn, &r, &broken, &env),
        Err(SecretError::Failed { .. })
    ));
}

#[test]
fn a_derived_child_reads_its_parents_ssh_slots() {
    let ws = ws();
    let parent = ws.declared("kids").unwrap();
    let child = ws.connection("kids/app").unwrap();
    let r = resolved(&child);
    let store = MemoryStore::new();
    store
        .set(
            &SecretKey::ssh_password_for(&ws, parent, &r.login()),
            &Secret::new("p"),
        )
        .unwrap();
    store
        .set(
            &SecretKey::ssh_key_passphrase_for(&ws, parent),
            &Secret::new("pp"),
        )
        .unwrap();
    assert_eq!(
        SecretKey::ssh_password_for(&ws, &child, &r.login()),
        SecretKey::ssh_password_for(&ws, parent, &r.login()),
        "a child's accounts are its parent's"
    );
    let found = resolve_ssh_secrets(&ws, &child, &r, &store, &no_env).unwrap();
    assert_eq!(found.password.unwrap().expose(), "p");
    assert_eq!(found.key_passphrase.unwrap().expose(), "pp");
}

#[test]
fn session_options_carry_no_ssh_secret_by_default() {
    let o = SessionOptions::default();
    assert!(o.ssh_secrets.password.is_none() && o.ssh_secrets.key_passphrase.is_none());
}
