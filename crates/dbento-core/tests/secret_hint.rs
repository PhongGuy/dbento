use dbento_core::{
    HintAudience, MemoryStore, Secret, SecretError, SecretKey, SecretStore, Workspace, secret_hint,
    secret_hint_for,
};

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

fn ws() -> (tempfile::TempDir, Workspace) {
    let pg = |name: &str, extra: &str| {
        format!(
            "[connections.{name}]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\n{extra}\n"
        )
    };
    let toml = format!(
        "dbento = 1\n{}{}{}{}[connections.lite]\nengine = \"sqlite\"\npath = \"x.db\"\n",
        pg("kc", ""),
        pg("my-db", ""),
        pg("none", "secret = \"none\""),
        pg("byenv", "secret = { env = \"APP_PW\" }"),
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    (dir, ws)
}

const KC: &str = "no secret was found for 'kc': run `dbento secret set kc` or set DBENTO_SECRET_KC";

#[test]
fn a_keychain_connection_names_both_places() {
    let (_d, ws) = ws();
    let hint = secret_hint(&ws, ws.declared("kc").unwrap(), &MemoryStore::new());
    assert_eq!(hint.as_deref(), Some(KC));
}
#[test]
fn an_unavailable_keychain_says_why() {
    let (_d, ws) = ws();
    let hint = secret_hint(&ws, ws.declared("kc").unwrap(), &Locked).unwrap();
    assert_eq!(hint, format!("{KC}; the keychain is unavailable: locked"));
}
#[test]
fn a_dashed_name_becomes_an_underscored_variable() {
    let (_d, ws) = ws();
    let hint = secret_hint(&ws, ws.declared("my-db").unwrap(), &MemoryStore::new()).unwrap();
    assert!(hint.ends_with("or set DBENTO_SECRET_MY_DB"), "{hint}");
}
#[test]
fn other_connections_get_no_hint() {
    let (_d, ws) = ws();
    for name in ["none", "byenv", "lite"] {
        assert_eq!(
            secret_hint(&ws, ws.declared(name).unwrap(), &MemoryStore::new()),
            None,
            "{name}"
        );
    }
}

const KC_LEGACY: &str = "the saved password for 'kc' was stored before passwords were tied to their server; run `dbento secret pin kc` to confirm this server, or `dbento secret set kc`";
const KC_APP: &str =
    "no password is saved for 'kc': edit the connection to enter it, or set DBENTO_SECRET_KC";
const KC_APP_LEGACY: &str = "the saved password for 'kc' is not sent until you confirm this server: edit the connection to enter it";

fn with_legacy(ws: &Workspace, owner: &str) -> MemoryStore {
    let store = MemoryStore::new();
    store
        .set(
            &SecretKey::legacy_password(ws.dir(), owner),
            &Secret::new("x"),
        )
        .unwrap();
    store
}

#[test]
fn each_audience_has_its_own_miss_text() {
    let (_d, ws) = ws();
    let kc = ws.declared("kc").unwrap();
    let empty = MemoryStore::new();
    assert_eq!(
        secret_hint_for(&ws, kc, &empty, HintAudience::Cli).as_deref(),
        Some(KC)
    );
    assert_eq!(
        secret_hint_for(&ws, kc, &empty, HintAudience::App).as_deref(),
        Some(KC_APP)
    );
}

#[test]
fn a_legacy_entry_changes_the_hint_for_both_audiences() {
    let (_d, ws) = ws();
    let kc = ws.declared("kc").unwrap();
    let store = with_legacy(&ws, "kc");
    assert_eq!(
        secret_hint_for(&ws, kc, &store, HintAudience::Cli).as_deref(),
        Some(KC_LEGACY)
    );
    assert_eq!(
        secret_hint(&ws, kc, &store).as_deref(),
        Some(KC_LEGACY),
        "secret_hint is Cli"
    );
    assert_eq!(
        secret_hint_for(&ws, kc, &store, HintAudience::App).as_deref(),
        Some(KC_APP_LEGACY)
    );
}

#[test]
fn an_unavailable_keychain_says_why_for_both_audiences() {
    let (_d, ws) = ws();
    let kc = ws.declared("kc").unwrap();
    let why = "; the keychain is unavailable: locked";
    assert_eq!(
        secret_hint_for(&ws, kc, &Locked, HintAudience::Cli),
        Some(format!("{KC}{why}"))
    );
    assert_eq!(
        secret_hint_for(&ws, kc, &Locked, HintAudience::App),
        Some(format!("{KC_APP}{why}"))
    );
}

#[test]
fn a_child_finds_the_legacy_entry_of_its_parent() {
    let dir = tempfile::tempdir().unwrap();
    let toml = "dbento = 1\n[connections.kc]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\ndatabases = [\"app\"]\n";
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    let child = ws.connection("kc/app").unwrap();
    let store = with_legacy(&ws, "kc");
    assert_eq!(
        secret_hint_for(&ws, &child, &store, HintAudience::Cli).as_deref(),
        Some(KC_LEGACY)
    );
}

#[test]
fn other_connections_get_no_hint_in_the_app_either() {
    let (_d, ws) = ws();
    for name in ["none", "byenv", "lite"] {
        let conn = ws.declared(name).unwrap();
        assert_eq!(
            secret_hint_for(&ws, conn, &MemoryStore::new(), HintAudience::App),
            None,
            "{name}"
        );
    }
}
