//! A hostile server: replies nested past the driver's wire bound are refused, not decoded.
#![cfg(feature = "mongodb")]

use crate::support::{code_of, failure, read};
use bson::{RawDocument, RawDocumentBuf, Timestamp, rawdoc};
use dbento_core::{Session, SessionOptions, Workspace, connect};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// A document `depth` levels deep, counting itself, built in one pass: every wrapper's length,
/// type byte and empty key, the innermost `{}`, then each wrapper's closing NUL.
fn nested(depth: usize) -> RawDocumentBuf {
    let mut bytes = Vec::with_capacity(5 + 7 * depth);
    for level in 1..depth {
        let len = 5 + 7 * (depth - level);
        bytes.extend_from_slice(&(len as i32).to_le_bytes());
        bytes.extend_from_slice(&[3, 0]);
    }
    bytes.extend_from_slice(&[5, 0, 0, 0, 0]);
    bytes.resize(bytes.len() + depth - 1, 0);
    RawDocumentBuf::from_bytes(bytes).unwrap()
}

/// A command reply `depth` levels deep: its root, `$clusterTime`, then the signature's levels.
fn deep_cluster_time(depth: usize) -> RawDocumentBuf {
    rawdoc! {
        "ok": 1.0,
        "$clusterTime": {
            "clusterTime": Timestamp { time: 1, increment: 1 },
            "signature": nested(depth - 2),
        },
    }
}

// Just enough of a standalone server for the driver's handshake and Dbento's connect.
fn answer(command: &str, hostile: &RawDocumentBuf) -> RawDocumentBuf {
    match command {
        "hello" | "isMaster" | "ismaster" => rawdoc! {
            "helloOk": true,
            "isWritablePrimary": true,
            "ismaster": true,
            "maxBsonObjectSize": 16_777_216,
            "maxMessageSizeBytes": 48_000_000,
            "maxWriteBatchSize": 100_000,
            "minWireVersion": 0,
            "maxWireVersion": 21,
            "ok": 1.0,
        },
        "buildInfo" => rawdoc! { "version": "8.0.0", "ok": 1.0 },
        "hostile" | "find" => hostile.clone(),
        _ => rawdoc! { "ok": 1.0 },
    }
}

// Reads OP_MSG requests and answers each by its command name, until the client hangs up.
async fn serve(mut socket: TcpStream, hostile: RawDocumentBuf) {
    let mut header = [0; 16];
    while socket.read_exact(&mut header).await.is_ok() {
        let len = i32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
        let mut body = vec![0; len - 16];
        socket.read_exact(&mut body).await.unwrap();
        // Flags, then the payload-0 section's kind byte, then the command document.
        let command = RawDocument::from_bytes(&body[5..]).unwrap();
        let name = command
            .iter_elements()
            .next()
            .unwrap()
            .unwrap()
            .key()
            .to_string();
        let reply = answer(&name, &hostile);
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

/// A session on a fake server whose `hostile` and `find` replies are `hostile`.
async fn session_on(hostile: RawDocumentBuf) -> (Session, tempfile::TempDir) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            tokio::spawn(serve(socket, hostile.clone()));
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let uri = format!("mongodb://127.0.0.1:{port}/x?directConnection=true");
    let toml = format!("dbento = 1\n[connections.c]\nengine = \"mongodb\"\nuri = \"{uri}\"\n");
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    let conn = ws.declared("c").unwrap();
    let session = connect(&ws, conn, None, SessionOptions::default())
        .await
        .unwrap();
    (session, dir)
}

#[tokio::test]
async fn a_reply_nested_1000_levels_deep_is_a_driver_error() {
    let (mut s, _dir) = session_on(deep_cluster_time(1000)).await;
    let e = failure(&mut s, "db.runCommand({ hostile: 1 })").await;
    assert_eq!(code_of(e), "driver");
}

// The deepest reply the bound lets through is decoded on this 2 MiB test thread, one past it is not.
#[tokio::test]
async fn cluster_time_is_admitted_at_32_levels_and_refused_at_33() {
    let (mut s, _dir) = session_on(deep_cluster_time(32)).await;
    read(&mut s, "db.runCommand({ hostile: 1 })").await;
    let (mut s, _dir) = session_on(deep_cluster_time(33)).await;
    let e = failure(&mut s, "db.runCommand({ hostile: 1 })").await;
    assert_eq!(code_of(e), "driver");
}

/// A cursor reply whose one batch document is `depth` levels deep, itself counted.
fn batch_of(depth: usize) -> RawDocumentBuf {
    rawdoc! {
        "cursor": { "id": 0_i64, "ns": "x.c", "firstBatch": [nested(depth)] },
        "ok": 1.0,
    }
}

#[tokio::test]
async fn a_200_level_stored_document_still_arrives_in_a_cursor_batch() {
    let (mut s, _dir) = session_on(batch_of(200)).await;
    assert_eq!(read(&mut s, "db.c.find()").await.rows.len(), 1);
}

// Root, `cursor` and `firstBatch` come first, so a 254-level document ends at level 257.
#[tokio::test]
async fn a_batch_document_past_256_levels_from_the_root_is_refused() {
    let (mut s, _dir) = session_on(batch_of(254)).await;
    assert_eq!(code_of(failure(&mut s, "db.c.find()").await), "driver");
}

// The driver decodes these whole, so a cursor-shaped batch in them gets no deeper allowance.
#[tokio::test]
async fn error_bodies_and_plain_replies_are_bounded_at_32_levels() {
    let error = rawdoc! { "ok": 0.0, "errmsg": "no", "code": 2, "errInfo": nested(40) };
    let error_batch = rawdoc! {
        "ok": 0.0, "errmsg": "no", "code": 2,
        "cursor": { "id": 0_i64, "ns": "x.c", "firstBatch": [nested(100)] },
    };
    for reply in [error, error_batch, batch_of(100)] {
        let (mut s, _dir) = session_on(reply).await;
        let e = failure(&mut s, "db.runCommand({ hostile: 1 })").await;
        assert_eq!(code_of(e), "driver");
    }
}
