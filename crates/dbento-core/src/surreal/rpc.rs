//! One WebSocket per session, owned by a task, so a cancel or a dropped session acts whether or
//! not the caller is polling. Replies and stream frames are matched to their caller by id.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use tokio::net::TcpStream;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::error::{CapacityError, Error as WsError};
use tokio_tungstenite::tungstenite::http::header;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream, client_async_tls_with_config};

use super::decode::{Wire, decode, envelope};
use super::wire::{WireError, field};
use crate::postgres::{TLS_WAYS_OUT, rustls_config, tls_error};
use crate::secrets::Secret;
use crate::session::ConnectError;
use crate::workspace::{ConnectionConfig, TlsMode};

/// The largest reply the socket accepts; one frame holds up to 256 rows, so wide rows need room.
pub(crate) const MAX_REPLY: usize = 256 << 20;
/// Bytes of undelivered frames at which the task stops reading the socket.
pub(crate) const MAX_BUFFERED: usize = 64 << 20;

/// How long any one call may wait for its reply. Past it the socket closes, which rolls back any
/// fence, because a server that stopped answering would otherwise hang the session.
pub(crate) const CALL_BOUND: Duration = Duration::from_secs(30);

const LOST: &str = "the connection to the server was lost";

/// A CBOR parameter: text, objects, and tags 6 and 37 only, which v3 and v4 both accept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Param<'a> {
    Text(&'a str),
    None,
    Uuid([u8; 16]),
    Int(i64),
    Object(Vec<(&'a str, Param<'a>)>),
}

/// One request as a definite CBOR map with an integer id.
pub(crate) fn request(
    id: i64,
    method: &str,
    params: &[Param<'_>],
    txn: Option<[u8; 16]>,
) -> Vec<u8> {
    let mut w = Vec::with_capacity(64);
    head(&mut w, MAP, 3 + u64::from(txn.is_some()));
    write_param(&Param::Text("id"), &mut w);
    write_param(&Param::Int(id), &mut w);
    write_param(&Param::Text("method"), &mut w);
    write_param(&Param::Text(method), &mut w);
    write_param(&Param::Text("params"), &mut w);
    head(&mut w, ARRAY, params.len() as u64);
    for param in params {
        write_param(param, &mut w);
    }
    if let Some(txn) = txn {
        write_param(&Param::Text("txn"), &mut w);
        write_param(&Param::Uuid(txn), &mut w);
    }
    w
}

const UNSIGNED: u8 = 0;
const NEGATIVE: u8 = 1;
const BYTES: u8 = 2;
const TEXT: u8 = 3;
const ARRAY: u8 = 4;
const MAP: u8 = 5;
const TAG: u8 = 6;
const NULL: u8 = 0xf6;

// A CBOR head in its shortest form: the argument in the low five bits, or in 1, 2, 4 or 8 bytes.
fn head(w: &mut Vec<u8>, major: u8, n: u64) {
    let major = major << 5;
    match n {
        0..=23 => w.push(major | n as u8),
        24..=0xff => w.extend([major | 24, n as u8]),
        0x100..=0xffff => {
            w.push(major | 25);
            w.extend((n as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            w.push(major | 26);
            w.extend((n as u32).to_be_bytes());
        }
        _ => {
            w.push(major | 27);
            w.extend(n.to_be_bytes());
        }
    }
}

fn write_param(param: &Param<'_>, w: &mut Vec<u8>) {
    match param {
        Param::Text(text) => {
            head(w, TEXT, text.len() as u64);
            w.extend(text.as_bytes());
        }
        Param::None => {
            head(w, TAG, 6);
            w.push(NULL);
        }
        Param::Uuid(bytes) => {
            head(w, TAG, 37);
            head(w, BYTES, bytes.len() as u64);
            w.extend(bytes);
        }
        Param::Int(n) if *n >= 0 => head(w, UNSIGNED, n.unsigned_abs()),
        Param::Int(n) => head(w, NEGATIVE, n.unsigned_abs() - 1),
        Param::Object(entries) => {
            head(w, MAP, entries.len() as u64);
            for (key, value) in entries {
                write_param(&Param::Text(key), w);
                write_param(value, w);
            }
        }
    }
}

/// A failed call: the server's error, or the session's end and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RpcError {
    Server(WireError),
    /// The reply came, but Dbento cannot decode it; the socket is fine.
    Unreadable(String),
    Closed(String),
}

/// One frame of a `query_stream`.
#[derive(Debug)]
pub(crate) enum Frame {
    Begin {
        statements: u64,
    },
    Rows {
        index: u64,
        values: Vec<Wire>,
        bytes: usize,
    },
    Value {
        index: u64,
        value: Wire,
    },
    Finished {
        index: u64,
        single: bool,
        error: Option<WireError>,
    },
    End {
        error: Option<WireError>,
    },
    /// A call-level error: no frames follow.
    Refused(WireError),
    /// A frame with this stream's id that could not be decoded.
    Unreadable {
        message: String,
    },
    Closed(String),
}

// Bytes handed to a stream and not yet taken, shared by the task and the reader.
#[derive(Default)]
struct Gauge {
    bytes: AtomicUsize,
    taken: Notify,
}

impl Gauge {
    fn over(&self) -> bool {
        self.bytes.load(Ordering::Acquire) >= MAX_BUFFERED
    }

    fn add(&self, bytes: usize) {
        self.bytes.fetch_add(bytes, Ordering::AcqRel);
    }

    fn take(&self, bytes: usize) {
        self.bytes.fetch_sub(bytes, Ordering::AcqRel);
        self.taken.notify_waiters();
    }

    // Resolves once the buffer is under the bound. The wait is registered before the check, so a
    // take landing between the two still wakes it.
    async fn room(&self) {
        loop {
            let mut taken = std::pin::pin!(self.taken.notified());
            taken.as_mut().enable();
            if !self.over() {
                return;
            }
            taken.await;
        }
    }
}

/// The frames of one stream.
pub(crate) struct Frames {
    rx: mpsc::UnboundedReceiver<(Frame, usize)>,
    gauge: Arc<Gauge>,
    closed: Arc<Mutex<Option<String>>>,
}

impl Frames {
    /// The next frame; `Closed` once the socket is gone.
    pub(crate) async fn next(&mut self) -> Frame {
        match self.rx.recv().await {
            Some((frame, bytes)) => {
                self.gauge.take(bytes);
                frame
            }
            None => Frame::Closed(reason(&self.closed)),
        }
    }

    /// Gives up the stream: what is unread is released, and the task throws away what follows,
    /// so a stream nobody reads can never hold up the replies behind it.
    pub(crate) fn discard(&mut self) {
        self.rx.close();
        while let Ok((_, bytes)) = self.rx.try_recv() {
            self.gauge.take(bytes);
        }
    }
}

impl Drop for Frames {
    fn drop(&mut self) {
        self.discard();
    }
}

type Reply = oneshot::Sender<Result<Wire, RpcError>>;
type FrameTx = mpsc::UnboundedSender<(Frame, usize)>;

enum Route {
    Call(Reply),
    Stream(FrameTx),
}

enum Command {
    Call {
        id: i64,
        bytes: Vec<u8>,
        reply: Reply,
    },
    Stream {
        id: i64,
        bytes: Vec<u8>,
        frames: FrameTx,
    },
    Send(Vec<u8>),
    Close(String),
}

/// The session's handle to its socket task; cloning shares the socket.
#[derive(Clone)]
pub(crate) struct Rpc {
    commands: mpsc::UnboundedSender<Command>,
    next_id: Arc<AtomicI64>,
    gauge: Arc<Gauge>,
    closed: Arc<Mutex<Option<String>>>,
    call_bound: Duration,
}

fn reason(closed: &Mutex<Option<String>>) -> String {
    let closed = closed.lock().unwrap_or_else(|e| e.into_inner());
    closed.clone().unwrap_or_else(|| LOST.to_owned())
}

impl Rpc {
    /// Connects, offering only `cbor`, and starts the socket task. No ALPN, so the server stays
    /// on HTTP/1.1. A 101 without `cbor` fails here, as that socket would speak JSON.
    pub(crate) async fn open(
        conn: &ConnectionConfig,
        passphrase: Option<&Secret>,
        call_bound: Duration,
    ) -> Result<Rpc, ConnectError> {
        let host = conn.host().unwrap_or_default();
        let port = conn.port().unwrap_or(8000);
        let tls = !matches!(conn.tls(), Some(TlsMode::Off));
        let scheme = if tls { "wss" } else { "ws" };
        let mut upgrade = format!("{scheme}://{host}:{port}/rpc")
            .into_client_request()
            .map_err(|e| open_error(conn, format!("bad server address: {e}")))?;
        upgrade.headers_mut().insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            header::HeaderValue::from_static("cbor"),
        );
        let tcp = TcpStream::connect((host, port))
            .await
            .map_err(|e| open_error(conn, e.to_string()))?;
        let _ = tcp.set_nodelay(true);
        // Always a connector: `None` would build one with no roots at all.
        let connector = if tls {
            Connector::Rustls(Arc::new(rustls_config(conn, passphrase)?))
        } else {
            Connector::Plain
        };
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_REPLY))
            .max_frame_size(Some(MAX_REPLY));
        let (socket, _) = client_async_tls_with_config(upgrade, tcp, Some(config), Some(connector))
            .await
            .map_err(|e| handshake_error(conn, tls, e))?;
        Ok(Rpc::start(socket, call_bound))
    }

    fn start(socket: WebSocketStream<MaybeTlsStream<TcpStream>>, call_bound: Duration) -> Rpc {
        let (commands, rx) = mpsc::unbounded_channel();
        let gauge = Arc::new(Gauge::default());
        let closed = Arc::new(Mutex::new(None));
        tokio::spawn(run(socket, rx, Arc::clone(&gauge), Arc::clone(&closed)));
        Rpc {
            commands,
            next_id: Arc::new(AtomicI64::new(1)),
            gauge,
            closed,
            call_bound,
        }
    }

    /// How long a call may wait, which also bounds a stream's `begin` frame.
    pub(crate) fn call_bound(&self) -> Duration {
        self.call_bound
    }

    /// A fresh request id.
    pub(crate) fn next_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Why the socket ended, once it has.
    pub(crate) fn ended(&self) -> Option<String> {
        self.closed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// One call and its reply.
    pub(crate) async fn call(
        &self,
        method: &str,
        params: &[Param<'_>],
        txn: Option<[u8; 16]>,
    ) -> Result<Wire, RpcError> {
        let id = self.next_id();
        let (reply, wait) = oneshot::channel();
        let bytes = request(id, method, params, txn);
        if self
            .commands
            .send(Command::Call { id, bytes, reply })
            .is_err()
        {
            return Err(RpcError::Closed(reason(&self.closed)));
        }
        match tokio::time::timeout(self.call_bound, wait).await {
            Ok(answer) => answer.unwrap_or_else(|_| Err(RpcError::Closed(reason(&self.closed)))),
            Err(_) => Err(RpcError::Closed(self.unanswered(method))),
        }
    }

    /// Ends the socket for a server that went quiet, and says so.
    pub(crate) fn unanswered(&self, what: &str) -> String {
        let why = format!(
            "the server did not answer {what} within {} s",
            self.call_bound.as_secs()
        );
        self.close(why.clone());
        why
    }

    /// Sends `text` as `query_stream` under id `id`.
    pub(crate) fn stream(&self, id: i64, text: &str, txn: Option<[u8; 16]>) -> Frames {
        let (frames, rx) = mpsc::unbounded_channel();
        let bytes = request(id, "query_stream", &[Param::Text(text)], txn);
        let _ = self.commands.send(Command::Stream { id, bytes, frames });
        Frames {
            rx,
            gauge: Arc::clone(&self.gauge),
            closed: Arc::clone(&self.closed),
        }
    }

    /// Asks the server to stop stream `id`, ignoring the reply: a stream already over refuses.
    pub(crate) fn query_cancel(&self, id: i64) {
        let bytes = request(self.next_id(), "query_cancel", &[Param::Int(id)], None);
        let _ = self.commands.send(Command::Send(bytes));
    }

    /// Ends the socket; every open stream and call then reports `reason`.
    pub(crate) fn close(&self, reason: String) {
        let _ = self.commands.send(Command::Close(reason));
    }
}

fn open_error(conn: &ConnectionConfig, message: String) -> ConnectError {
    ConnectError::Open {
        message: format!("{}: {message}", conn.display_target()),
    }
}

// The wrong pairing of `ws` and `wss` fails in two ways, and neither says "wrong scheme", so
// each error names the setting that would fix it.
fn handshake_error(conn: &ConnectionConfig, tls: bool, e: WsError) -> ConnectError {
    use tokio_tungstenite::tungstenite::error::{ProtocolError, SubProtocolError};
    match e {
        WsError::Protocol(ProtocolError::SecWebSocketSubProtocolError(
            SubProtocolError::NoSubProtocol,
        )) => open_error(
            conn,
            "the server did not accept the cbor subprotocol".to_owned(),
        ),
        WsError::Tls(e) => tls_error(conn, format!("{e}; {TLS_WAYS_OUT}")),
        // rustls reports a refused certificate as `InvalidData` through its stream.
        WsError::Io(e) if tls && e.kind() == std::io::ErrorKind::InvalidData => {
            tls_error(conn, format!("{e}; {TLS_WAYS_OUT}"))
        }
        WsError::Io(e) if tls => tls_error(
            conn,
            format!(
                "{e}; the server may not serve TLS on this port: set tls = \"off\" for ws, or \
                 check the port"
            ),
        ),
        other if !tls => open_error(
            conn,
            format!(
                "{other}; the server may require TLS: set tls = \"verify\" for wss, or check \
                 the port"
            ),
        ),
        other => open_error(conn, other.to_string()),
    }
}

// The task: a `select` over commands and the socket, which it stops polling past the buffer
// bound. Commands still run then, so a stop can always reach the server.
async fn run(
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    gauge: Arc<Gauge>,
    closed: Arc<Mutex<Option<String>>>,
) {
    let (mut sink, mut stream) = socket.split();
    let mut routes: HashMap<i64, Route> = HashMap::new();
    let why = loop {
        // One reading of the gauge enables exactly one of the socket and the wait for room.
        let over = gauge.over();
        tokio::select! {
            command = commands.recv() => {
                let bytes = match command {
                    None => break "the session was dropped".to_owned(),
                    Some(Command::Close(why)) => break why,
                    Some(Command::Call { id, bytes, reply }) => {
                        routes.insert(id, Route::Call(reply));
                        bytes
                    }
                    Some(Command::Stream { id, bytes, frames }) => {
                        routes.insert(id, Route::Stream(frames));
                        bytes
                    }
                    Some(Command::Send(bytes)) => bytes,
                };
                if sink.send(Message::binary(bytes)).await.is_err() {
                    break LOST.to_owned();
                }
            }
            message = stream.next(), if !over => match message {
                Some(Ok(Message::Binary(frame))) => {
                    if let Err(why) = route(&frame, &mut routes, &gauge) {
                        break why;
                    }
                }
                Some(Ok(Message::Text(_))) => {
                    break "the server sent a text frame, which cbor never does".to_owned();
                }
                Some(Ok(Message::Close(_))) | None => break LOST.to_owned(),
                Some(Ok(_)) => {}
                Some(Err(WsError::Capacity(CapacityError::MessageTooLong { size, .. }))) => {
                    break format!(
                        "a reply of {size} bytes is over the {} MiB limit",
                        MAX_REPLY >> 20
                    );
                }
                Some(Err(_)) => break LOST.to_owned(),
            },
            () = gauge.room(), if over => {}
        }
    };
    *closed.lock().unwrap_or_else(|e| e.into_inner()) = Some(why);
}

// Sends one frame where its id says. `Err` ends the session: a frame no id can be read from
// may belong to anyone, so nothing after it can be trusted.
fn route(frame: &[u8], routes: &mut HashMap<i64, Route>, gauge: &Gauge) -> Result<(), String> {
    let reply = match decode(frame) {
        Ok(reply) => reply,
        Err(failure) => {
            let Some(id) = envelope(frame) else {
                return Err(format!(
                    "the server sent a reply Dbento cannot read: {failure}"
                ));
            };
            unreadable(id, failure.message, routes, gauge, frame.len());
            return Ok(());
        }
    };
    let Some(Wire::Int(id)) = field(&reply, "id") else {
        return match field(&reply, "error") {
            Some(error) => Err(format!(
                "the server could not read a request: {}",
                WireError::of(error).message
            )),
            None => Ok(()), // a live notification
        };
    };
    let id = i64::try_from(*id).unwrap_or_default();
    match routes.get(&id) {
        None => {}
        Some(Route::Call(_)) => {
            if let Some(Route::Call(answer)) = routes.remove(&id) {
                let _ = answer.send(match field(&reply, "error") {
                    Some(error) => Err(RpcError::Server(WireError::of(error))),
                    None => Ok(take(reply, "result")),
                });
            }
        }
        Some(Route::Stream(frames)) => {
            let parsed = stream_frame(reply, frame.len());
            let done = matches!(parsed, Frame::End { .. } | Frame::Refused(_));
            deliver(frames, parsed, frame.len(), gauge);
            if done {
                routes.remove(&id);
            }
        }
    }
    Ok(())
}

// A send fails once the reader has gone, and that frame counts for nothing.
fn deliver(frames: &FrameTx, frame: Frame, bytes: usize, gauge: &Gauge) {
    gauge.add(bytes);
    if frames.send((frame, bytes)).is_err() {
        gauge.take(bytes);
    }
}

fn unreadable(
    id: i64,
    message: String,
    routes: &mut HashMap<i64, Route>,
    gauge: &Gauge,
    bytes: usize,
) {
    match routes.get(&id) {
        Some(Route::Stream(frames)) => deliver(frames, Frame::Unreadable { message }, bytes, gauge),
        Some(Route::Call(_)) => {
            if let Some(Route::Call(answer)) = routes.remove(&id) {
                let _ = answer.send(Err(RpcError::Unreadable(message)));
            }
        }
        None => {}
    }
}

fn take(object: Wire, name: &str) -> Wire {
    let Wire::Object(entries) = object else {
        return Wire::None;
    };
    entries
        .into_iter()
        .find(|(key, _)| key == name)
        .map_or(Wire::None, |(_, value)| value)
}

fn number(result: &Wire, name: &str) -> u64 {
    match field(result, name) {
        Some(Wire::Int(n)) => u64::try_from(*n).unwrap_or_default(),
        _ => 0,
    }
}

fn stream_frame(reply: Wire, bytes: usize) -> Frame {
    if let Some(error) = field(&reply, "error") {
        return Frame::Refused(WireError::of(error));
    }
    let result = take(reply, "result");
    let index = number(&result, "index");
    let error = field(&result, "error").map(WireError::of);
    let kind = match field(&result, "stream") {
        Some(Wire::Text(kind)) => kind.clone(),
        _ => String::new(),
    };
    match kind.as_str() {
        "begin" => Frame::Begin {
            statements: number(&result, "statements"),
        },
        "rows" => Frame::Rows {
            index,
            values: match take(result, "values") {
                Wire::Array(values) => values,
                _ => Vec::new(),
            },
            bytes,
        },
        "value" => Frame::Value {
            index,
            value: take(result, "value"),
        },
        "finished" => Frame::Finished {
            index,
            single: matches!(field(&result, "single"), Some(Wire::Bool(true))),
            error,
        },
        "end" => Frame::End { error },
        other => Frame::Unreadable {
            message: format!("an unknown stream frame {other:?}"),
        },
    }
}

#[cfg(test)]
mod tests;
