use std::sync::Arc;

use ::redis::{ConnectionAddr, ProtocolVersion, Value, parse_redis_value};
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::TlsAcceptor;

use super::{client, rustls_cause, server_info, tls_wrap};
use crate::Secret;
use crate::session::{ConnectError, Flavor};
use crate::workspace::Workspace;

fn hello(server: &str, version: &str) -> Value {
    let (s, v) = (server.len(), version.len());
    let text =
        format!("%2\r\n$6\r\nserver\r\n${s}\r\n{server}\r\n$7\r\nversion\r\n${v}\r\n{version}\r\n");
    parse_redis_value(text.as_bytes()).unwrap()
}

#[test]
fn hello_names_the_flavor_and_the_version() {
    let v = server_info(&hello("valkey", "9.0.7")).unwrap();
    assert_eq!((v.flavor, v.version), (Flavor::Valkey, (9, 0, 7)));
    let r = server_info(&hello("redis", "7.2.4")).unwrap();
    assert_eq!((r.flavor, r.version), (Flavor::Redis, (7, 2, 4)));
    let old = server_info(&hello("redis", "6.2.14")).unwrap_err();
    assert!(old.contains("6.2.14"), "{old}");
}

fn workspace(keys: &str) -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    let toml = format!("dbento = 1\n[connections.c]\nengine = \"redis\"\n{keys}\n");
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    (dir, ws)
}

#[test]
fn the_client_carries_resp3_the_db_index_and_the_login() {
    let (_d, ws) = workspace(
        "host = \"cache.internal\"\nport = 6380\ndatabase = \"3\"\nuser = \"dbento_ro\"\ntls = \"verify\"",
    );
    let secret = Secret::new("pw");
    let c = client(ws.declared("c").unwrap(), Some(&secret), None).unwrap();
    let info = c.get_connection_info();
    assert!(
        matches!(info.addr(), ConnectionAddr::TcpTls { host, port: 6380, insecure: false, .. } if host == "cache.internal")
    );
    let r = info.redis_settings();
    assert_eq!(
        (r.db(), r.username(), r.password()),
        (3, Some("dbento_ro"), Some("pw"))
    );
    assert_eq!(r.protocol(), ProtocolVersion::RESP3);
}

#[test]
fn defaults_are_localhost_6379_db0_and_the_default_user() {
    let (_d, ws) = workspace("secret = \"none\"");
    let c = client(ws.declared("c").unwrap(), None, None).unwrap();
    let info = c.get_connection_info();
    assert!(matches!(info.addr(), ConnectionAddr::Tcp(h, 6379) if h == "127.0.0.1"));
    assert_eq!(
        (info.redis_settings().db(), info.redis_settings().username()),
        (0, None)
    );
}

#[test]
fn a_missing_ca_file_is_a_tls_error() {
    let (_d, ws) = workspace("tls = \"verify\"\ntls_ca = \"nope.pem\"\nsecret = \"none\"");
    assert!(matches!(
        client(ws.declared("c").unwrap(), None, None),
        Err(ConnectError::Tls { .. })
    ));
}

#[test]
fn a_rustls_error_is_found_stored_directly_or_inside_an_io_error() {
    let config = ::redis::RedisError::from(rustls::Error::General("config".into()));
    assert!(matches!(rustls_cause(&config), Some(rustls::Error::General(m)) if m == "config"));
    let io = std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        rustls::Error::General("handshake".into()),
    );
    let handshake = ::redis::RedisError::from(io);
    assert!(
        matches!(rustls_cause(&handshake), Some(rustls::Error::General(m)) if m == "handshake")
    );
    // A host that is no TLS server name is neither shape: it is `Open`, not `Tls`.
    let name = rustls::pki_types::ServerName::try_from("bad name").unwrap_err();
    assert!(rustls_cause(&::redis::RedisError::from(name)).is_none());
}

/// A server presenting `tls`'s certificate, demanding a client certificate from its CA when
/// `client_auth` is set.
fn acceptor(tls: &dbento_testkit::NamedTls, client_auth: bool) -> TlsAcceptor {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let pem = |text: &str| {
        CertificateDer::pem_slice_iter(text.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };
    let builder = rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .unwrap();
    let builder = if client_auth {
        let mut roots = rustls::RootCertStore::empty();
        roots.add_parsable_certificates(pem(&tls.ca_pem));
        let verifier =
            rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
                .build()
                .unwrap();
        builder.with_client_cert_verifier(verifier)
    } else {
        builder.with_no_client_auth()
    };
    let key = PrivateKeyDer::from_pem_slice(tls.key_pem.as_bytes()).unwrap();
    TlsAcceptor::from(Arc::new(
        builder.with_single_cert(pem(&tls.cert_pem), key).unwrap(),
    ))
}

#[tokio::test]
async fn the_tunneled_handshake_verifies_the_configured_host_against_tls_ca() {
    let tls = dbento_testkit::named_tls(&["cache.internal"]);
    for (host, ok) in [("cache.internal", true), ("other.internal", false)] {
        let (d, ws) = workspace(&format!(
            "host = \"{host}\"\ntls = \"verify\"\ntls_ca = \"ca.pem\"\nsecret = \"none\""
        ));
        std::fs::write(d.path().join("ca.pem"), &tls.ca_pem).unwrap();
        let (client, server_end) = tokio::io::duplex(64 * 1024);
        let acceptor = acceptor(&tls, false);
        let served = tokio::spawn(async move { acceptor.accept(server_end).await.is_ok() });
        match tls_wrap(ws.declared("c").unwrap(), None, client).await {
            Ok(stream) => {
                assert!(ok, "{host} was accepted");
                assert!(served.await.unwrap());
                drop(stream);
            }
            Err(ConnectError::Tls { message, .. }) => {
                assert!(!ok, "{host}: {message}");
                assert!(message.contains("tls_ca"), "{message}");
            }
            Err(other) => panic!("{host}: {other:?}"),
        }
    }
}

#[tokio::test]
async fn the_tunneled_handshake_presents_the_configured_client_certificate() {
    let tls = dbento_testkit::named_tls(&["cache.internal"]);
    for (pair, ok) in [(true, true), (false, false)] {
        let keys = if pair {
            "tls_cert = \"me.crt\"\ntls_key = \"me.key\"\n"
        } else {
            ""
        };
        let (d, ws) = workspace(&format!(
            "host = \"cache.internal\"\ntls = \"verify\"\ntls_ca = \"ca.pem\"\nsecret = \"none\"\n{keys}"
        ));
        std::fs::write(d.path().join("ca.pem"), &tls.ca_pem).unwrap();
        std::fs::write(d.path().join("me.crt"), &tls.client.cert_pem).unwrap();
        std::fs::write(d.path().join("me.key"), &tls.client.key_pem).unwrap();
        let (client, server_end) = tokio::io::duplex(64 * 1024);
        let acceptor = acceptor(&tls, true);
        let served = tokio::spawn(async move { acceptor.accept(server_end).await.is_ok() });
        // Under TLS 1.3 the client finishes first; the server's verdict is the one that counts.
        let wrapped = tls_wrap(ws.declared("c").unwrap(), None, client).await;
        assert_eq!(served.await.unwrap(), ok, "pair {pair}");
        drop(wrapped);
    }
}
