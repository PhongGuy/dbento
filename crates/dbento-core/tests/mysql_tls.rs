//! MySQL over TLS: verify with tls_ca connects and cancels over TLS, a client certificate
//! from a plain or encrypted key logs in, and a certificate failure names both ways out.
use std::hash::{BuildHasher, RandomState};
use std::time::Duration;

use crate::support::{Cell, my_fixtures, run};
use dbento_core::{ConnectError, ExecuteError, Limits, Secret, SessionOptions, Workspace, connect};
use dbento_testkit::{CERT_USER, ClientCert, MyDb, MyFlavor, my_tls};
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

fn workspace(db: &MyDb, cert: &ClientCert, locked_key: &str) -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    let files = [
        ("ca.pem", cert.ca_pem.as_str()),
        ("me.crt", &cert.cert_pem),
        ("me.key", &cert.key_pem),
        ("locked.key", locked_key),
    ];
    for (name, body) in files {
        std::fs::write(dir.path().join(name), body).unwrap();
    }
    let entry = |name: &str, user: &str, extra: &str| {
        format!(
            "[connections.{name}]\nengine = \"mysql\"\nhost = \"{}\"\nport = {}\ndatabase = \"{}\"\n\
             user = \"{user}\"\nsecret = \"none\"\ntls = \"verify\"\n{extra}",
            db.host(),
            db.port(),
            db.database()
        )
    };
    let ca = "tls_ca = \"ca.pem\"\n";
    let toml = format!(
        "dbento = 1\n{}{}{}{}{}",
        entry("root", db.user(), ca),
        entry("noca", db.user(), ""),
        entry(
            "plain",
            CERT_USER,
            &format!("{ca}tls_cert = \"me.crt\"\ntls_key = \"me.key\"\n")
        ),
        entry(
            "locked",
            CERT_USER,
            &format!("{ca}tls_cert = \"me.crt\"\ntls_key = \"locked.key\"\n")
        ),
        entry("nocert", CERT_USER, ca),
    );
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    (dir, ws)
}

fn names_both_ways_out(r: Result<dbento_core::Session, ConnectError>) -> bool {
    matches!(r, Err(ConnectError::Tls { ref message, .. })
        if message.contains("tls_ca") && message.contains("tls = \"off\""))
}

async fn tls_on(flavor: MyFlavor) {
    let (db, cert) = my_tls(flavor).await;
    let passphrase = format!("{:x}", RandomState::new().hash_one(7u8));
    let (_dir, ws) = workspace(&db, &cert, &encrypted_key(&cert, &passphrase));
    let root = || Some(Secret::new(db.password()));
    let conn = |name: &str| ws.connection(name).unwrap();
    let mut s = connect(&ws, conn("root"), root(), SessionOptions::default())
        .await
        .unwrap();
    let cipher = run(
        &mut s,
        "SHOW SESSION STATUS LIKE 'Ssl_cipher'",
        Limits::default(),
    )
    .await
    .remove(0)
    .unwrap();
    assert!(
        matches!(&cipher.rows[0][1], Cell::Text(c) if !c.is_empty()),
        "{flavor:?}: not encrypted"
    );
    let cancel = s.cancel_handle();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        cancel.cancel();
    });
    let r = tokio::time::timeout(
        Duration::from_secs(20),
        run(&mut s, "SELECT SLEEP(30)", Limits::default()),
    )
    .await
    .expect("the cancel never arrived over TLS");
    t.join().unwrap();
    assert!(
        matches!(r.last(), Some(Err(ExecuteError::Cancelled))),
        "{flavor:?}"
    );
    let mut locked = SessionOptions::default();
    locked.key_passphrase = Some(Secret::new(&passphrase));
    for (name, opts) in [("plain", SessionOptions::default()), ("locked", locked)] {
        let mut s = connect(&ws, conn(name), None, opts)
            .await
            .unwrap_or_else(|e| panic!("{flavor:?} {name}: {e}"));
        let user = run(&mut s, "SELECT CURRENT_USER()", Limits::default())
            .await
            .remove(0)
            .unwrap();
        assert!(
            matches!(&user.rows[0][0], Cell::Text(u) if u.starts_with(CERT_USER)),
            "{flavor:?} {name}"
        );
    }
    match connect(&ws, conn("nocert"), None, SessionOptions::default()).await {
        Err(ConnectError::Open { message }) => assert!(message.contains("1045"), "{message}"),
        other => panic!("{flavor:?}: {:?}", other.err()),
    }
    assert!(
        names_both_ways_out(connect(&ws, conn("noca"), root(), SessionOptions::default()).await),
        "{flavor:?}"
    );
}

#[tokio::test]
async fn mysql_over_verified_tls() {
    tls_on(MyFlavor::Mysql).await;
}

#[tokio::test]
async fn mariadb_over_verified_tls() {
    tls_on(MyFlavor::Mariadb).await;
}

#[tokio::test]
async fn an_untrusted_gate_server_certificate_names_tls_ca_and_tls_off() {
    for (label, fx) in my_fixtures("tls = \"verify\"").await {
        assert!(
            names_both_ways_out(fx.try_session(SessionOptions::default()).await),
            "{label}"
        );
    }
}

#[tokio::test]
async fn a_tls_ca_without_certificates_is_refused_before_connecting() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("empty.pem"), "").unwrap();
    let toml = "dbento = 1\n[connections.c]\nengine = \"mysql\"\nhost = \"127.0.0.1\"\nport = 1\n\
                user = \"u\"\nsecret = \"none\"\ntls = \"verify\"\ntls_ca = \"empty.pem\"\n";
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    match connect(
        &ws,
        ws.connection("c").unwrap(),
        None,
        SessionOptions::default(),
    )
    .await
    {
        Err(ConnectError::Tls { message, .. }) => {
            assert!(
                message.contains("no certificates") && message.contains("empty.pem"),
                "{message}"
            )
        }
        other => panic!("{:?}", other.err()),
    }
}

#[tokio::test]
async fn a_certificate_for_another_host_name_names_tls_ca_and_tls_off() {
    let (db, cert) = my_tls(MyFlavor::Mysql).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ca.pem"), &cert.ca_pem).unwrap();
    // The certificate names localhost, so the same server by its address must fail verification.
    let toml = format!(
        "dbento = 1\n[connections.c]\nengine = \"mysql\"\nhost = \"127.0.0.1\"\nport = {}\n\
         database = \"{}\"\nuser = \"{}\"\nsecret = \"none\"\ntls = \"verify\"\ntls_ca = \"ca.pem\"\n",
        db.port(),
        db.database(),
        db.user()
    );
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    let r = connect(
        &ws,
        ws.connection("c").unwrap(),
        Some(Secret::new(db.password())),
        SessionOptions::default(),
    )
    .await;
    assert!(names_both_ways_out(r));
}
