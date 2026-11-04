//! Secrets stored after an import: final names, never replacing one, failures per name.
use super::*;
use crate::import::model::{
    ConnectionDraft, ImportSet, ImportedConnection, PgDraft, SecretSetting, SkipReason, WarningKind,
};
use crate::import::{OnConflict, plan};
use crate::{MemoryStore, Secret, SecretError, SecretKey, SecretStore, TlsMode, Workspace};
use std::collections::BTreeMap;
use std::path::Path;

fn pg_with(name: &str, host: &str, secret: SecretSetting) -> ImportedConnection {
    let pg = PgDraft {
        host: host.to_owned(),
        port: None,
        database: "orders".to_owned(),
        user: "app".to_owned(),
        secret,
        tls: Some(TlsMode::Verify),
        tls_ca: None,
        tls_cert: None,
        tls_key: None,
        settings: BTreeMap::new(),
    };
    let draft = ConnectionDraft::postgres(pg, false);
    ImportedConnection::new(format!("id-{name}-{host}"), name, name, draft)
}

fn pg(name: &str, host: &str) -> ImportedConnection {
    pg_with(name, host, SecretSetting::Keychain)
}

fn set_of(connections: Vec<ImportedConnection>) -> ImportSet {
    let mut set = ImportSet::default();
    for c in connections {
        set.add_connection(c);
    }
    set
}

/// A workspace holding a Postgres `app@<host>/orders` connection per pair.
fn existing(pairs: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    let mut text = String::from("dbento = 1\n");
    for (name, host) in pairs {
        text.push_str(&format!(
            "[connections.{name}]\nengine = \"postgres\"\nhost = \"{host}\"\ndatabase = \"orders\"\nuser = \"app\"\n"
        ));
    }
    std::fs::write(dir.path().join("dbento.toml"), text).unwrap();
    dir
}

fn secrets(pairs: &[(&str, &str)]) -> ImportSecrets {
    let mut s = ImportSecrets::default();
    for (id, pw) in pairs {
        s.insert(*id, Secret::new(*pw));
    }
    s
}

/// Plans `set` into `dir`, applies it and stores `secrets`.
fn import(
    dir: &Path,
    set: &ImportSet,
    on_conflict: OnConflict,
    secrets: &ImportSecrets,
    store: &dyn SecretStore,
    env: &dyn Fn(&str) -> Option<String>,
) -> StoredSecrets {
    let p = plan(dir, set, on_conflict).unwrap();
    let imported = p.imported().to_vec();
    p.apply().unwrap();
    store_secrets(dir, &imported, secrets, store, env)
}

fn stored_value(dir: &Path, name: &str, store: &dyn SecretStore) -> Option<String> {
    let ws = Workspace::open(dir).unwrap();
    let key = SecretKey::for_connection(&ws, ws.connection(name).unwrap());
    store.get(&key).unwrap().map(|s| s.expose().to_owned())
}

fn no_env(_: &str) -> Option<String> {
    None
}

struct Unavailable;
impl SecretStore for Unavailable {
    fn get(&self, _: &SecretKey) -> Result<Option<Secret>, SecretError> {
        Err(SecretError::Unavailable {
            message: "no keychain in this test".into(),
        })
    }
    fn set(&self, _: &SecretKey, _: &Secret) -> Result<(), SecretError> {
        Err(SecretError::Unavailable {
            message: "no keychain in this test".into(),
        })
    }
    fn delete(&self, _: &SecretKey) -> Result<(), SecretError> {
        Ok(())
    }
}

#[test]
fn imported_lists_added_and_unchanged_connections_under_their_final_names() {
    let ws = existing(&[("orders", "db.example")]);
    let set = set_of(vec![
        pg("orders", "db.example"),
        pg("shop", "shop.example"),
        pg("orders", "other.example"),
    ]);
    let p = plan(ws.path(), &set, OnConflict::Skip).unwrap();
    let got: Vec<(&str, &str, bool)> = p
        .imported()
        .iter()
        .map(|i| (i.source_id(), i.name(), i.added()))
        .collect();
    assert_eq!(
        got,
        [
            ("id-orders-db.example", "orders", false),
            ("id-shop-shop.example", "shop", true)
        ]
    );
}

#[test]
fn an_added_connection_stores_its_secret_under_the_renamed_name() {
    let ws = existing(&[("orders", "db.example")]);
    let set = set_of(vec![pg("orders", "other.example")]);
    let store = MemoryStore::new();
    let given = secrets(&[("id-orders-other.example", "fake-pw-1")]);
    let out = import(ws.path(), &set, OnConflict::Rename, &given, &store, &no_env);
    assert_eq!(
        out.outcomes(),
        [("orders-2".to_owned(), SecretOutcome::Stored)]
    );
    assert_eq!(out.stored(), ["orders-2"]);
    assert_eq!(
        stored_value(ws.path(), "orders-2", &store).as_deref(),
        Some("fake-pw-1")
    );
    assert_eq!(stored_value(ws.path(), "orders", &store), None);
}

#[test]
fn an_unchanged_connection_fills_an_empty_entry_and_never_replaces_one() {
    let ws = existing(&[("a", "db.a"), ("b", "db.b")]);
    let store = MemoryStore::new();
    {
        let w = Workspace::open(ws.path()).unwrap();
        let key = SecretKey::for_connection(&w, w.connection("b").unwrap());
        store.set(&key, &Secret::new("user-changed")).unwrap();
    }
    let set = set_of(vec![pg("a", "db.a"), pg("b", "db.b")]);
    let given = secrets(&[("id-a-db.a", "fake-pw-1"), ("id-b-db.b", "fake-pw-2")]);
    let out = import(ws.path(), &set, OnConflict::Skip, &given, &store, &no_env);
    assert_eq!((out.stored(), out.present()), (vec!["a"], vec!["b"]));
    assert_eq!(
        stored_value(ws.path(), "a", &store).as_deref(),
        Some("fake-pw-1")
    );
    assert_eq!(
        stored_value(ws.path(), "b", &store).as_deref(),
        Some("user-changed")
    );
}

#[test]
fn an_env_secret_counts_as_present_so_the_keychain_never_shadows_it() {
    let ws = existing(&[("a", "db.a")]);
    let store = MemoryStore::new();
    let env = |v: &str| (v == "DBENTO_SECRET_A").then(|| "from-env".to_owned());
    let given = secrets(&[("id-a-db.a", "fake-pw-1")]);
    let out = import(
        ws.path(),
        &set_of(vec![pg("a", "db.a")]),
        OnConflict::Skip,
        &given,
        &store,
        &env,
    );
    assert_eq!(out.present(), ["a"]);
    assert_eq!(stored_value(ws.path(), "a", &store), None);
}

#[test]
fn failures_and_missing_secrets_are_reported_per_name_without_the_secret() {
    let ws = existing(&[("kept", "db.kept")]);
    let set = set_of(vec![
        pg("new", "db.new"),
        pg("bare", "db.bare"),
        pg("kept", "db.kept"),
    ]);
    let given = secrets(&[
        ("id-new-db.new", "fake-pw-1"),
        ("id-kept-db.kept", "fake-pw-3"),
    ]);
    let out = import(
        ws.path(),
        &set,
        OnConflict::Skip,
        &given,
        &Unavailable,
        &no_env,
    );
    let failed: Vec<&str> = out.failed().iter().map(|(n, _)| *n).collect();
    assert_eq!(failed, ["new", "kept"]);
    for (_, e) in out.failed() {
        assert!(
            e.contains("no keychain in this test") && !e.contains("fake-pw"),
            "{e}"
        );
    }
    assert_eq!(out.needs_secret(), ["new", "bare", "kept"]);
    assert!(out.stored().is_empty() && out.present().is_empty());
    assert!(!format!("{out:?}").contains("fake-pw"));
}

#[test]
fn sqlite_and_secret_none_connections_are_left_out() {
    let ws = tempfile::TempDir::new().unwrap();
    let path = ws.path().join("x.db").to_string_lossy().into_owned();
    let lite = ImportedConnection::new(
        "id-lite",
        "lite",
        "lite",
        ConnectionDraft::sqlite(path, false),
    );
    let set = set_of(vec![lite, pg_with("open", "db.open", SecretSetting::None)]);
    let given = secrets(&[("id-lite", "fake-pw-1"), ("id-open-db.open", "fake-pw-2")]);
    let store = MemoryStore::new();
    let out = import(ws.path(), &set, OnConflict::Skip, &given, &store, &no_env);
    assert!(out.outcomes().is_empty(), "{out:?}");
}

#[test]
fn a_workspace_that_cannot_be_opened_fails_every_secret_and_stores_none() {
    let ws = tempfile::TempDir::new().unwrap();
    let set = set_of(vec![pg("a", "db.a"), pg("b", "db.b")]);
    let p = plan(ws.path(), &set, OnConflict::Skip).unwrap();
    let imported = p.imported().to_vec();
    // Not applied: there is no dbento.toml to open.
    let store = MemoryStore::new();
    let out = store_secrets(
        ws.path(),
        &imported,
        &secrets(&[("id-a-db.a", "fake-pw-1")]),
        &store,
        &no_env,
    );
    let failed: Vec<&str> = out.failed().iter().map(|(n, _)| *n).collect();
    assert_eq!(failed, ["a"]);
    assert!(out.stored().is_empty());
}

#[test]
fn import_secrets_debug_shows_ids_only_and_the_new_codes_are_snake_case() {
    let s = secrets(&[("id-a", "fake-pw-1")]);
    let text = format!("{s:?}");
    assert!(text.contains("id-a") && !text.contains("fake-pw"), "{text}");
    assert_eq!(s.get("id-a").map(Secret::expose), Some("fake-pw-1"));
    assert_eq!(
        (s.len(), s.is_empty(), s.ids().collect::<Vec<_>>()),
        (1, false, vec!["id-a"])
    );
    assert_eq!(
        (SkipReason::Proxy.code(), SkipReason::Proxy.detail()),
        ("proxy", None)
    );
    let w = WarningKind::AuthModel("postgres_pgpass".into());
    assert_eq!(
        (w.code(), w.detail()),
        ("auth_model", Some("postgres_pgpass"))
    );
}
