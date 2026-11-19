//! Renames found between two versions of dbento.toml, and moving a secret without overwriting.
use dbento_core::{
    Adopted, MemoryStore, RenameMatch, Secret, SecretError, SecretKey, SecretRename, SecretStore,
    Workspace, adopt_secret, secret_renames,
};
use std::path::{Path, PathBuf};

fn ws(body: &str) -> Workspace {
    let text = format!("dbento = 1\n{body}");
    Workspace::from_text(PathBuf::from("/w"), Path::new("/w/dbento.toml"), &text)
        .unwrap_or_else(|e| panic!("{text}: {e}"))
}

/// Postgres connection `name` on `host` as `user` reading `db`, with `extra` lines.
fn pg(name: &str, host: &str, user: &str, db: &str, extra: &str) -> String {
    format!(
        "[connections.{name}]\nengine = \"postgres\"\nhost = \"{host}\"\ndatabase = \"{db}\"\nuser = \"{user}\"\n{extra}\n"
    )
}

fn renames(old: &str, new: &str) -> Vec<SecretRename> {
    secret_renames(&ws(old), &ws(new))
}

fn pairs(r: &[SecretRename]) -> Vec<(String, String, RenameMatch)> {
    r.iter()
        .map(|r| (r.from.clone(), r.to.clone(), r.matched.clone()))
        .collect()
}

fn pair(from: &str, to: &str, m: RenameMatch) -> Vec<(String, String, RenameMatch)> {
    vec![(from.into(), to.into(), m)]
}

const KEY: &str = "tls_cert = \"c.crt\"\ntls_key = \"c.key\"\n";

#[test]
fn an_exact_rename_pairs_with_nothing_changed() {
    let r = renames(
        &pg("a", "h", "u", "d", ""),
        &pg("b", "h", "u", "d", "color = \"red\"\n"),
    );
    assert_eq!(pairs(&r), pair("a", "b", RenameMatch::Exact));
    assert!(r[0].changed.is_empty());
    assert!(r[0].password && !r[0].passphrase);
    assert_eq!(
        (r[0].from_target.as_str(), r[0].to_target.as_str()),
        ("u@h:5432/d", "u@h:5432/d")
    );
}

#[test]
fn a_database_change_is_the_same_login_and_names_the_key() {
    let r = renames(&pg("a", "h", "u", "d", ""), &pg("b", "h", "u", "e", ""));
    assert_eq!(pairs(&r), pair("a", "b", RenameMatch::SameLogin));
    assert_eq!(r[0].changed, vec!["database"]);
    assert_eq!(r[0].to_target, "u@h:5432/e");
}

#[test]
fn another_host_or_user_pairs_nothing() {
    assert!(renames(&pg("a", "h", "u", "d", ""), &pg("b", "h2", "u", "d", "")).is_empty());
    assert!(renames(&pg("a", "h", "u", "d", ""), &pg("b", "h", "v", "d", "")).is_empty());
}

#[test]
fn unchanged_names_and_a_name_swap_pair_nothing() {
    let both = format!(
        "{}{}",
        pg("a", "h", "u", "d", ""),
        pg("b", "h", "u", "e", "")
    );
    assert!(renames(&both, &both).is_empty());
    let swapped = format!(
        "{}{}",
        pg("a", "h", "u", "e", ""),
        pg("b", "h", "u", "d", "")
    );
    assert!(renames(&both, &swapped).is_empty());
}

#[test]
fn two_identical_connections_renamed_at_once_pair_nothing() {
    let old = format!(
        "{}{}",
        pg("a", "h", "u", "d", ""),
        pg("b", "h", "u", "d", "")
    );
    let new = format!(
        "{}{}",
        pg("c", "h", "u", "d", ""),
        pg("e", "h", "u", "d", "")
    );
    assert!(renames(&old, &new).is_empty());
}

#[test]
fn a_mixed_level_ambiguity_drops_both_pairs() {
    // a matches x exactly and b matches x by login only: x is claimed twice.
    let old = format!(
        "{}{}",
        pg("a", "h", "u", "d", ""),
        pg("b", "h", "u", "e", "")
    );
    assert!(renames(&old, &pg("x", "h", "u", "d", "")).is_empty());
}

#[test]
fn file_engines_and_secretless_connections_pair_nothing() {
    let lite = |n: &str| format!("[connections.{n}]\nengine = \"sqlite\"\npath = \"a.db\"\n");
    assert!(renames(&lite("a"), &lite("b")).is_empty());
    let env = "secret = { env = \"PGPW\" }\n";
    assert!(renames(&pg("a", "h", "u", "d", env), &pg("b", "h", "u", "d", env)).is_empty());
    let none = "secret = \"none\"\n";
    assert!(renames(&pg("a", "h", "u", "d", none), &pg("b", "h", "u", "d", none)).is_empty());
    assert!(
        renames(&pg("a", "h", "u", "d", ""), &pg("b", "h", "u", "d", env)).is_empty(),
        "keychain to env"
    );
}

#[test]
fn a_secretless_connection_with_a_client_key_pairs_its_passphrase_only() {
    let extra = format!("secret = \"none\"\n{KEY}");
    let r = renames(
        &pg("a", "h", "u", "d", &extra),
        &pg("b", "h", "u", "d", &extra),
    );
    assert_eq!(pairs(&r), pair("a", "b", RenameMatch::Exact));
    assert!(!r[0].password && r[0].passphrase);
    let r = renames(&pg("a", "h", "u", "d", KEY), &pg("b", "h", "u", "d", KEY));
    assert!(r[0].password && r[0].passphrase);
}

#[test]
fn a_derived_child_is_never_a_candidate_and_its_parent_pairs() {
    let dbs = "databases = [\"d1\"]\n";
    let r = renames(&pg("a", "h", "u", "d", dbs), &pg("b", "h", "u", "d", dbs));
    assert_eq!(pairs(&r), pair("a", "b", RenameMatch::Exact));
}

#[test]
fn the_output_is_sorted_by_the_old_name() {
    let old = format!(
        "{}{}",
        pg("z", "h", "u1", "d", ""),
        pg("m", "h", "u2", "d", "")
    );
    let new = format!(
        "{}{}",
        pg("z2", "h", "u1", "d", ""),
        pg("m2", "h", "u2", "d", "")
    );
    let from: Vec<String> = renames(&old, &new).into_iter().map(|r| r.from).collect();
    assert_eq!(from, vec!["m", "z"]);
}

fn key(name: &str) -> SecretKey {
    SecretKey::new("/w", name)
}

fn value(store: &dyn SecretStore, k: &SecretKey) -> Option<String> {
    store.get(k).unwrap().map(|s| s.expose().to_owned())
}

#[test]
fn adopt_moves_into_an_empty_name() {
    let store = MemoryStore::new();
    store.set(&key("a"), &Secret::new("old")).unwrap();
    assert_eq!(
        adopt_secret(&store, &key("a"), &key("b")).unwrap(),
        Adopted::Moved
    );
    assert_eq!(value(&store, &key("b")).as_deref(), Some("old"));
    assert_eq!(value(&store, &key("a")), None);
}

#[test]
fn adopt_with_nothing_stored_writes_nothing() {
    let store = MemoryStore::new();
    assert_eq!(
        adopt_secret(&store, &key("a"), &key("b")).unwrap(),
        Adopted::NothingStored
    );
    assert_eq!(value(&store, &key("b")), None);
}

#[test]
fn adopt_leaves_a_taken_name_and_the_old_entry_alone() {
    let store = MemoryStore::new();
    store.set(&key("a"), &Secret::new("old")).unwrap();
    store.set(&key("b"), &Secret::new("new")).unwrap();
    assert_eq!(
        adopt_secret(&store, &key("a"), &key("b")).unwrap(),
        Adopted::TargetTaken
    );
    assert_eq!(value(&store, &key("a")).as_deref(), Some("old"));
    assert_eq!(value(&store, &key("b")).as_deref(), Some("new"));
}

#[test]
fn adopt_onto_itself_never_writes() {
    let store = MemoryStore::new();
    assert_eq!(
        adopt_secret(&store, &key("a"), &key("a")).unwrap(),
        Adopted::NothingStored
    );
    store.set(&key("a"), &Secret::new("old")).unwrap();
    assert_eq!(
        adopt_secret(&store, &key("a"), &key("a")).unwrap(),
        Adopted::TargetTaken
    );
    assert_eq!(value(&store, &key("a")).as_deref(), Some("old"));
}

#[test]
fn adopt_moves_a_passphrase_slot_without_touching_the_password_slot() {
    let store = MemoryStore::new();
    let (from, to) = (
        SecretKey::key_passphrase("/w", "a"),
        SecretKey::key_passphrase("/w", "b"),
    );
    store.set(&from, &Secret::new("phrase")).unwrap();
    assert_eq!(adopt_secret(&store, &from, &to).unwrap(), Adopted::Moved);
    assert_eq!(value(&store, &to).as_deref(), Some("phrase"));
    assert_eq!(value(&store, &key("b")), None);
}

struct FailingDelete(MemoryStore);
impl SecretStore for FailingDelete {
    fn get(&self, k: &SecretKey) -> Result<Option<Secret>, SecretError> {
        self.0.get(k)
    }
    fn set(&self, k: &SecretKey, s: &Secret) -> Result<(), SecretError> {
        self.0.set(k, s)
    }
    fn delete(&self, _: &SecretKey) -> Result<(), SecretError> {
        Err(SecretError::Failed {
            message: "locked".into(),
        })
    }
}

#[test]
fn a_failed_delete_reports_the_error_after_the_new_entry_is_written() {
    let store = FailingDelete(MemoryStore::new());
    store.set(&key("a"), &Secret::new("old")).unwrap();
    assert!(adopt_secret(&store, &key("a"), &key("b")).is_err());
    assert_eq!(value(&store, &key("b")).as_deref(), Some("old"));
    assert_eq!(value(&store, &key("a")).as_deref(), Some("old"));
}
