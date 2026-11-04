use dbento_core::{
    MemoryStore, Secret, SecretError, SecretKey, SecretStore, Workspace, secret_hint,
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
    let hint = secret_hint(&ws, ws.connection("kc").unwrap(), &MemoryStore::new());
    assert_eq!(hint.as_deref(), Some(KC));
}
#[test]
fn an_unavailable_keychain_says_why() {
    let (_d, ws) = ws();
    let hint = secret_hint(&ws, ws.connection("kc").unwrap(), &Locked).unwrap();
    assert_eq!(hint, format!("{KC}; the keychain is unavailable: locked"));
}
#[test]
fn a_dashed_name_becomes_an_underscored_variable() {
    let (_d, ws) = ws();
    let hint = secret_hint(&ws, ws.connection("my-db").unwrap(), &MemoryStore::new()).unwrap();
    assert!(hint.ends_with("or set DBENTO_SECRET_MY_DB"), "{hint}");
}
#[test]
fn other_connections_get_no_hint() {
    let (_d, ws) = ws();
    for name in ["none", "byenv", "lite"] {
        assert_eq!(
            secret_hint(&ws, ws.connection(name).unwrap(), &MemoryStore::new()),
            None,
            "{name}"
        );
    }
}
