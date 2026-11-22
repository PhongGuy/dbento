//! Loopback Potluck stand-in: canned replies by method and path, event streams held open.
#![allow(dead_code, clippy::unwrap_used)]
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub enum Reply {
    Json(u16, String),
    Status(u16),
    Stream,
}

#[derive(Default)]
struct Inner {
    routes: HashMap<(String, String), Reply>,
    streams: Vec<TcpStream>,
    hits: Vec<(String, String, String)>,
    closed: bool,
}

pub struct Stub {
    uri: String,
    addr: SocketAddr,
    inner: Arc<Mutex<Inner>>,
}

impl Stub {
    pub fn start() -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let inner = Arc::new(Mutex::new(Inner::default()));
        let shared = Arc::clone(&inner);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                if shared.lock().unwrap().closed {
                    return;
                }
                let Ok(conn) = conn else { continue };
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || serve(conn, &shared));
            }
        });
        Stub {
            uri: format!("http://{addr}"),
            addr,
            inner,
        }
    }
    pub fn uri(&self) -> &str {
        &self.uri
    }
    pub fn reply(&self, verb: &str, path: &str, reply: Reply) {
        self.inner
            .lock()
            .unwrap()
            .routes
            .insert((verb.into(), path.into()), reply);
    }
    /// Writes `frame` to every open stream.
    pub fn send(&self, frame: &str) {
        self.inner
            .lock()
            .unwrap()
            .streams
            .retain_mut(|s| s.write_all(frame.as_bytes()).is_ok());
    }
    pub fn close_streams(&self) {
        for s in self.inner.lock().unwrap().streams.drain(..) {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
    /// Requests seen; a stream counts once `send` can reach it.
    pub fn hits(&self, verb: &str, path: &str) -> usize {
        self.inner
            .lock()
            .unwrap()
            .hits
            .iter()
            .filter(|(v, p, _)| v == verb && p == path)
            .count()
    }
    /// The last request head for `verb` and `path`, lowercased.
    pub fn head(&self, verb: &str, path: &str) -> Option<String> {
        let g = self.inner.lock().unwrap();
        g.hits
            .iter()
            .rev()
            .find(|(v, p, _)| v == verb && p == path)
            .map(|h| h.2.to_ascii_lowercase())
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.inner.lock().unwrap().closed = true;
        self.close_streams();
        let _ = TcpStream::connect(self.addr);
    }
}

fn serve(mut conn: TcpStream, inner: &Mutex<Inner>) {
    let (mut head, mut byte) = (Vec::new(), [0u8; 1]);
    while !head.ends_with(b"\r\n\r\n") {
        match conn.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => return,
        }
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let mut first = head.lines().next().unwrap_or("").split(' ');
    let verb = first.next().unwrap_or("").to_owned();
    let path = first
        .next()
        .unwrap_or("")
        .split('?')
        .next()
        .unwrap_or("")
        .to_owned();
    let reply = inner
        .lock()
        .unwrap()
        .routes
        .get(&(verb.clone(), path.clone()))
        .cloned();
    let (code, ctype, body) = match reply {
        Some(Reply::Stream) => {
            let _ = conn.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
            );
            let mut g = inner.lock().unwrap();
            g.streams.push(conn);
            g.hits.push((verb, path, head));
            return;
        }
        Some(Reply::Json(code, body)) => (code, "application/json", body),
        Some(Reply::Status(code)) => (code, "text/plain", String::new()),
        None => (404, "text/plain", String::new()),
    };
    inner.lock().unwrap().hits.push((verb, path, head));
    let _ = write!(
        conn,
        "HTTP/1.1 {code} X\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = conn.shutdown(Shutdown::Write);
}
