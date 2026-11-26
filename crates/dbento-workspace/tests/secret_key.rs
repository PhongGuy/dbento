//! The keys a connection's secrets are stored under.
use dbento_workspace::{SecretKey, Slot, Workspace};
use std::path::{Path, PathBuf};

fn open(text: &str) -> Workspace {
    let text = format!("dbento = 1\n{text}");
    Workspace::from_text(PathBuf::from("/w"), Path::new("/w/dbento.toml"), &text)
        .unwrap_or_else(|e| panic!("{text}: {e}"))
}

const PG: &str = "[connections.pg]\nengine = \"postgres\"\nhost = \"db.example\"\ndatabase = \"d\"\nuser = \"ada\"\ndatabases = [\"app\"]\n";

#[test]
fn a_connection_password_is_keyed_by_its_pin() {
    let w = open(PG);
    let conn = w.declared("pg").unwrap();
    let key = SecretKey::for_connection(&w, conn);
    assert_eq!(key, SecretKey::password("/w", "pg", &conn.secret_pin()));
    assert_eq!(key.slot(), Slot::Password);
    assert_eq!(key.pin(), Some(conn.secret_pin().as_str()));
    assert_eq!(w.secret_target("pg"), Some(key.clone()));
    let child = w.connection("pg/app").unwrap();
    assert_eq!(
        SecretKey::for_connection(&w, &child),
        key,
        "a child uses its parent's key"
    );
}

#[test]
fn another_target_is_another_key_and_the_legacy_key_is_neither() {
    let (a, b) = (open(PG), open(&PG.replace("db.example", "db2.example")));
    let (ka, kb) = (
        a.secret_target("pg").unwrap(),
        b.secret_target("pg").unwrap(),
    );
    assert_ne!(ka, kb, "a repointed host names another entry");
    let legacy = SecretKey::legacy_password("/w", "pg");
    assert!(
        legacy != ka && legacy != kb,
        "the legacy key is its own entry"
    );
    assert_eq!(legacy.slot(), Slot::LegacyPassword);
    assert_eq!(legacy.pin(), None);
    assert_eq!(
        (legacy.workspace_dir(), legacy.connection()),
        (Path::new("/w"), "pg")
    );
}

#[test]
fn a_rename_keeps_the_pin() {
    let w = open(PG);
    let key = w.secret_target("pg").unwrap();
    let renamed = key.renamed("pg2");
    assert_eq!(renamed.connection(), "pg2");
    assert_eq!(renamed.pin(), key.pin());
    let pin = w.declared("pg").unwrap().secret_pin();
    assert_eq!(renamed, SecretKey::password("/w", "pg2", &pin));
}

#[test]
fn only_the_password_and_the_ssh_password_carry_a_pin() {
    let ssh = SecretKey::ssh_password("/w", "pg", "ops@jump:22");
    assert_eq!(ssh.ssh_login(), Some("ops@jump:22"));
    assert_eq!(ssh.pin(), Some("ops@jump:22"));
    assert_eq!(SecretKey::key_passphrase("/w", "pg").pin(), None);
    assert_eq!(SecretKey::ssh_key_passphrase("/w", "pg").pin(), None);
    let w = open(PG);
    assert_eq!(
        w.secret_target("pg").unwrap().ssh_login(),
        None,
        "no SSH login on a db password"
    );
}
