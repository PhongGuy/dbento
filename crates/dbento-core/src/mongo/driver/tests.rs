use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bson::doc;
use mongodb::options::{ClientOptions, Tls};

use super::*;
use crate::session::{Unguarded, Writes};

mod unix_member;

fn set(actions: &[&str]) -> BTreeSet<String> {
    actions.iter().map(|a| (*a).to_owned()).collect()
}

#[test]
fn only_a_non_empty_list_inside_the_reader_set_is_database_enforcement() {
    assert_eq!(enforcement_of(&set(&[])), ReadOnlyEnforcement::BestEffort);
    assert_eq!(
        enforcement_of(&set(&READER_ACTIONS)),
        ReadOnlyEnforcement::Database
    );
    assert_eq!(
        enforcement_of(&set(&["find"])),
        ReadOnlyEnforcement::Database
    );
    for extra in ["insert", "aNewActionInALaterServer"] {
        assert_eq!(
            enforcement_of(&set(&["find", extra])),
            ReadOnlyEnforcement::BestEffort
        );
    }
}

#[test]
fn privileges_are_the_union_over_every_resource() {
    let reply = doc! { "authInfo": { "authenticatedUserPrivileges": [
        { "resource": { "db": "app", "collection": "" }, "actions": ["find", "listIndexes"] },
        { "resource": { "db": "app", "collection": "system.js" }, "actions": ["find", "dbHash"] },
    ] }, "ok": 1 };
    assert_eq!(
        privilege_actions(&reply),
        set(&["dbHash", "find", "listIndexes"])
    );
    assert_eq!(privilege_actions(&doc! { "ok": 1 }), set(&[]));
}

#[test]
fn the_version_is_its_first_three_parts() {
    assert_eq!(version_of("8.0.34"), Some((8, 0, 34)));
    assert_eq!(version_of("7.0.2-rc1"), Some((7, 0, 2)));
    assert_eq!(version_of("eight"), None);
}

#[tokio::test]
async fn relative_tls_files_resolve_against_the_workspace_and_absolute_ones_stay() {
    let uri = "mongodb://h/?tls=true&tlsCAFile=certs/ca.pem&tlsCertificateKeyFile=/abs/k.pem";
    let mut options = ClientOptions::parse(uri).await.unwrap();
    resolve_tls_paths(&mut options, Path::new("/ws"));
    let Some(Tls::Enabled(tls)) = options.tls else {
        panic!("tls is on")
    };
    assert_eq!(
        tls.ca_file_path.as_deref(),
        Some(Path::new("/ws/certs/ca.pem"))
    );
    assert_eq!(
        tls.cert_key_file_path.as_deref(),
        Some(Path::new("/abs/k.pem"))
    );
}

#[tokio::test]
async fn the_connect_timeouts_fill_what_the_uri_left_unset_and_bound_the_handshake() {
    let mut unset = ClientOptions::parse("mongodb://h/").await.unwrap();
    assert_eq!(fill_timeouts(&mut unset), Duration::from_secs(20));
    assert_eq!(unset.server_selection_timeout, Some(CONNECT_TIMEOUT));
    assert_eq!(unset.connect_timeout, Some(CONNECT_TIMEOUT));

    let uri = "mongodb://h/?serverSelectionTimeoutMS=2000&connectTimeoutMS=3000";
    let mut set = ClientOptions::parse(uri).await.unwrap();
    assert_eq!(fill_timeouts(&mut set), Duration::from_secs(12));
    assert_eq!(set.server_selection_timeout, Some(Duration::from_secs(2)));
    assert_eq!(set.connect_timeout, Some(Duration::from_secs(3)));
}

#[tokio::test]
async fn a_server_that_stalls_after_it_is_selected_fails_the_connect() {
    let stalled = std::future::pending::<Result<(), ConnectError>>();
    let failure = within(Duration::from_millis(10), "h:1".to_owned(), stalled).await;
    match failure {
        Err(ConnectError::Open { message }) => {
            assert!(
                message.starts_with("h:1: the server did not answer"),
                "{message}"
            );
        }
        other => panic!("{other:?}"),
    }
}

fn class(writes: Writes) -> Classification {
    Classification {
        writes,
        unguarded: Vec::<Unguarded>::new(),
        transaction_control: false,
        categories: Vec::new(),
    }
}

#[test]
fn the_gate_refuses_by_level() {
    use ReadOnlyEnforcement::{BestEffort, Database};
    let cases = [
        (Writes::No, Some(BestEffort), false),
        (Writes::No, Some(Database), false),
        (Writes::No, None, false),
        (Writes::Yes, None, false),
        (Writes::Unknown, None, false),
        (Writes::Yes, Some(Database), true),
        (Writes::Unknown, Some(Database), false),
        (Writes::Yes, Some(BestEffort), true),
        (Writes::Unknown, Some(BestEffort), true),
    ];
    for (writes, level, want) in cases {
        let got = refused(&class(writes), level);
        assert_eq!(
            matches!(got, Some(ExecuteError::ReadOnlyViolation { .. })),
            want
        );
    }
}

#[tokio::test]
async fn a_cancel_before_the_call_stops_it_and_is_spent_by_that() {
    let cancel = MongoCancel::new();
    cancel.cancel();
    assert!(matches!(
        cancel.guard(async { 1 }).await,
        Err(ExecuteError::Cancelled)
    ));
    assert_eq!(cancel.guard(async { 1 }).await.unwrap(), 1);
    cancel.cancel();
    cancel.spend();
    assert_eq!(cancel.guard(async { 1 }).await.unwrap(), 1);
}

#[tokio::test]
async fn a_cancel_during_the_call_drops_it() {
    let cancel = Arc::new(MongoCancel::new());
    let other = Arc::clone(&cancel);
    let stop = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        other.cancel();
    });
    let waited = cancel.guard(std::future::pending::<()>()).await;
    assert!(matches!(waited, Err(ExecuteError::Cancelled)));
    stop.await.unwrap();
}

// A client whose every command fails server selection, without a server or a wait.
async fn lost_server() -> MongoDriver {
    let uri = "mongodb://127.0.0.1:1/?directConnection=true&serverSelectionTimeoutMS=50";
    let client = Client::with_options(ClientOptions::parse(uri).await.unwrap()).unwrap();
    MongoDriver {
        client,
        socks: None,
        server: ServerInfo {
            flavor: Flavor::MongoDb,
            version: (8, 0, 0),
        },
        uri_database: "test".to_owned(),
        shared: Arc::new(Shared {
            database: Mutex::new("test".to_owned()),
            enforcement: None,
            timeout: None,
            closed: AtomicBool::new(false),
            settings_changed: AtomicBool::new(false),
        }),
        cancel: Arc::new(MongoCancel::new()),
    }
}

#[tokio::test]
async fn a_catalog_call_that_loses_the_server_closes_the_session() {
    let mut driver = lost_server().await;
    let closed = |e| matches!(e, IntrospectError::Execute(ExecuteError::SessionClosed));
    assert!(closed(driver.schemas().await.unwrap_err()));
    assert!(driver.shared.closed.load(Ordering::Relaxed));
    assert!(closed(driver.relations("test").await.unwrap_err()));
}

#[tokio::test]
async fn a_write_that_loses_the_server_closes_the_session() {
    let mut driver = lost_server().await;
    let mut script = driver.start_script("db.t.insertOne({a: 1})\ndb.t.insertOne({a: 2})");
    MongoDriver::prepare_next(&mut script)
        .await
        .unwrap()
        .unwrap();
    let failure = MongoDriver::execute(&mut script, Limits::default())
        .await
        .err();
    assert!(matches!(failure, Some(ExecuteError::SessionClosed)));
    assert!(matches!(
        MongoDriver::prepare_next(&mut script).await,
        Some(Err(ExecuteError::SessionClosed))
    ));
    assert!(matches!(
        driver.settle().await,
        Err(ExecuteError::SessionClosed)
    ));
}

// Cancelled before it is sent, so the dead server never answers and the session stays open.
#[tokio::test]
async fn an_unspent_cancel_stops_a_write_before_it_is_sent() {
    let mut driver = lost_server().await;
    let mut script = driver.start_script("db.t.insertOne({a: 1})");
    MongoDriver::prepare_next(&mut script)
        .await
        .unwrap()
        .unwrap();
    driver.cancel.cancel();
    let failure = MongoDriver::execute(&mut script, Limits::default())
        .await
        .err();
    assert!(matches!(failure, Some(ExecuteError::Cancelled)));
    assert!(driver.settle().await.is_ok());
}

fn method_op(src: &str) -> MongoOp {
    parse_script(src).remove(0).unwrap().op
}

#[test]
fn a_shell_method_fails_on_a_failure_inside_an_ok_reply() {
    let op = method_op("db.w.findOneAndDelete({})");
    let reply = doc! {
        "ok": 1,
        "value": { "_id": 1 },
        "writeConcernError": { "code": 64, "codeName": "WriteConcernFailed", "errmsg": "late" },
    };
    match shown(&op, reply) {
        Err(ExecuteError::Database { code, .. }) => assert_eq!(code, "WriteConcernFailed"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn run_command_shows_a_failed_reply_except_a_timeout() {
    let op = method_op("db.runCommand({insert: 'w', documents: [{}]})");
    let failed = |code: i32, n: i32| {
        doc! { "n": n, "ok": 1, "writeErrors": [{ "index": 0, "code": code, "errmsg": "x" }] }
    };
    assert!(shown(&op, failed(11000, 0)).unwrap().is_some());
    assert!(matches!(
        shown(&op, failed(50, 0)),
        Err(ExecuteError::Timeout)
    ));
}

#[test]
fn find_one_and_modify_shows_its_value_or_nothing() {
    let op = method_op("db.w.findOneAndDelete({})");
    let found = doc! { "lastErrorObject": { "n": 1 }, "value": { "_id": 1 }, "ok": 1 };
    assert_eq!(shown(&op, found).unwrap(), Some(doc! { "_id": 1 }));
    let missing = doc! { "lastErrorObject": { "n": 0 }, "value": null, "ok": 1 };
    assert_eq!(shown(&op, missing).unwrap(), None);
}

fn mongo_conn() -> ConnectionConfig {
    let text =
        "dbento = 1\n[connections.c]\nengine = \"mongodb\"\nuri = \"mongodb://127.0.0.1:1/\"\n";
    let ws = crate::workspace::Workspace::from_text(
        std::path::PathBuf::from("/ws"),
        Path::new("/ws/dbento.toml"),
        text,
    )
    .unwrap();
    ws.declared("c").unwrap().clone()
}

#[tokio::test]
async fn a_recorded_ssh_refusal_replaces_a_selection_timeout_and_nothing_else() {
    let driver = lost_server().await;
    let e = driver
        .client
        .database("admin")
        .run_command(doc! { "ping": 1 })
        .await
        .unwrap_err();
    assert!(
        matches!(&*e.kind, ErrorKind::ServerSelection { .. }),
        "{:?}",
        e.kind
    );
    let refusal = "the SSH server would not open a channel to m9:27017 (resolved on the SSH server): it could not connect there";
    let tunnelled = connect_error(&mongo_conn(), Some(refusal.to_owned()), true, e.clone());
    assert!(
        matches!(&tunnelled, ConnectError::SshTunnel { message } if message == refusal),
        "{tunnelled:?}"
    );
    assert!(matches!(
        connect_error(&mongo_conn(), None, false, e),
        ConnectError::Open { .. }
    ));
}

// The proxy reaches the server once: the monitor's connection is served, then the listener is
// gone, so the command's own connection is refused by the endpoint, which records why.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recorded_ssh_refusal_replaces_a_proxy_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let me = format!("127.0.0.1:{port}");
    let mut hosts = bson::RawArrayBuf::new();
    hosts.push(me.as_str());
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        drop(listener);
        unix_member::serve(socket, me, hosts).await;
    });
    let ep = SocksEndpoint::start(Dialer::Direct).await.unwrap();
    let uri = format!(
        "mongodb://127.0.0.1:{port}/?directConnection=true&retryReads=false&serverSelectionTimeoutMS=2000"
    );
    let mut options = ClientOptions::parse(uri).await.unwrap();
    options.socks5_proxy = Some(proxy_of(&ep));
    let client = Client::with_options(options).unwrap();
    let e = client
        .database("admin")
        .run_command(doc! { "ping": 1 })
        .await
        .unwrap_err();
    assert!(
        matches!(&*e.kind, ErrorKind::ProxyConnect { .. }),
        "{:?}",
        e.kind
    );
    let refusal = ep.last_refusal().expect("the endpoint recorded no refusal");
    let tunnelled = connect_error(&mongo_conn(), Some(refusal.clone()), true, e.clone());
    assert!(
        matches!(&tunnelled, ConnectError::SshTunnel { message } if *message == refusal),
        "{tunnelled:?}"
    );
    assert!(matches!(
        connect_error(&mongo_conn(), None, true, e),
        ConnectError::Open { .. }
    ));
}

/// A UDP nameserver on loopback that answers every question with NXDOMAIN.
async fn nxdomain_server() -> std::net::SocketAddr {
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        while let Ok((n, peer)) = socket.recv_from(&mut buf).await {
            // The query echoed back as a response: QR set, recursion available, rcode 3.
            let mut reply = buf[..n].to_vec();
            if reply.len() < 12 {
                continue;
            }
            reply[2] |= 0x80;
            reply[3] = (reply[3] & 0x70) | 0x80 | 0x03;
            let _ = socket.send_to(&reply, peer).await;
        }
    });
    addr
}

// The SRV rewording names the SSH server, so a direct connection keeps the driver's own text.
#[tokio::test]
async fn a_direct_srv_lookup_that_fails_keeps_the_plain_open_error() {
    let text = "dbento = 1\n[connections.c]\nengine = \"mongodb\"\nuri = \"mongodb+srv://gone.example.test/app\"\n";
    let ws = crate::workspace::Workspace::from_text(
        std::path::PathBuf::from("/ws"),
        Path::new("/ws/dbento.toml"),
        text,
    )
    .unwrap();
    let conn = ws.declared("c").unwrap();
    let opts = crate::session::SessionOptions {
        mongo_dns: Some(nxdomain_server().await),
        ..Default::default()
    };
    let err = crate::session::connect(&ws, conn, None, opts)
        .await
        .err()
        .expect("an unresolvable SRV name connected");
    let ConnectError::Open { message } = &err else {
        panic!("{err:?}");
    };
    assert!(
        message.starts_with(&format!("{}: ", conn.display_target()))
            && message.contains("DNS resolution")
            && !message.contains("SSH server"),
        "{message}"
    );
}
