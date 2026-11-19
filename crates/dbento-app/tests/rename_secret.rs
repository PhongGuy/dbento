//! An outside rename carries a connection's saved keychain secrets to its new name.
use dbento_app::rename_secret::{Held, adopt_held, probe, rename_keys};
use dbento_core::{
    MemoryStore, Secret, SecretError, SecretKey, SecretStore, Workspace, secret_renames,
};
use std::path::PathBuf;

const LITE: &str = "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"data/app.db\"\n";
const KEY: &str = "tls = \"verify\"\ntls_cert = \"c.crt\"\ntls_key = \"c.key\"\n";
const SECRET: &str = "Qz7vK9xW";

/// Postgres `name` as `user` on the closed port 127.0.0.1:9 reading `db`, with `extra` lines.
fn pg(name: &str, user: &str, db: &str, extra: &str) -> String {
    format!(
        "\n[connections.{name}]\nengine = \"postgres\"\nhost = \"127.0.0.1\"\nport = 9\ndatabase = \"{db}\"\nuser = \"{user}\"\n{extra}"
    )
}

/// A workspace at /w holding `lite` and `body`, parsed without touching the disk.
fn at_w(body: &str) -> Workspace {
    let dir = PathBuf::from("/w");
    Workspace::from_text(
        dir.clone(),
        &dir.join("dbento.toml"),
        &format!("{LITE}{body}"),
    )
    .unwrap()
}

/// A store whose reads fail with `get` when set and whose writes fail when `set_fails`.
struct Broken {
    inner: MemoryStore,
    get: Option<SecretError>,
    set_fails: bool,
}
impl SecretStore for Broken {
    fn get(&self, k: &SecretKey) -> Result<Option<Secret>, SecretError> {
        match &self.get {
            Some(e) => Err(e.clone()),
            None => self.inner.get(k),
        }
    }
    fn set(&self, k: &SecretKey, s: &Secret) -> Result<(), SecretError> {
        if self.set_fails {
            return Err(locked());
        }
        self.inner.set(k, s)
    }
    fn delete(&self, k: &SecretKey) -> Result<(), SecretError> {
        self.inner.delete(k)
    }
}
fn locked() -> SecretError {
    SecretError::Failed {
        message: "locked".into(),
    }
}
fn unavailable() -> SecretError {
    SecretError::Unavailable {
        message: "no keychain".into(),
    }
}

fn keys_ab(extra: &str) -> dbento_app::rename_secret::RenameKeys {
    let (old, new) = (
        at_w(&pg("a", "x", "x", extra)),
        at_w(&pg("b", "x", "x", extra)),
    );
    rename_keys(&old, &new, &secret_renames(&old, &new)[0])
}

#[test]
fn rename_keys_name_each_slot_both_sides_use() {
    let keys = keys_ab(KEY);
    assert_eq!(
        keys.password,
        Some((SecretKey::new("/w", "a"), SecretKey::new("/w", "b")))
    );
    assert_eq!(
        keys.passphrase,
        Some((
            SecretKey::key_passphrase("/w", "a"),
            SecretKey::key_passphrase("/w", "b")
        ))
    );
    assert_eq!(keys_ab("").passphrase, None);
}

#[test]
fn probe_sees_which_slots_hold_and_adopt_moves_only_those() {
    let keys = keys_ab(KEY);
    let store = MemoryStore::new();
    store
        .set(&SecretKey::new("/w", "a"), &Secret::new(SECRET))
        .unwrap();
    let held = probe(&store, &keys).unwrap();
    assert_eq!(
        held,
        Held {
            password: true,
            ..Held::default()
        }
    );
    assert!(held.movable());
    adopt_held(&store, &keys, held).unwrap();
    assert!(store.get(&SecretKey::new("/w", "b")).unwrap().is_some());
    assert!(store.get(&SecretKey::new("/w", "a")).unwrap().is_none());
    assert!(
        store
            .get(&SecretKey::key_passphrase("/w", "b"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_slot_already_filled_under_the_new_name_is_not_movable() {
    let keys = keys_ab("");
    let store = MemoryStore::new();
    store
        .set(&SecretKey::new("/w", "a"), &Secret::new(SECRET))
        .unwrap();
    store
        .set(&SecretKey::new("/w", "b"), &Secret::new("typed"))
        .unwrap();
    let held = probe(&store, &keys).unwrap();
    assert_eq!(
        held,
        Held {
            password: true,
            password_taken: true,
            ..Held::default()
        }
    );
    assert!(!held.movable());
}

#[test]
fn an_unavailable_keychain_probes_as_empty_and_a_failing_one_is_an_error() {
    let keys = keys_ab(KEY);
    let gone = Broken {
        inner: MemoryStore::new(),
        get: Some(unavailable()),
        set_fails: false,
    };
    assert_eq!(probe(&gone, &keys).unwrap(), Held::default());
    let failing = Broken {
        inner: MemoryStore::new(),
        get: Some(locked()),
        set_fails: false,
    };
    assert!(probe(&failing, &keys).is_err());
}
