//! Moving passwords saved before they were pinned to their server.
use dbento_core::{
    Adopted, LegacyMigration, MemoryStore, Secret, SecretError, SecretKey, SecretStore, Workspace,
    adopt_legacy_password, migrate_legacy_passwords, resolve_secret, secret_hint,
};
use std::path::{Path, PathBuf};

fn ws(body: &str) -> Workspace {
    let text = format!("dbento = 1\n{body}");
    Workspace::from_text(PathBuf::from("/w"), Path::new("/w/dbento.toml"), &text)
        .unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn pg(name: &str, host: &str, extra: &str) -> String {
    format!(
        "[connections.{name}]\nengine = \"postgres\"\nhost = \"{host}\"\ndatabase = \"d\"\nuser = \"u\"\n{extra}"
    )
}

fn legacy(name: &str) -> SecretKey {
    SecretKey::legacy_password("/w", name)
}

fn pinned(w: &Workspace, name: &str) -> SecretKey {
    SecretKey::for_connection(w, w.declared(name).unwrap())
}

fn value(store: &dyn SecretStore, key: &SecretKey) -> Option<String> {
    store.get(key).unwrap().map(|s| s.expose().to_owned())
}

fn no_env(_: &str) -> Option<String> {
    None
}

fn resolved(
    w: &Workspace,
    name: &str,
    store: &dyn SecretStore,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    resolve_secret(w, w.declared(name).unwrap(), store, env)
        .unwrap()
        .map(|s| s.expose().to_owned())
}

fn outcomes(
    r: Vec<(String, Result<LegacyMigration, SecretError>)>,
) -> Vec<(String, Option<LegacyMigration>)> {
    r.into_iter().map(|(n, r)| (n, r.ok())).collect()
}

fn one(name: &str, m: LegacyMigration) -> Vec<(String, Option<LegacyMigration>)> {
    vec![(name.to_owned(), Some(m))]
}

struct Locked;
impl SecretStore for Locked {
    fn get(&self, _: &SecretKey) -> Result<Option<Secret>, SecretError> {
        Err(SecretError::Unavailable {
            message: "locked".into(),
        })
    }
    fn set(&self, _: &SecretKey, _: &Secret) -> Result<(), SecretError> {
        Err(SecretError::Unavailable {
            message: "locked".into(),
        })
    }
    fn delete(&self, _: &SecretKey) -> Result<(), SecretError> {
        Err(SecretError::Unavailable {
            message: "locked".into(),
        })
    }
}

/// A store whose deletes fail, so a move stops between its set and its delete.
struct FailDelete(MemoryStore);
impl SecretStore for FailDelete {
    fn get(&self, k: &SecretKey) -> Result<Option<Secret>, SecretError> {
        self.0.get(k)
    }
    fn set(&self, k: &SecretKey, s: &Secret) -> Result<(), SecretError> {
        self.0.set(k, s)
    }
    fn delete(&self, _: &SecretKey) -> Result<(), SecretError> {
        Err(SecretError::Failed {
            message: "delete refused".into(),
        })
    }
}

#[test]
fn a_found_pinned_entry_drops_the_legacy_one() {
    let w = ws(&pg("pg", "db.example", ""));
    let store = MemoryStore::new();
    store.set(&pinned(&w, "pg"), &Secret::new("x")).unwrap();
    store.set(&legacy("pg"), &Secret::new("x")).unwrap();
    assert_eq!(
        outcomes(migrate_legacy_passwords(&store, &w)),
        one("pg", LegacyMigration::LegacyDropped)
    );
    assert_eq!(
        value(&store, &legacy("pg")),
        None,
        "the legacy entry is gone"
    );
    assert_eq!(
        value(&store, &pinned(&w, "pg")).as_deref(),
        Some("x"),
        "the pinned entry stays"
    );
}

#[test]
fn an_unchanged_target_moves_the_legacy_entry_and_the_connect_finds_it() {
    let (baseline, current) = (
        ws(&pg("pg", "db.example", "")),
        ws(&pg("pg", "db.example", "")),
    );
    let store = MemoryStore::new();
    store.set(&legacy("pg"), &Secret::new("x")).unwrap();
    assert_eq!(
        outcomes(migrate_legacy_passwords(&store, &baseline)),
        one("pg", LegacyMigration::Pinned)
    );
    assert_eq!(
        value(&store, &legacy("pg")),
        None,
        "the legacy entry is gone"
    );
    assert_eq!(
        resolved(&current, "pg", &store, &no_env).as_deref(),
        Some("x")
    );
}

#[test]
fn a_changed_target_keeps_the_entry_under_the_old_servers_pin() {
    let (baseline, current) = (
        ws(&pg("pg", "db.example", "")),
        ws(&pg("pg", "evil.example", "")),
    );
    let store = MemoryStore::new();
    store.set(&legacy("pg"), &Secret::new("x")).unwrap();
    assert_eq!(
        outcomes(migrate_legacy_passwords(&store, &baseline)),
        one("pg", LegacyMigration::Pinned)
    );
    assert_eq!(
        value(&store, &pinned(&baseline, "pg")).as_deref(),
        Some("x"),
        "orphaned under the old pin"
    );
    assert_eq!(
        resolved(&current, "pg", &store, &no_env),
        None,
        "the new server misses"
    );
    assert!(
        secret_hint(&current, current.declared("pg").unwrap(), &store).is_some(),
        "the connect gets a hint"
    );
}

#[test]
fn a_connection_absent_from_the_baseline_is_untouched() {
    let (baseline, current) = (
        ws(&pg("other", "db.example", "")),
        ws(&pg("pg", "db.example", "")),
    );
    let store = MemoryStore::new();
    store.set(&legacy("pg"), &Secret::new("x")).unwrap();
    let results = migrate_legacy_passwords(&store, &baseline);
    assert!(results.iter().all(|(n, _)| n != "pg"), "{results:?}");
    assert_eq!(
        value(&store, &legacy("pg")).as_deref(),
        Some("x"),
        "the legacy entry stays"
    );
    assert_eq!(
        resolved(&current, "pg", &store, &no_env),
        None,
        "a connect never reads the legacy entry"
    );
}

#[test]
fn nothing_stored_falls_back_to_the_environment() {
    let w = ws(&pg("pg", "db.example", ""));
    let store = MemoryStore::new();
    assert_eq!(
        outcomes(migrate_legacy_passwords(&store, &w)),
        one("pg", LegacyMigration::NothingStored)
    );
    let env = |v: &str| (v == "DBENTO_SECRET_PG").then(|| "x".to_owned());
    assert_eq!(resolved(&w, "pg", &store, &env).as_deref(), Some("x"));
}

#[test]
fn an_unavailable_keychain_is_reported_per_connection() {
    let w = ws(&format!("{}{}", pg("a", "h", ""), pg("b", "h", "")));
    let results = migrate_legacy_passwords(&Locked, &w);
    assert_eq!(results.len(), 2, "{results:?}");
    for (name, r) in &results {
        assert!(
            matches!(r, Err(SecretError::Unavailable { .. })),
            "{name}: {r:?}"
        );
    }
}

#[test]
fn a_failed_delete_after_the_set_keeps_both_entries() {
    let w = ws(&pg("pg", "db.example", ""));
    let store = FailDelete(MemoryStore::new());
    store.0.set(&legacy("pg"), &Secret::new("x")).unwrap();
    let results = migrate_legacy_passwords(&store, &w);
    assert!(
        matches!(results[0].1, Err(SecretError::Failed { .. })),
        "{results:?}"
    );
    assert_eq!(
        value(&store.0, &pinned(&w, "pg")).as_deref(),
        Some("x"),
        "the set happened first"
    );
    assert_eq!(
        value(&store.0, &legacy("pg")).as_deref(),
        Some("x"),
        "the legacy entry survives"
    );
}

#[test]
fn only_keychain_password_connections_are_migrated() {
    let body = format!(
        "{}{}{}[connections.lite]\nengine = \"sqlite\"\npath = \"a.db\"\n",
        pg("kc", "h", ""),
        pg("byenv", "h", "secret = { env = \"PW\" }\n"),
        pg("nopw", "h", "secret = \"none\"\n"),
    );
    let names: Vec<String> = migrate_legacy_passwords(&MemoryStore::new(), &ws(&body))
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(names, vec!["kc"]);
}

#[test]
fn adopt_pins_the_legacy_entry_to_the_current_target() {
    let w = ws(&pg("pg", "evil.example", ""));
    let store = MemoryStore::new();
    store.set(&legacy("pg"), &Secret::new("x")).unwrap();
    assert_eq!(
        adopt_legacy_password(&store, &w, "pg").unwrap(),
        Adopted::Moved
    );
    assert_eq!(value(&store, &pinned(&w, "pg")).as_deref(), Some("x"));
    assert_eq!(value(&store, &legacy("pg")), None);
}

#[test]
fn adopt_with_no_legacy_entry_writes_nothing() {
    let w = ws(&pg("pg", "db.example", ""));
    let store = MemoryStore::new();
    assert_eq!(
        adopt_legacy_password(&store, &w, "pg").unwrap(),
        Adopted::NothingStored
    );
    assert_eq!(value(&store, &pinned(&w, "pg")), None);
}

#[test]
fn adopt_leaves_a_taken_pin_and_the_legacy_entry_alone() {
    let w = ws(&pg("pg", "db.example", ""));
    let store = MemoryStore::new();
    store.set(&pinned(&w, "pg"), &Secret::new("x")).unwrap();
    store.set(&legacy("pg"), &Secret::new("x")).unwrap();
    assert_eq!(
        adopt_legacy_password(&store, &w, "pg").unwrap(),
        Adopted::TargetTaken
    );
    assert_eq!(
        value(&store, &legacy("pg")).as_deref(),
        Some("x"),
        "the legacy entry stays"
    );
}

#[test]
fn adopt_ignores_names_without_a_keychain_password_without_a_store_call() {
    let body = format!(
        "{}{}[connections.lite]\nengine = \"sqlite\"\npath = \"a.db\"\n",
        pg("byenv", "h", "secret = { env = \"PW\" }\n"),
        pg("nopw", "h", "secret = \"none\"\n"),
    );
    let w = ws(&body);
    // `Locked` fails every call, so an Ok proves the store was never asked.
    for name in ["byenv", "nopw", "lite", "missing"] {
        assert_eq!(
            adopt_legacy_password(&Locked, &w, name).unwrap(),
            Adopted::NothingStored,
            "{name}"
        );
    }
}
