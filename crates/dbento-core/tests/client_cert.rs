//! TLS client certificates without a server: the `tls_cert` and `tls_key` keys, the key
//! passphrase's keychain slot and environment fallback, and the edit layer's round trip.

use dbento_core::{
    ConnectionSpec, Edit, MemoryStore, OpenError, Secret, SecretError, SecretKey, SecretStore,
    Workspace, key_passphrase_env_var, plan_edit, resolve_key_passphrase,
};
use std::fs;

const PG: &str = "dbento = 1\n[connections.p]\nengine = \"postgres\"\nhost = \"db.example\"\ndatabase = \"d\"\nuser = \"u\"\n";

fn ws_with(toml: &str) -> (tempfile::TempDir, Result<Workspace, OpenError>) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path());
    (dir, ws)
}

#[test]
fn cert_and_key_resolve_against_the_workspace() {
    let (dir, ws) = ws_with(&format!(
        "{PG}tls_cert = \"certs/me.crt\"\ntls_key = \"certs/me.key\"\n"
    ));
    let ws = ws.unwrap();
    let conn = ws.connection("p").unwrap();
    // The workspace root is canonical, and a temp dir may sit behind a symlink (macOS /var).
    let root = fs::canonicalize(dir.path()).unwrap();
    assert_eq!(conn.tls_cert(), Some(root.join("certs/me.crt").as_path()));
    assert_eq!(conn.tls_key(), Some(root.join("certs/me.key").as_path()));
    let spec = ws.connection_spec("p").unwrap();
    assert_eq!(spec.tls_cert.as_deref(), Some("certs/me.crt"));
    assert_eq!(spec.tls_key.as_deref(), Some("certs/me.key"));
}

#[test]
fn cert_and_key_come_together_over_verified_tls_on_postgres_only() {
    for (toml, says) in [
        (format!("{PG}tls_cert = \"c.crt\"\n"), "tls_key"),
        (format!("{PG}tls_key = \"c.key\"\n"), "tls_cert"),
        (
            format!("{PG}tls = \"off\"\ntls_cert = \"c.crt\"\ntls_key = \"c.key\"\n"),
            "verify",
        ),
        (
            "dbento = 1\n[connections.p]\nengine = \"postgres\"\nhost = \"localhost\"\ndatabase = \"d\"\nuser = \"u\"\ntls_cert = \"c.crt\"\ntls_key = \"c.key\"\n".to_owned(),
            "verify",
        ),
        (
            "dbento = 1\n[connections.s]\nengine = \"sqlite\"\npath = \"x.db\"\ntls_cert = \"c.crt\"\n".to_owned(),
            "postgres key",
        ),
    ] {
        let err = ws_with(&toml).1.expect_err("should be rejected");
        assert!(matches!(err, OpenError::Invalid { .. }), "{toml}: {err:?}");
        assert!(err.to_string().contains(says), "{toml}: {err}");
    }
}

const ENCRYPTED: &str =
    "-----BEGIN ENCRYPTED PRIVATE KEY-----\nAAAA\n-----END ENCRYPTED PRIVATE KEY-----\n";
const PLAIN: &str = "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n";

fn cert_ws(key_pem: &str) -> (tempfile::TempDir, Workspace) {
    let (dir, ws) = ws_with(&format!(
        "{PG}tls_cert = \"me.crt\"\ntls_key = \"me.key\"\n[connections.bare]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\n"
    ));
    fs::write(dir.path().join("me.key"), key_pem).unwrap();
    (dir, ws.unwrap())
}

/// A store that fails every call, to prove it is never asked.
struct Refusing;
impl SecretStore for Refusing {
    fn get(&self, _: &SecretKey) -> Result<Option<Secret>, SecretError> {
        Err(SecretError::Failed {
            message: "asked".into(),
        })
    }
    fn set(&self, _: &SecretKey, _: &Secret) -> Result<(), SecretError> {
        unreachable!()
    }
    fn delete(&self, _: &SecretKey) -> Result<(), SecretError> {
        unreachable!()
    }
}

fn no_env(_: &str) -> Option<String> {
    None
}

#[test]
fn an_unencrypted_key_or_no_key_never_asks_the_store() {
    let (_d, ws) = cert_ws(PLAIN);
    for name in ["p", "bare"] {
        let conn = ws.connection(name).unwrap();
        let found = resolve_key_passphrase(&ws, conn, &Refusing, &no_env).unwrap();
        assert!(found.is_none(), "{name}");
    }
}

#[test]
fn an_encrypted_key_reads_its_own_keychain_slot_then_the_environment() {
    let (_d, ws) = cert_ws(ENCRYPTED);
    let conn = ws.connection("p").unwrap();
    let store = MemoryStore::new();
    // The password's slot is not the passphrase's.
    store
        .set(&SecretKey::for_connection(&ws, conn), &Secret::new("pw"))
        .unwrap();
    assert!(
        resolve_key_passphrase(&ws, conn, &store, &no_env)
            .unwrap()
            .is_none()
    );
    let var = key_passphrase_env_var("p");
    assert_eq!(var, "DBENTO_KEY_PASSPHRASE_P");
    let env = |v: &str| (v == "DBENTO_KEY_PASSPHRASE_P").then(|| "from-env".to_owned());
    let found = resolve_key_passphrase(&ws, conn, &store, &env).unwrap();
    assert_eq!(found.unwrap().expose(), "from-env");
    let key = SecretKey::key_passphrase_for(&ws, conn);
    assert_ne!(key, SecretKey::for_connection(&ws, conn));
    store.set(&key, &Secret::new("from-keychain")).unwrap();
    let found = resolve_key_passphrase(&ws, conn, &store, &env).unwrap();
    assert_eq!(found.unwrap().expose(), "from-keychain");
    // A rename keeps the slot.
    assert_eq!(key.renamed("q"), SecretKey::key_passphrase(ws.dir(), "q"));
}

#[test]
fn an_edit_writes_and_removes_cert_and_key() {
    let (_d, ws) = ws_with(PG);
    let ws = ws.unwrap();
    let mut spec = ws.connection_spec("p").unwrap();
    spec.tls_cert = Some("me.crt".into());
    spec.tls_key = Some("me.key".into());
    let edit = |spec: ConnectionSpec| Edit::PutConnection {
        previous: Some("p".into()),
        name: "p".into(),
        spec,
    };
    let plan = plan_edit(&ws, &edit(spec.clone())).unwrap();
    assert!(
        plan.new_text()
            .contains("tls_cert = \"me.crt\"\ntls_key = \"me.key\"\n")
    );
    let after = plan.workspace();
    assert_eq!(after.connection_spec("p").unwrap(), spec);
    let mut cleared = spec.clone();
    (cleared.tls_cert, cleared.tls_key) = (None, None);
    let plan = plan_edit(after, &edit(cleared)).unwrap();
    assert!(!plan.new_text().contains("tls_"), "{}", plan.new_text());
}

#[test]
fn an_edit_writes_reads_back_and_removes_the_ca() {
    let (_d, ws) = ws_with(&format!("{PG}tls_ca = \"certs/ca.pem\"\n"));
    let ws = ws.unwrap();
    let mut spec = ws.connection_spec("p").unwrap();
    assert_eq!(spec.tls_ca.as_deref(), Some("certs/ca.pem"));
    spec.tls_ca = Some("other/ca.pem".into());
    let edit = |spec: ConnectionSpec| Edit::PutConnection {
        previous: Some("p".into()),
        name: "p".into(),
        spec,
    };
    let plan = plan_edit(&ws, &edit(spec.clone())).unwrap();
    assert!(plan.new_text().contains("tls_ca = \"other/ca.pem\"\n"));
    let after = plan.workspace();
    assert_eq!(after.connection_spec("p").unwrap(), spec);
    let conn = after.connection("p").unwrap();
    assert_eq!(
        conn.tls_ca(),
        Some(after.dir().join("other/ca.pem").as_path())
    );
    spec.tls_ca = None;
    let plan = plan_edit(after, &edit(spec)).unwrap();
    assert!(!plan.new_text().contains("tls_ca"), "{}", plan.new_text());
}
