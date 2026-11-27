//! A replica set that advertises a member as a local `*.sock` path: the member is dialled only
//! when the connection names that socket itself, with or without the SSH proxy.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use bson::{RawArrayBuf, RawDocument, RawDocumentBuf, rawdoc};
use mongodb::Client;
use mongodb::options::ClientOptions;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener};

use super::super::{connect_error, proxy_of};
use super::mongo_conn;
use crate::session::ConnectError;
use crate::ssh::Dialer;
use crate::ssh::socks::SocksEndpoint;

/// How long the monitors get to see the advertised member after the first command.
const SETTLE: Duration = Duration::from_secs(2);

// A primary of set `rs0` at `me` that reports `hosts` as the set's members.
fn primary_hello(me: &str, hosts: &RawArrayBuf) -> RawDocumentBuf {
    rawdoc! {
        "helloOk": true,
        "isWritablePrimary": true,
        "ismaster": true,
        "secondary": false,
        "setName": "rs0",
        "hosts": hosts.clone(),
        "me": me,
        "maxBsonObjectSize": 16_777_216,
        "maxMessageSizeBytes": 48_000_000,
        "maxWriteBatchSize": 100_000,
        "minWireVersion": 0,
        "maxWireVersion": 21,
        "ok": 1.0,
    }
}

// A writable server in no replica set.
fn standalone_hello() -> RawDocumentBuf {
    rawdoc! {
        "helloOk": true,
        "isWritablePrimary": true,
        "ismaster": true,
        "maxBsonObjectSize": 16_777_216,
        "maxMessageSizeBytes": 48_000_000,
        "maxWriteBatchSize": 100_000,
        "minWireVersion": 0,
        "maxWireVersion": 21,
        "ok": 1.0,
    }
}

// A primary of set `rs0` reached on the socket at `path`, listing only that socket.
fn set_of_itself(path: &str) -> RawDocumentBuf {
    let mut hosts = RawArrayBuf::new();
    hosts.push(path);
    primary_hello(path, &hosts)
}

// Answers requests as the primary of `rs0` at `me` that lists `hosts`.
pub(super) async fn serve<S>(socket: S, me: String, hosts: RawArrayBuf)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    serve_hello(socket, primary_hello(&me, &hosts)).await;
}

// Reads OP_MSG requests until the client hangs up, answering a handshake with `hello` and any
// other command with `ok`.
async fn serve_hello<S>(mut socket: S, hello: RawDocumentBuf)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut header = [0; 16];
    while socket.read_exact(&mut header).await.is_ok() {
        let len = i32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
        let mut body = vec![0; len - 16];
        if socket.read_exact(&mut body).await.is_err() {
            return;
        }
        // Flags, then the payload-0 section's kind byte, then the command document.
        let command = RawDocument::from_bytes(&body[5..]).unwrap();
        let name = command
            .iter_elements()
            .next()
            .unwrap()
            .unwrap()
            .key()
            .to_string();
        let reply = match name.as_str() {
            "hello" | "isMaster" | "ismaster" => hello.clone(),
            _ => rawdoc! { "ok": 1.0 },
        };
        let total = 16 + 4 + 1 + reply.as_bytes().len();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&(total as i32).to_le_bytes());
        out.extend_from_slice(&1_i32.to_le_bytes());
        out.extend_from_slice(&header[4..8]);
        out.extend_from_slice(&2013_i32.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0, 0]);
        out.extend_from_slice(reply.as_bytes());
        if socket.write_all(&out).await.is_err() {
            return;
        }
    }
}

/// The fake primary's port, serving until the test's runtime ends. It lists the socket at
/// `path` as a member, and itself too when `listed`.
async fn fake_primary(path: &Path, listed: bool) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let me = format!("127.0.0.1:{port}");
    let mut hosts = RawArrayBuf::new();
    if listed {
        hosts.push(me.as_str());
    }
    hosts.push(path.to_str().unwrap());
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            tokio::spawn(serve(socket, me.clone(), hosts.clone()));
        }
    });
    port
}

/// Driver options for the set at `port`, proxied through `endpoint` exactly as a tunnelled
/// connect sets them.
async fn set_options(port: u16, endpoint: Option<&SocksEndpoint>) -> ClientOptions {
    let uri = format!("mongodb://127.0.0.1:{port}/?replicaSet=rs0&serverSelectionTimeoutMS=2000");
    let mut options = ClientOptions::parse(uri).await.unwrap();
    options.socks5_proxy = endpoint.map(proxy_of);
    options
}

/// A fresh directory for the advertised socket, removed on drop. The driver lower-cases every
/// member name it is sent, so the path is built lower case rather than from a random tempdir.
struct SocketDir(PathBuf);

impl SocketDir {
    fn new() -> SocketDir {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("dbento-p4-{}-{n}", std::process::id()));
        let text = dir.to_str().unwrap();
        assert_eq!(
            text,
            text.to_lowercase(),
            "the socket path must be lower case"
        );
        std::fs::create_dir(&dir).unwrap();
        SocketDir(dir)
    }
}

impl Drop for SocketDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A Unix listener at `path` that counts the connections it accepts and drops each.
fn counting_socket(path: &Path) -> Arc<AtomicUsize> {
    let listener = UnixListener::bind(path).unwrap();
    let accepts = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&accepts);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    accepts
}

/// How many times the advertised socket was dialled once a client, proxied or not, had pinged
/// the set and its monitors had time to reach every member.
async fn socket_dials(proxied: bool) -> usize {
    let dir = SocketDir::new();
    let path = dir.0.join("dbento-p4.sock");
    let accepts = counting_socket(&path);
    let port = fake_primary(&path, true).await;
    let endpoint = if proxied {
        Some(SocksEndpoint::start(Dialer::Direct).await.unwrap())
    } else {
        None
    };
    let client = Client::with_options(set_options(port, endpoint.as_ref()).await).unwrap();
    // The primary answers either way; what matters is what the monitors dial next.
    let ping = client
        .database("admin")
        .run_command(bson::doc! { "ping": 1 })
        .await;
    assert!(ping.is_ok(), "the fake primary answers a ping: {ping:?}");
    tokio::time::sleep(SETTLE).await;
    let dials = accepts.load(Ordering::SeqCst);
    client.shutdown().immediate(true).await;
    drop(endpoint);
    dials
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_advertised_socket_member_is_never_dialled_through_the_proxy() {
    assert_eq!(
        socket_dials(true).await,
        0,
        "a proxied client must not dial a local socket a member advertised"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_proxy_the_advertised_socket_member_is_not_dialled() {
    assert_eq!(
        socket_dials(false).await,
        0,
        "a direct client must not dial a local socket a member advertised and the uri does not name"
    );
}

// A primary that lists only the socket drops out of the set, so selection waits on the refused
// socket member alone and its timeout carries the driver's refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_set_left_with_only_a_socket_member_says_it_cannot_cross_ssh() {
    let dir = SocketDir::new();
    let path = dir.0.join("dbento-p4.sock");
    let accepts = counting_socket(&path);
    let port = fake_primary(&path, false).await;
    let ep = SocksEndpoint::start(Dialer::Direct).await.unwrap();
    let client = Client::with_options(set_options(port, Some(&ep)).await).unwrap();
    let e = client
        .database("admin")
        .run_command(bson::doc! { "ping": 1 })
        .await
        .unwrap_err();
    assert_eq!(accepts.load(Ordering::SeqCst), 0, "the socket was dialled");
    let mapped = connect_error(&mongo_conn(), ep.last_refusal(), true, e);
    assert!(
        matches!(&mapped, ConnectError::SshTunnel { message }
            if message.contains("advertised a local socket")
                && message.contains("cannot be reached over SSH")
                && !message.contains("dbento-p4")),
        "{mapped:?}"
    );
}

// Without a proxy the same set fails as an Open error that says why, with no path in it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_set_left_with_only_an_advertised_socket_says_it_was_not_dialled() {
    let dir = SocketDir::new();
    let path = dir.0.join("dbento-p4.sock");
    let accepts = counting_socket(&path);
    let port = fake_primary(&path, false).await;
    let client = Client::with_options(set_options(port, None).await).unwrap();
    let e = client
        .database("admin")
        .run_command(bson::doc! { "ping": 1 })
        .await
        .unwrap_err();
    assert_eq!(accepts.load(Ordering::SeqCst), 0, "the socket was dialled");
    let mapped = connect_error(&mongo_conn(), None, false, e);
    assert!(
        matches!(&mapped, ConnectError::Open { message }
            if message.contains("advertised a local socket")
                && message.contains("does not name")
                && !message.contains("dbento-p4")),
        "{mapped:?}"
    );
}

/// A server on a Unix listener at `path` that answers each connection's handshake with `hello`,
/// counting the connections it accepts.
fn socket_server(path: &Path, hello: RawDocumentBuf) -> Arc<AtomicUsize> {
    let listener = UnixListener::bind(path).unwrap();
    let accepts = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&accepts);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(serve_hello(stream, hello.clone()));
        }
    });
    accepts
}

/// A uri whose only host is the socket at `path`, percent-encoded as the driver requires,
/// followed by `query`.
fn socket_uri(path: &Path, query: &str) -> String {
    let host = path.to_str().unwrap().replace('/', "%2F");
    format!("mongodb://{host}/?serverSelectionTimeoutMS=2000{query}")
}

/// Pings, without a proxy, a server answering `hello(path)` on a socket the uri names, and
/// returns the ping with how many connections the socket accepted.
async fn ping_named_socket(
    hello: fn(&str) -> RawDocumentBuf,
    query: &str,
) -> (mongodb::error::Result<bson::Document>, usize) {
    let dir = SocketDir::new();
    let path = dir.0.join("dbento-p5.sock");
    let accepts = socket_server(&path, hello(path.to_str().unwrap()));
    let options = ClientOptions::parse(socket_uri(&path, query))
        .await
        .unwrap();
    let client = Client::with_options(options).unwrap();
    let ping = client
        .database("admin")
        .run_command(bson::doc! { "ping": 1 })
        .await;
    client.shutdown().immediate(true).await;
    (ping, accepts.load(Ordering::SeqCst))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_standalone_on_a_socket_the_uri_names_answers() {
    for query in ["", "&directConnection=true"] {
        let (ping, dials) = ping_named_socket(|_| standalone_hello(), query).await;
        assert!(
            ping.is_ok(),
            "a socket seed must stay usable ({query:?}): {ping:?}"
        );
        assert!(dials > 0, "the named socket was never dialled ({query:?})");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_set_seeded_by_a_socket_it_lists_answers() {
    let (ping, dials) = ping_named_socket(set_of_itself, "&replicaSet=rs0").await;
    assert!(
        ping.is_ok(),
        "a set whose primary lists the seed socket must stay usable: {ping:?}"
    );
    assert!(dials > 0, "the seed socket was never dialled");
}

// A socket the uri names is refused as well once a proxy is set: the driver rejects the options
// before any dial, and the error names no path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_named_socket_is_never_dialled_through_the_proxy() {
    let dir = SocketDir::new();
    let path = dir.0.join("dbento-p5.sock");
    let accepts = counting_socket(&path);
    let ep = SocksEndpoint::start(Dialer::Direct).await.unwrap();
    let mut options = ClientOptions::parse(socket_uri(&path, "")).await.unwrap();
    options.socks5_proxy = Some(proxy_of(&ep));
    let e = Client::with_options(options).unwrap_err();
    assert_eq!(
        accepts.load(Ordering::SeqCst),
        0,
        "the named socket was dialled through the proxy"
    );
    let mapped = connect_error(&mongo_conn(), ep.last_refusal(), true, e);
    assert!(
        matches!(&mapped, ConnectError::Open { message }
            if message.contains("non-TCP address") && !message.contains("dbento-p5")),
        "{mapped:?}"
    );
}
