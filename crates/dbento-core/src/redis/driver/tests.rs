use ::redis::{ConnectionAddr, ProtocolVersion, Value, parse_redis_value};

use super::{client, rustls_cause, server_info};
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
