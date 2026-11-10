//! The SQL Server driver: each session's tiberius client lives on a connection thread of its
//! own, with a current-thread runtime, and the async half below talks to it over channels:
//! one request channel per session, one control channel per script and one depth-1 event
//! channel per statement, as the SQLite driver does.

pub(crate) mod catalog;
mod conn;
mod types;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tiberius::error::TokenError;
use tiberius::{AttentionHandle, AuthMethod, Config, EncryptionLevel};
use tokio::sync::{Notify, mpsc, oneshot};

use crate::introspect::{ColumnMeta, IntrospectError, RelationList, RelationRef, SchemaInfo};
use crate::session::{
    CancelHandle, ConnectError, ConnectParams, Driver, ExecuteError, ExtendError, Limits,
    OnTruncate, Prepared, ReadOnlyEnforcement, ServerInfo, StreamItem,
};
use crate::value::ColumnInfo;
use crate::workspace::TlsMode;
use conn::{Ctl, Event, Request};

/// TCP, login and the version query share this bound, as MySQL's connect does.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// From a stop to the Attention acknowledgement; past it the session closes.
const ATTENTION_BOUND: Duration = Duration::from_secs(5);

/// The async handle to one session's connection thread.
pub(crate) struct MssqlDriver {
    requests: mpsc::UnboundedSender<Request>,
    server: ServerInfo,
    cancel: Arc<MssqlCancel>,
    /// The session runs as a reader principal, so the database refuses its writes.
    reader: bool,
    /// Set once a USE or session SET is sent on this session; see `Session::settings_changed`.
    settings_changed: Arc<AtomicBool>,
}

/// The async handle to one script on the connection thread.
pub(crate) struct MssqlScript {
    ctl: mpsc::UnboundedSender<Ctl>,
}

/// One statement's events, plus the resume sender a parked statement waits on.
pub(crate) struct MssqlStream {
    events: mpsc::Receiver<Event>,
    resume: Option<mpsc::UnboundedSender<Option<u64>>>,
    parked: bool,
}

/// Why Dbento stopped a send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    Cancelled,
    Timeout,
}

impl Stop {
    fn error(self) -> ExecuteError {
        match self {
            Stop::Cancelled => ExecuteError::Cancelled,
            Stop::Timeout => ExecuteError::Timeout,
        }
    }
}

/// The send a stop may aim at. Locked only briefly, never across an await.
#[derive(Default)]
struct Target {
    /// The generation handed out by `Next` until its response is read; the first is 1.
    handed: Option<u64>,
    last: u64,
    /// The first stop requested, with its generation and when it was requested.
    stop: Option<(u64, Stop, std::time::Instant)>,
    /// The handed send's statement timeout, aborted when the send ends.
    watchdog: Option<tokio::task::AbortHandle>,
}

/// Stops the session's send in flight by generation, with Attention on the session's own
/// connection. The request is made under the lock that also ends a generation, so a stop
/// that comes late is either forgotten by the next send or lands before that send begins.
pub(crate) struct MssqlCancel {
    attention: AttentionHandle,
    target: Mutex<Target>,
    /// Wakes the connection thread from an await on the caller.
    stopped: Notify,
}

impl std::fmt::Debug for MssqlCancel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MssqlCancel").finish_non_exhaustive()
    }
}

impl MssqlCancel {
    fn new(attention: AttentionHandle) -> Self {
        MssqlCancel {
            attention,
            target: Mutex::default(),
            stopped: Notify::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Target> {
        self.target.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Stops the send handed out, if any; never blocks.
    pub(crate) fn cancel(&self) {
        let handed = self.lock().handed;
        if let Some(g) = handed {
            self.stop(g, Stop::Cancelled);
        }
    }

    /// Records `why` for send `g` unless it is no longer handed out or has a stop already.
    fn stop(&self, g: u64, why: Stop) {
        {
            let mut t = self.lock();
            if t.handed != Some(g) || t.stop.is_some_and(|(s, ..)| s == g) {
                return;
            }
            t.stop = Some((g, why, std::time::Instant::now()));
            self.attention.request();
        }
        self.stopped.notify_waiters();
    }

    /// Requests Attention for send `g`, still handed out, with no cause to report: Close, a
    /// dropped session and the drain cap end the session after it.
    fn attend(&self, g: u64) {
        let t = self.lock();
        if t.handed == Some(g) {
            self.attention.request();
        }
    }

    /// Hands out the next send's generation, ending the one before it.
    fn begin(&self) -> u64 {
        let mut t = self.lock();
        Self::idle(&mut t);
        t.last += 1;
        t.handed = Some(t.last);
        t.last
    }

    /// No send is handed out any more.
    fn end(&self) {
        Self::idle(&mut self.lock());
    }

    /// Send `g`'s response was read to its end, so nothing of it is left to stop.
    fn finish(&self, g: u64) {
        let mut t = self.lock();
        if t.handed == Some(g) {
            Self::idle(&mut t);
        }
    }

    fn idle(t: &mut Target) {
        t.handed = None;
        if let Some(watchdog) = t.watchdog.take() {
            watchdog.abort();
        }
    }

    /// Arms send `g`'s statement timeout; called just before the send.
    fn arm(self: &Arc<Self>, g: u64, after: Duration) {
        let this = Arc::clone(self);
        let task = tokio::spawn(async move {
            tokio::time::sleep(after).await;
            this.stop(g, Stop::Timeout);
        });
        if let Some(old) = self.lock().watchdog.replace(task.abort_handle()) {
            old.abort();
        }
    }

    /// Repeats a stop that came while the batch was being written, since the write forgets
    /// any Attention requested before it.
    fn sent(&self, g: u64) {
        let t = self.lock();
        if t.stop.is_some_and(|(s, ..)| s == g) {
            self.attention.request();
        }
    }

    /// The stop requested for send `g`, if any.
    fn stopped(&self, g: u64) -> Option<Stop> {
        self.lock()
            .stop
            .filter(|&(s, ..)| s == g)
            .map(|(_, why, _)| why)
    }

    /// When send `g`'s response must be gone: `ATTENTION_BOUND` after its first stop request.
    fn deadline(&self, g: u64) -> Option<tokio::time::Instant> {
        self.lock()
            .stop
            .filter(|&(s, ..)| s == g)
            .map(|(_, _, at)| tokio::time::Instant::from_std(at + ATTENTION_BOUND))
    }

    /// Resolves with send `g`'s stop once one is requested.
    async fn until_stopped(&self, g: u64) -> Stop {
        loop {
            let mut woken = std::pin::pin!(self.stopped.notified());
            woken.as_mut().enable();
            if let Some(why) = self.stopped(g) {
                return why;
            }
            woken.await;
        }
    }
}

impl MssqlDriver {
    pub(crate) fn server(&self) -> ServerInfo {
        self.server
    }

    pub(crate) fn settings_changed(&self) -> bool {
        self.settings_changed.load(Ordering::SeqCst)
    }

    /// A read-only session's level: the database's with a reader principal, otherwise Dbento's
    /// fence and refusals.
    pub(crate) fn enforcement(&self) -> ReadOnlyEnforcement {
        if self.reader {
            ReadOnlyEnforcement::Database
        } else {
            ReadOnlyEnforcement::BestEffort
        }
    }

    /// Runs Dbento's own `sql` on the connection's database with `params` bound as @P1..,
    /// returning each row's string cells as text or NULL.
    pub(crate) async fn catalog(
        &mut self,
        sql: &str,
        params: &[&str],
    ) -> Result<Vec<Vec<Option<String>>>, IntrospectError> {
        let (reply, wait) = oneshot::channel();
        let request = Request::Catalog {
            sql: sql.to_owned(),
            params: params.iter().map(|p| (*p).to_owned()).collect(),
            reply,
        };
        if self.requests.send(request).is_err() {
            return Err(ExecuteError::SessionClosed.into());
        }
        Ok(wait.await.unwrap_or(Err(ExecuteError::SessionClosed))?)
    }

    /// The session's database once a USE moved it off the login's; None on failure.
    pub(crate) async fn moved_database(&mut self) -> Option<String> {
        let (reply, wait) = oneshot::channel();
        self.requests.send(Request::Moved { reply }).ok()?;
        wait.await.ok().flatten()
    }
}

/// A dropped session stops its send in flight; the thread reads through the acknowledgement,
/// then finds its channels closed and exits, closing the socket.
impl Drop for MssqlDriver {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Driver for MssqlDriver {
    type Script = MssqlScript;
    type Stream = MssqlStream;

    async fn connect(params: ConnectParams<'_>) -> Result<Self, ConnectError> {
        let conn = params.conn;
        let mut config = Config::new();
        config.host(conn.host().unwrap_or("localhost"));
        config.port(conn.port().unwrap_or(1433));
        if let Some(database) = conn.database() {
            config.database(database);
        }
        config.authentication(AuthMethod::sql_server(
            conn.user().unwrap_or_default(),
            params.secret.map_or("", |s| s.expose()),
        ));
        // Off still encrypts the login packet, so its certificate is taken as it comes.
        match conn.tls() {
            Some(TlsMode::Off) => {
                config.encryption(EncryptionLevel::Off);
                config.trust_cert();
            }
            Some(TlsMode::Verify) | None => {
                config.encryption(EncryptionLevel::Required);
                if let Some(ca) = conn.tls_ca() {
                    config.trust_cert_ca(ca.display());
                }
            }
        }
        config.command_timeout(None);
        config.application_name("dbento");
        let host = conn.host().unwrap_or_default().to_owned();
        let reader = conn
            .read_only_user()
            .filter(|_| params.read_only)
            .map(str::to_owned);
        let reads_as_reader = reader.is_some();
        let settings_changed = Arc::new(AtomicBool::new(false));
        let rules = conn::Rules {
            timeout: params.timeout,
            read_only: params.read_only,
            reader,
            settings_changed: Arc::clone(&settings_changed),
        };
        let (requests, server, cancel) =
            conn::start(config, host, conn.display_target(), rules).await?;
        Ok(MssqlDriver {
            requests,
            server,
            cancel,
            reader: reads_as_reader,
            settings_changed,
        })
    }

    fn start_script(&mut self, sql: &str) -> MssqlScript {
        let (ctl, ctl_rx) = mpsc::unbounded_channel();
        // A gone thread drops the receiver; the first `Next` then reports the session closed.
        let _ = self.requests.send(Request::Script {
            sql: sql.to_owned(),
            ctl: ctl_rx,
        });
        MssqlScript { ctl }
    }

    async fn prepare_next(script: &mut MssqlScript) -> Option<Result<Prepared, ExecuteError>> {
        let (reply, wait) = oneshot::channel();
        if script.ctl.send(Ctl::Next { reply }).is_err() {
            return Some(Err(ExecuteError::SessionClosed));
        }
        wait.await.unwrap_or(Some(Err(ExecuteError::SessionClosed)))
    }

    async fn is_last(script: &mut MssqlScript) -> bool {
        let (reply, wait) = oneshot::channel();
        if script.ctl.send(Ctl::IsLast { reply }).is_err() {
            return true;
        }
        wait.await.unwrap_or(true)
    }

    async fn execute(
        script: &mut MssqlScript,
        limits: Limits,
    ) -> Result<(Vec<ColumnInfo>, MssqlStream), ExecuteError> {
        let (reply, wait) = oneshot::channel();
        let (events, rx) = mpsc::channel(1);
        let (resume, resume_rx) = match limits.on_truncate {
            OnTruncate::Park => {
                let (tx, rx) = mpsc::unbounded_channel();
                (Some(tx), Some(rx))
            }
            OnTruncate::Drain | OnTruncate::Close => (None, None),
        };
        let sent = script.ctl.send(Ctl::Execute {
            reply,
            events,
            limits,
            resume: resume_rx,
        });
        if sent.is_err() {
            return Err(ExecuteError::SessionClosed);
        }
        let columns = wait.await.map_err(|_| ExecuteError::SessionClosed)??;
        let stream = MssqlStream {
            events: rx,
            resume,
            parked: false,
        };
        Ok((columns, stream))
    }

    async fn next_event(stream: &mut MssqlStream) -> Result<StreamItem, ExecuteError> {
        match stream.events.recv().await {
            Some(Event::Batch(b)) => Ok(StreamItem::Batch(b)),
            Some(Event::Done(o)) => Ok(StreamItem::Done(o)),
            Some(Event::Parked(o)) => {
                stream.parked = true;
                Ok(StreamItem::Done(o))
            }
            Some(Event::Failed(e)) => Err(e),
            None => Err(ExecuteError::SessionClosed),
        }
    }

    fn extend(stream: &mut MssqlStream, more: Option<u64>) -> Result<(), ExtendError> {
        if !std::mem::take(&mut stream.parked) {
            return Err(ExtendError::NotParked);
        }
        let resume = stream.resume.as_ref().ok_or(ExtendError::NotParked)?;
        resume.send(more).map_err(|_| ExtendError::SessionClosed)
    }

    fn cancel_handle(&self) -> CancelHandle {
        CancelHandle::mssql(Arc::clone(&self.cancel))
    }

    /// Reports, once, the drain error of a statement whose stream nobody was reading.
    async fn settle(&mut self) -> Result<(), ExecuteError> {
        let (done, wait) = oneshot::channel();
        if self.requests.send(Request::Settle { done }).is_err() {
            return Err(ExecuteError::SessionClosed);
        }
        wait.await.unwrap_or(Err(ExecuteError::SessionClosed))
    }

    /// Drops the client on its thread and returns any unreported drain error.
    async fn close(self) -> Result<(), ExecuteError> {
        let (done, wait) = oneshot::channel();
        if self.requests.send(Request::Close { done }).is_err() {
            return Ok(());
        }
        wait.await.unwrap_or(Ok(()))
    }

    async fn schemas(&mut self) -> Result<Vec<SchemaInfo>, IntrospectError> {
        catalog::schemas(self).await
    }

    async fn relations(&mut self, schema: &str) -> Result<RelationList, IntrospectError> {
        catalog::relations(self, schema).await
    }

    async fn columns(&mut self, rel: &RelationRef) -> Result<Vec<ColumnMeta>, IntrospectError> {
        catalog::columns(self, rel).await
    }
    fn read_only_level(&self, requested: bool) -> Option<ReadOnlyEnforcement> {
        requested.then(|| self.enforcement())
    }
}

/// A failed login as a `ConnectError`: a refused certificate is `Tls` with the fix in Dbento's
/// terms (tiberius's own advice names its API, so it is cut); a server error is
/// `"<target>: <message> (<number>)"`; anything else names the target.
fn connect_error(host: &str, target: &str, e: &tiberius::error::Error) -> ConnectError {
    use tiberius::error::Error;
    let message = match e {
        Error::Tls(m) => {
            // The stock certificate is X.509 v1 with no SAN, which rustls refuses outright.
            let cause = if m.contains("UnsupportedCertVersion") {
                "the server presents a self-signed X.509 v1 certificate (SQL Server's built-in \
                 one), which certificate verification refuses"
            } else {
                m.split_once(". ").map_or(m.as_str(), |(head, _)| head)
            };
            return ConnectError::Tls {
                host: host.to_owned(),
                message: format!("{cause}; {}", crate::postgres::TLS_WAYS_OUT),
            };
        }
        Error::Routing { host, port } => {
            format!(
                "{target}: the server redirects to {host}:{port}, and redirects are not followed"
            )
        }
        Error::Server(t) => format!("{target}: {} ({})", t.message(), t.code()),
        other @ (Error::Io { .. }
        | Error::Protocol(_)
        | Error::Encoding(_)
        | Error::Conversion(_)
        | Error::Utf8
        | Error::Utf16
        | Error::ParseInt(_)
        | Error::BulkInput(_)) => format!("{target}: {other}"),
    };
    ConnectError::Open { message }
}

/// Whether server error `code` is a refused write: 3906 (a read-only database) always, and
/// with a reader principal the permission-denied family.
fn refused_write(code: u32, reader: bool) -> bool {
    // 6115 is how KILL ends inside the fence, ahead of its permission check 6102.
    const FAMILY: [u32; 16] = [
        229, 230, 262, 297, 916, 1088, 2104, 2571, 3505, 3701, 4834, 6102, 6115, 7983, 15151, 15247,
    ];
    code == 3906 || (reader && FAMILY.contains(&code))
}

/// A server's error in Dbento's terms, in a session that has a reader principal or not.
fn database_error(e: &TokenError, reader: bool) -> ExecuteError {
    if refused_write(e.code(), reader) {
        return ExecuteError::ReadOnlyViolation {
            message: e.message().to_owned(),
        };
    }
    let mut error = ExecuteError::database(e.code().to_string(), e.message());
    if let ExecuteError::Database { class, state, .. } = &mut error {
        *class = Some(e.class());
        *state = Some(e.state());
    }
    error
}

/// A tiberius error read mid-response.
struct StreamError {
    error: ExecuteError,
    /// A server error is the statement's own, but any other leaves tiberius's token decoder
    /// out of step, so the session closes.
    closes: bool,
}

fn stream_error(e: &tiberius::error::Error, reader: bool) -> StreamError {
    use tiberius::error::Error;
    let reason = match e {
        Error::Server(t) => {
            return StreamError {
                error: database_error(t, reader),
                closes: false,
            };
        }
        Error::Io { .. } | Error::Protocol(_) => {
            return StreamError {
                error: ExecuteError::SessionClosed,
                closes: true,
            };
        }
        Error::Encoding(m) => format!("a text cell could not be decoded: {m}"),
        other @ (Error::Conversion(_)
        | Error::Utf8
        | Error::Utf16
        | Error::ParseInt(_)
        | Error::Tls(_)
        | Error::Routing { .. }
        | Error::BulkInput(_)) => format!("a cell could not be decoded: {other}"),
    };
    StreamError {
        error: ExecuteError::Unsupported { reason },
        closes: true,
    }
}

/// Major, minor and build of `SERVERPROPERTY('ProductVersion')`, such as "16.0.4295.3".
fn product_version(text: &str) -> Option<(u16, u16, u16)> {
    let mut parts = text.split('.').map(|p| p.parse::<u16>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiberius::error::Error;

    #[test]
    fn the_version_is_the_first_three_parts() {
        assert_eq!(product_version("16.0.4295.3"), Some((16, 0, 4295)));
        assert_eq!(product_version("17.0.1000.7"), Some((17, 0, 1000)));
        assert_eq!(product_version("sixteen"), None);
    }

    #[test]
    fn tls_failures_name_tls_ca_and_off_but_not_tiberius_calls() {
        let v1 = Error::Tls("the server's certificate was rejected during the TLS handshake: Other(OtherError(UnsupportedCertVersion)). The SQL Server presented a certificate rustls considers too old. To connect anyway you can: (1) trust a specific CA certificate with `Config::trust_cert_ca(path)`".into());
        let issuer = Error::Tls("the server's certificate was rejected during the TLS handshake: UnknownIssuer. To connect anyway you can: (3) skip certificate validation entirely with `Config::trust_cert()`".into());
        for (e, want) in [(v1, "X.509 v1"), (issuer, "UnknownIssuer")] {
            match connect_error("db.example.com", "app@db.example.com:1433", &e) {
                ConnectError::Tls { host, message } => {
                    assert_eq!(host, "db.example.com");
                    assert!(
                        message.contains(want) && message.contains("tls_ca"),
                        "{message}"
                    );
                    assert!(
                        message.contains("tls = \"off\"") && !message.contains("Config::"),
                        "{message}"
                    );
                }
                other @ (ConnectError::NotFound { .. }
                | ConnectError::Open { .. }
                | ConnectError::InvalidTimeout { .. }
                | ConnectError::Unsupported { .. }) => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn a_tls_failure_ends_with_the_ways_out_shared_with_mysql() {
        let e = Error::Tls("UnknownIssuer. To connect anyway you can: skip it".into());
        match connect_error("h", "t", &e) {
            ConnectError::Tls { message, .. } => assert_eq!(
                message,
                format!("UnknownIssuer; {}", crate::postgres::TLS_WAYS_OUT)
            ),
            other @ (ConnectError::NotFound { .. }
            | ConnectError::Open { .. }
            | ConnectError::InvalidTimeout { .. }
            | ConnectError::Unsupported { .. }) => panic!("{other:?}"),
        }
        assert_eq!(
            crate::postgres::TLS_WAYS_OUT,
            "trust the server's CA with tls_ca, or set tls = \"off\""
        );
    }

    #[test]
    fn non_server_errors_close_the_session_and_name_what_failed() {
        let cases = [
            (
                Error::Io {
                    kind: std::io::ErrorKind::ConnectionReset,
                    message: "reset".into(),
                },
                "the session is closed",
            ),
            (Error::Protocol("bad token".into()), "the session is closed"),
            (
                Error::Encoding("encoding: unspported encoding (LCID: 0x0429, sort ID: 0)".into()),
                "LCID: 0x0429",
            ),
            (Error::Conversion("bad numeric".into()), "bad numeric"),
            (Error::Utf16, "UTF-16"),
        ];
        for (e, want) in cases {
            let StreamError { error: err, closes } = stream_error(&e, false);
            assert!(closes, "{e:?}");
            assert!(err.to_string().contains(want), "{e:?}: {err}");
            assert!(!matches!(err, ExecuteError::Database { .. }), "{e:?}");
        }
    }

    #[test]
    fn the_permission_family_is_a_violation_only_under_a_reader() {
        for code in [
            229, 230, 262, 297, 916, 1088, 2104, 2571, 3505, 3701, 4834, 6102, 6115, 7983, 15151,
            15247,
        ] {
            assert!(refused_write(code, true), "{code}");
            assert!(!refused_write(code, false), "{code}");
        }
        assert!(refused_write(3906, true) && refused_write(3906, false));
        for code in [8134, 208, 15517, 15196, 50000] {
            assert!(!refused_write(code, true), "{code}");
        }
    }

    #[test]
    fn a_routing_redirect_names_its_target_and_is_not_followed() {
        let e = Error::Routing {
            host: "gw.example.net".into(),
            port: 11000,
        };
        match connect_error("db.example.com", "app@db.example.com:1433", &e) {
            ConnectError::Open { message } => assert!(
                message.contains("gw.example.net:11000")
                    && message.contains("app@db.example.com:1433"),
                "{message}"
            ),
            other @ (ConnectError::NotFound { .. }
            | ConnectError::Tls { .. }
            | ConnectError::InvalidTimeout { .. }
            | ConnectError::Unsupported { .. }) => panic!("{other:?}"),
        }
    }
}
