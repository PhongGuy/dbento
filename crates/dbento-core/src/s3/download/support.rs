//! A loopback HTTP/1.1 server that answers from a script, for download tests that need the
//! real hyper body path: short bodies, stalls and missing lengths.
use crate::ObjectKey;
use crate::s3::client::S3Client;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Answers each request with the next scripted (head, body, hold) entry: `hold` keeps the socket
/// open for 30 s after sending, and a `Connection: close` head closes it after sending.
pub(crate) struct Stub {
    pub(crate) port: u16,
    pub(crate) seen: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

pub(crate) async fn stub(script: Vec<(String, Vec<u8>, bool)>) -> Stub {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&seen);
    tokio::spawn(async move {
        let mut script = std::collections::VecDeque::from(script);
        while let Ok((mut sock, _)) = listener.accept().await {
            loop {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if sock.read(&mut byte).await.unwrap_or(0) == 0 {
                        break;
                    }
                    head.push(byte[0]);
                }
                if head.is_empty() {
                    break;
                }
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Some((status_and_headers, body, hold)) = script.pop_front() else {
                    return;
                };
                let _ = sock.write_all(status_and_headers.as_bytes()).await;
                let _ = sock.write_all(&body).await;
                if hold {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    return;
                }
                if status_and_headers.contains("Connection: close") {
                    break;
                }
            }
        }
    });
    Stub { port, seen }
}

pub(crate) fn head(len: usize) -> (String, Vec<u8>, bool) {
    (
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nETag: \"e\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        ),
        Vec::new(),
        false,
    )
}
pub(crate) fn get(len: usize, body: &[u8], hold: bool) -> (String, Vec<u8>, bool) {
    (
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nContent-Type: application/octet-stream\r\n\r\n"
        ),
        body.to_vec(),
        hold,
    )
}
pub(crate) fn client(port: u16) -> S3Client {
    S3Client::new(
        crate::s3::replay::settings(
            Some(&format!("http://127.0.0.1:{port}")),
            "us-east-1",
            false,
        ),
        crate::aws::https_client(),
    )
}
pub(crate) fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}
pub(crate) fn key() -> ObjectKey {
    ObjectKey::new("bkt", "dir/out.bin")
}
