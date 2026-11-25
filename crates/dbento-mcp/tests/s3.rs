//! S3 through the MCP tools: buckets as schemas, the table tools refused, error hints, `cat`
//! within the byte budget, hostile keys escaped in the text block, and the row, byte and time
//! bounds. A loopback stub stands in where moto cannot deny or stall.
#![cfg(feature = "s3")]
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::common::{TestServer, text, unfence};
use dbento_testkit::{S3_ACCESS_KEY, S3_SECRET_VAR};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn q(text: &str) -> Value {
    json!({"connection": "ob", "sql": text})
}

/// `[connections.ob]` on `endpoint`, signed with the harness keys, read-only, then `extra`.
fn stub_toml(endpoint: &str, extra: &str) -> String {
    format!(
        "dbento = 1\n[connections.ob]\nengine = \"s3\"\nregion = \"us-east-1\"\n\
         endpoint = \"{endpoint}\"\naccess_key_id = \"{S3_ACCESS_KEY}\"\n\
         secret = {{ env = \"{S3_SECRET_VAR}\" }}\nread_only = true\n{extra}\n"
    )
}

/// A loopback endpoint and how many requests it has read.
struct Stub {
    endpoint: String,
    seen: Arc<AtomicUsize>,
}

const DENIED: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
    <Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>";

/// An HTTP server on loopback that answers its first `answered` requests with S3's
/// `AccessDenied` and holds every later one open without a reply.
async fn stub(answered: usize) -> Stub {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            tokio::spawn(serve(socket, Arc::clone(&counter), answered));
        }
    });
    Stub { endpoint, seen }
}

/// Reads requests off one connection, each ending at its blank header line: every S3 call
/// Dbento sends is a GET or HEAD without a body.
async fn serve(mut socket: TcpStream, seen: Arc<AtomicUsize>, answered: usize) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        while let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            buf.drain(..end + 4);
            if seen.fetch_add(1, Ordering::SeqCst) >= answered {
                std::future::pending::<()>().await;
            }
            let reply = format!(
                "HTTP/1.1 403 Forbidden\r\ncontent-type: application/xml\r\n\
                 content-length: {}\r\n\r\n{DENIED}",
                DENIED.len()
            );
            if socket.write_all(reply.as_bytes()).await.is_err() {
                return;
            }
        }
        match socket.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

#[tokio::test]
async fn the_table_tools_point_at_ls_without_connecting() {
    let stub = stub(0).await;
    let server = TestServer::s3_toml(&stub_toml(&stub.endpoint, ""), |o| o).await;
    for (tool, args) in [
        ("list_tables", json!({"connection": "ob", "schema": "logs"})),
        (
            "describe_table",
            json!({"connection": "ob", "schema": "logs", "table": "k"}),
        ),
    ] {
        let e = server.err(tool, args).await;
        assert_eq!(e["code"], "unsupported", "{tool}: {e}");
        let m = e["message"].as_str().unwrap();
        assert!(
            m.contains("objects") && m.contains("ls 's3://"),
            "{tool}: {m}"
        );
    }
    assert_eq!(
        stub.seen.load(Ordering::SeqCst),
        0,
        "no request reached the endpoint"
    );
}

#[tokio::test]
async fn a_denied_bucket_listing_says_to_set_buckets_in_its_hint() {
    let stub = stub(usize::MAX).await;
    let server = TestServer::s3_toml(&stub_toml(&stub.endpoint, ""), |o| o).await;
    let e = server
        .err("list_schemas", json!({"connection": "ob"}))
        .await;
    assert_eq!(
        (&e["code"], &e["sqlstate"]),
        (&json!("database"), &json!("AccessDenied")),
        "{e}"
    );
    assert!(
        e["hint"]
            .as_str()
            .unwrap_or_default()
            .contains("buckets = ["),
        "{e}"
    );
    let e = server.err("query", q("ls")).await;
    assert!(
        e["hint"]
            .as_str()
            .unwrap_or_default()
            .contains("buckets = ["),
        "query carries the hint too: {e}"
    );
}

#[tokio::test]
async fn buckets_are_the_schemas() {
    let s3 = dbento_testkit::s3_moto().await;
    let server = TestServer::s3(&s3, "read_only = true").await;
    let schemas = server.ok("list_schemas", json!({"connection": "ob"})).await;
    assert_eq!(
        schemas["schemas"],
        json!([{"name": s3.bucket()}]),
        "the configured bucket"
    );
}

#[tokio::test]
async fn hostile_keys_reach_the_text_block_escaped_and_round_trip_into_cat() {
    let s3 = dbento_testkit::s3_moto().await;
    let server = TestServer::s3(&s3, "read_only = true").await;
    let result = server
        .call(
            "query",
            q(&format!("ls -r 's3://{}/hostile/'", s3.bucket())),
        )
        .await;
    let structured = result.structured_content.clone().expect("structured");
    let block = text(&result);
    for raw in ['\u{202e}', '\u{85}', '\u{1b}'] {
        assert!(!block.contains(raw), "{raw:?} reached the text block raw");
    }
    assert!(
        block.contains("\\u202e") && block.contains("\\u0085"),
        "{block}"
    );
    let (_, parsed) = unfence(&block);
    assert_eq!(parsed, structured, "the escapes decode to the exact keys");
    let bidi = "hostile/bidi\u{202e}txt.exe";
    let keys: Vec<&str> = structured["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r[1].as_str().unwrap())
        .collect();
    assert!(keys.contains(&bidi), "{keys:?}");
    let quoted = dbento_core::quote_literal(
        dbento_core::Engine::S3,
        &format!("s3://{}/{bidi}", s3.bucket()),
    );
    let r = server.ok("query", q(&format!("cat {quoted}"))).await;
    assert_eq!(r["rows"][0][0], "x", "the exact key reads its object");
}

/// `bytes` as lowercase hex.
fn hex(bytes: impl IntoIterator<Item = u8>) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[tokio::test]
async fn cat_shows_text_up_to_the_byte_budget_and_binary_as_cut_hex() {
    let s3 = dbento_testkit::s3_moto().await;
    s3.put(
        "text/long.txt",
        "a".repeat(100 << 10).into_bytes(),
        Some("text/plain"),
    )
    .await;
    let server = TestServer::s3(&s3, "read_only = true").await;
    let at = |key: &str| q(&format!("cat 's3://{}/{key}'", s3.bucket()));

    let long = server.ok("query", at("text/long.txt")).await;
    assert_eq!(
        long["columns"][0],
        json!({"name": "content", "type": "text"}),
        "{}",
        long["columns"]
    );
    let content = long["rows"][0][0]
        .as_str()
        .expect("whole text, not a cut object");
    assert_eq!(
        content.len(),
        (64 << 10) - 64,
        "the read under the byte budget"
    );
    assert_eq!(long["rows"][0][1], "102400", "size is the whole object");
    assert_eq!(
        long["rows"][0][2], "true",
        "the object is longer than the read"
    );
    assert_eq!(
        (&long["truncated"], &long["truncated_by"]),
        (&json!(false), &json!(null)),
        "one row, not cut"
    );

    let blob = server.ok("query", at("bin/blob.bin")).await;
    assert_eq!(blob["columns"][0]["type"], "bytes", "{blob}");
    assert_eq!(
        blob["rows"][0],
        json!([{"hex": hex(0..=255u8)}, "256", "false"]),
        "a small binary object whole"
    );

    let big = server.ok("query", at("big/three-mib.bin")).await;
    let cell = &big["rows"][0][0];
    assert_eq!(cell["cut"], true, "{cell}");
    assert_eq!(
        cell["hex"],
        json!(hex((0..2048).map(dbento_testkit::big_byte))),
        "the first 2 KiB as hex"
    );
    assert_eq!(big["rows"][0][1], "3145728", "{big}");
    assert_eq!(big["rows"][0][2], "true", "{big}");
}

#[tokio::test]
async fn a_sqlite_result_named_like_cat_is_still_cut() {
    let server = TestServer::sqlite("", "").await;
    let r = server
        .ok(
            "query",
            json!({"connection": "lite", "sql": "SELECT replace(hex(zeroblob(5000)), '0', 'a') AS content, 1 AS size, 0 AS truncated"}),
        )
        .await;
    assert_eq!(r["rows"][0][0]["cut"], true, "{}", r["rows"][0][0]);
    assert_eq!(
        r["rows"][0][0]["text"].as_str().map(str::len),
        Some(4 << 10),
        "the generic 4 KiB cut"
    );
}
