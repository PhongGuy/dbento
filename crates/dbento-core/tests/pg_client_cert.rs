//! Postgres client-certificate auth against a server whose pg_hba admits the role only with a
//! certificate (`hostssl ... cert`): plain and encrypted keys connect, and a missing cert, a
//! missing passphrase or a wrong one fails with an error that says which.

use std::hash::{BuildHasher, RandomState};

use crate::support::{run, text};
use dbento_core::{ConnectError, Limits, Secret, SessionOptions, Workspace, connect};
use dbento_testkit::{CERT_USER, ClientCert, PgDb};
use pkcs8::der::pem::LineEnding;
use pkcs8::pkcs5::pbes2;

/// Random bytes from the std hasher's per-process keys; a test passphrase is never a literal.
fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    for (i, chunk) in out.chunks_mut(8).enumerate() {
        let v = RandomState::new().hash_one(i).to_le_bytes();
        chunk.copy_from_slice(&v[..chunk.len()]);
    }
    out
}

/// The client key encrypted as PKCS#8 PBES2 (PBKDF2-SHA256, AES-256-CBC), as openssl writes it.
fn encrypted_key(cert: &ClientCert, passphrase: &str) -> String {
    let info = pkcs8::PrivateKeyInfoRef::try_from(cert.key_der.as_slice()).unwrap();
    let (salt, iv) = (random_bytes::<16>(), random_bytes::<16>());
    let params = pbes2::Parameters::generate_pbkdf2_sha256_aes256cbc(2048, &salt, iv).unwrap();
    let doc = info.encrypt_with_params(params, passphrase).unwrap();
    doc.to_pem("ENCRYPTED PRIVATE KEY", LineEnding::LF)
        .unwrap()
        .to_string()
}

/// A workspace with one connection per case: `plain` (cert and unencrypted key), `locked`
/// (cert and encrypted key) and `nocert` (TLS only). None has a password.
fn workspace(db: &PgDb, cert: &ClientCert, locked_key: &str) -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    for (name, body) in [
        ("ca.pem", cert.ca_pem.as_str()),
        ("me.crt", &cert.cert_pem),
        ("me.key", &cert.key_pem),
        ("locked.key", locked_key),
    ] {
        std::fs::write(dir.path().join(name), body).unwrap();
    }
    let entry = |name: &str, extra: &str| {
        format!(
            "[connections.{name}]\nengine = \"postgres\"\nhost = \"{}\"\nport = {}\n\
             database = \"{}\"\nuser = \"{CERT_USER}\"\nsecret = \"none\"\ntls = \"verify\"\n\
             tls_ca = \"ca.pem\"\n{extra}",
            db.host(),
            db.port(),
            db.database()
        )
    };
    let toml = format!(
        "dbento = 1\n{}{}{}",
        entry("plain", "tls_cert = \"me.crt\"\ntls_key = \"me.key\"\n"),
        entry(
            "locked",
            "tls_cert = \"me.crt\"\ntls_key = \"locked.key\"\n"
        ),
        entry("nocert", ""),
    );
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    (dir, ws)
}

fn with_passphrase(passphrase: Option<&str>) -> SessionOptions {
    let mut opts = SessionOptions::default();
    opts.key_passphrase = passphrase.map(Secret::new);
    opts
}

async fn try_connect(
    ws: &Workspace,
    name: &str,
    passphrase: Option<&str>,
) -> Result<dbento_core::Session, ConnectError> {
    connect(
        ws,
        ws.declared(name).unwrap(),
        None,
        with_passphrase(passphrase),
    )
    .await
}

#[tokio::test]
async fn a_client_certificate_logs_in_with_a_plain_or_an_encrypted_key() {
    let (db, cert) = dbento_testkit::pg_client_cert().await;
    let passphrase = format!("{:x}", u128::from_le_bytes(random_bytes::<16>()));
    let (_dir, ws) = workspace(&db, &cert, &encrypted_key(&cert, &passphrase));
    for (name, passphrase) in [("plain", None), ("locked", Some(passphrase.as_str()))] {
        let mut s = try_connect(&ws, name, passphrase)
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let r = run(
            &mut s,
            "SELECT current_user, ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()",
            Limits::default(),
        )
        .await;
        assert_eq!(
            r[0].as_ref().unwrap().rows,
            vec![vec![text(CERT_USER), text("t")]],
            "{name}"
        );
    }
}

#[tokio::test]
async fn a_missing_cert_or_a_missing_or_wrong_passphrase_fails_clearly() {
    let (db, cert) = dbento_testkit::pg_client_cert().await;
    let passphrase = format!("{:x}", u128::from_le_bytes(random_bytes::<16>()));
    let (_dir, ws) = workspace(&db, &cert, &encrypted_key(&cert, &passphrase));

    // The server refuses the role without a certificate and says so.
    let err = try_connect(&ws, "nocert", None)
        .await
        .err()
        .expect("refused");
    assert!(matches!(err, ConnectError::Open { .. }), "{err:?}");
    assert!(err.to_string().contains("certificate"), "{err}");

    // An encrypted key without a passphrase names where to put one.
    let err = try_connect(&ws, "locked", None)
        .await
        .err()
        .expect("locked");
    assert!(matches!(err, ConnectError::Tls { .. }), "{err:?}");
    let says = err.to_string();
    for part in [
        "locked.key",
        "passphrase",
        "dbento secret set --tls-key locked",
        "DBENTO_KEY_PASSPHRASE_LOCKED",
    ] {
        assert!(says.contains(part), "{part}: {says}");
    }

    // A wrong passphrase is the key's error, not the server's.
    let wrong = format!("{passphrase}x");
    let err = try_connect(&ws, "locked", Some(&wrong))
        .await
        .err()
        .expect("wrong passphrase");
    assert!(matches!(err, ConnectError::Tls { .. }), "{err:?}");
    let says = err.to_string();
    assert!(
        says.contains("locked.key") && says.contains("wrong passphrase"),
        "{says}"
    );
    assert!(!says.contains(&wrong), "the passphrase leaked: {says}");
}
