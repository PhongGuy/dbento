use dbento_core::{
    MemoryStore, OpenError, Secret, SecretError, SecretKey, SecretStore, TimedStore, Workspace,
    resolve_secret,
};
use std::collections::HashMap;
use std::time::{Duration, Instant};

fn workspace() -> (tempfile::TempDir, Workspace) {
    let pg = |name: &str, extra: &str| {
        format!(
            "[connections.{name}]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\n{extra}\n"
        )
    };
    let toml = format!(
        "dbento = 1\n{}{}{}{}[connections.lite]\nengine = \"sqlite\"\npath = \"x.db\"\n",
        pg("kc", ""),
        pg("my-db", ""),
        pg("byenv", "secret = { env = \"APP_PW\" }"),
        pg("nosecret", "secret = \"none\""),
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    (dir, ws)
}

fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |name| map.get(name).cloned()
}

fn exposed(r: Result<Option<Secret>, SecretError>) -> Option<String> {
    r.unwrap().map(|s| s.expose().to_owned())
}

/// A store that is locked (`Unavailable`), broken (`Failing`), never answers (`Hanging`) or panics.
enum Broken {
    Unavailable,
    Failing,
    Hanging,
    Panicking,
}
impl Broken {
    fn answer(&self) -> Result<(), SecretError> {
        match self {
            Broken::Hanging => std::thread::sleep(Duration::from_secs(60)),
            Broken::Panicking => panic!("store bug"),
            Broken::Failing => {
                return Err(SecretError::Failed {
                    message: "denied".into(),
                });
            }
            Broken::Unavailable => {}
        }
        Err(SecretError::Unavailable {
            message: "locked".into(),
        })
    }
}
impl SecretStore for Broken {
    fn get(&self, _: &SecretKey) -> Result<Option<Secret>, SecretError> {
        self.answer().map(|()| None)
    }
    fn set(&self, _: &SecretKey, _: &Secret) -> Result<(), SecretError> {
        self.answer()
    }
    fn delete(&self, _: &SecretKey) -> Result<(), SecretError> {
        self.answer()
    }
}

#[test]
fn the_keychain_entry_wins_over_the_environment() {
    let (_d, ws) = workspace();
    let conn = ws.connection("kc").unwrap();
    let store = MemoryStore::new();
    store
        .set(
            &SecretKey::for_connection(&ws, conn),
            &Secret::new("from-keychain"),
        )
        .unwrap();
    let env = env_of(&[("DBENTO_SECRET_KC", "from-env")]);
    assert_eq!(
        exposed(resolve_secret(&ws, conn, &store, &env)),
        Some("from-keychain".into())
    );
}

#[test]
fn an_absent_or_unavailable_entry_falls_back_to_the_named_variable() {
    let (_d, ws) = workspace();
    let conn = ws.connection("my-db").unwrap();
    let env = env_of(&[("DBENTO_SECRET_MY_DB", "from-env")]);
    assert_eq!(
        exposed(resolve_secret(&ws, conn, &MemoryStore::new(), &env)),
        Some("from-env".into())
    );
    assert_eq!(
        exposed(resolve_secret(&ws, conn, &Broken::Unavailable, &env)),
        Some("from-env".into())
    );
    assert_eq!(
        exposed(resolve_secret(&ws, conn, &MemoryStore::new(), &env_of(&[]))),
        None
    );
}

#[test]
fn a_failing_keychain_is_an_error_not_a_fallback() {
    let (_d, ws) = workspace();
    let conn = ws.connection("my-db").unwrap();
    let env = env_of(&[("DBENTO_SECRET_MY_DB", "from-env")]);
    let err = resolve_secret(&ws, conn, &Broken::Failing, &env).expect_err("Failed propagates");
    assert!(matches!(err, SecretError::Failed { .. }), "{err:?}");
}

#[test]
fn an_env_secret_reads_only_its_variable() {
    let (_d, ws) = workspace();
    let conn = ws.connection("byenv").unwrap();
    let store = MemoryStore::new();
    store
        .set(
            &SecretKey::for_connection(&ws, conn),
            &Secret::new("ignored"),
        )
        .unwrap();
    assert_eq!(
        exposed(resolve_secret(
            &ws,
            conn,
            &store,
            &env_of(&[("APP_PW", "pw")])
        )),
        Some("pw".into())
    );
    let err = resolve_secret(&ws, conn, &store, &env_of(&[("DBENTO_SECRET_BYENV", "x")]))
        .expect_err("unset variable");
    assert!(
        matches!(err, SecretError::Missing { ref variable } if variable == "APP_PW"),
        "{err:?}"
    );
}

#[test]
fn secret_none_and_sqlite_resolve_to_nothing() {
    let (_d, ws) = workspace();
    let store = MemoryStore::new();
    let env = env_of(&[("DBENTO_SECRET_NOSECRET", "x"), ("DBENTO_SECRET_LITE", "x")]);
    for name in ["nosecret", "lite"] {
        let conn = ws.connection(name).unwrap();
        store
            .set(&SecretKey::for_connection(&ws, conn), &Secret::new("x"))
            .unwrap();
        assert_eq!(
            exposed(resolve_secret(&ws, conn, &store, &env)),
            None,
            "{name}"
        );
    }
}

#[test]
fn an_empty_env_variable_name_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let toml = "dbento = 1\n[connections.p]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\nsecret = { env = \"\" }\n";
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let err = Workspace::open(dir.path()).expect_err("empty variable name");
    assert!(
        matches!(err, OpenError::Invalid { ref connection, .. } if connection == "p"),
        "{err:?}"
    );
}

#[test]
fn the_key_names_the_canonical_workspace_dir_and_connection() {
    let (_d, ws) = workspace();
    let key = SecretKey::for_connection(&ws, ws.connection("kc").unwrap());
    assert_eq!(key.workspace_dir(), ws.dir());
    assert_eq!(key.connection(), "kc");
}

#[test]
fn the_memory_store_round_trips_and_deletes() {
    let store = MemoryStore::new();
    let key = SecretKey::new("/w", "c");
    assert!(store.get(&key).unwrap().is_none());
    store.set(&key, &Secret::new("pw")).unwrap();
    assert_eq!(store.get(&key).unwrap().unwrap().expose(), "pw");
    assert!(store.get(&SecretKey::new("/other", "c")).unwrap().is_none());
    store.delete(&key).unwrap();
    store.delete(&key).unwrap();
    assert!(store.get(&key).unwrap().is_none());
}

#[test]
fn a_timed_store_passes_a_healthy_store_through() {
    let store = TimedStore::new(MemoryStore::new());
    let key = SecretKey::new("/w", "c");
    store.set(&key, &Secret::new("pw")).unwrap();
    assert_eq!(store.get(&key).unwrap().unwrap().expose(), "pw");
    store.delete(&key).unwrap();
    assert!(store.get(&key).unwrap().is_none());
}

#[test]
fn a_hanging_store_times_out_then_fails_fast() {
    let store = TimedStore::with_timeout(Broken::Hanging, Duration::from_millis(100));
    let key = SecretKey::new("/w", "c");
    let started = Instant::now();
    assert!(matches!(
        store.get(&key),
        Err(SecretError::Unavailable { .. })
    ));
    assert!(started.elapsed() < Duration::from_secs(5));
    let again = Instant::now();
    assert!(matches!(
        store.set(&key, &Secret::new("x")),
        Err(SecretError::Unavailable { .. })
    ));
    assert!(
        again.elapsed() < Duration::from_millis(50),
        "a stuck store fails fast"
    );
}

#[test]
fn a_panicking_store_is_a_failure_not_a_timeout() {
    let store = TimedStore::with_timeout(Broken::Panicking, Duration::from_secs(5));
    let started = Instant::now();
    assert!(matches!(
        store.get(&SecretKey::new("/w", "c")),
        Err(SecretError::Failed { .. })
    ));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "a dead worker is not waited out"
    );
}
