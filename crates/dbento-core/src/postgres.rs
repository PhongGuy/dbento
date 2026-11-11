//! The Postgres driver over tokio-postgres.
//!
//! The `Connection` future runs on a spawned task and does all socket I/O; scripts share the
//! session's `Arc<Client>`. Each statement is prepared first, for its column types and so a
//! failing statement runs nothing, then run with the simple query protocol so every value
//! arrives as the server's own text. A read-only session runs each statement in its own
//! `BEGIN READ ONLY` transaction, which the session ends before its next request or at the
//! end of the script.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::ops::Range;
#[cfg(unix)]
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, ready};
use std::time::Duration;

use futures_util::{FutureExt as _, StreamExt as _};
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls_platform_verifier::Verifier;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::runtime::Handle;
use tokio::sync::watch;
use tokio::task::{AbortHandle, JoinHandle};
use tokio_postgres::config::{Host, SslMode};
use tokio_postgres::error::ErrorPosition;
use tokio_postgres::tls::MakeTlsConnect;
use tokio_postgres::types::{FromSql, Kind, ToSql, Type};
use tokio_postgres::{
    CancelToken, Client, Column, Config, NoTls, Row, SimpleQueryMessage, SimpleQueryRow,
    SimpleQueryStream, Statement,
};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::Secret;
use crate::classify::{PgReach, pg_classification, pg_reach};
use crate::introspect::{
    ColumnMeta, IntrospectError, RELATION_CAP, Relation, RelationKind, RelationList, RelationRef,
    SchemaInfo, capped,
};
use crate::rowedit::Param;
use crate::secrets::{is_encrypted_pem, key_passphrase_env_var};
use crate::session::{
    CancelHandle, ConnectError, ConnectParams, DRAIN_CAP, Driver, ExecuteError, ExtendError,
    Limits, OnTruncate, Outcome, Prepared, StreamItem, TruncatedBy, Writes,
};
use crate::split::split;
use crate::value::{BatchBuilder, CellRef, ColumnInfo, ColumnSource};
use crate::workspace::{ConnectionConfig, TlsMode};

/// Startup options: UTF-8 text in the forms the grid expects, then the connection's settings,
/// then the statement timeout. Values escape whitespace and `\` so one setting is one option.
fn startup_options(settings: &BTreeMap<String, String>, timeout: Option<Duration>) -> String {
    let mut out =
        String::from("-c client_encoding=UTF8 -c bytea_output=hex -c application_name=dbento");
    for (name, value) in settings {
        out.push_str(" -c ");
        out.push_str(name);
        out.push('=');
        for c in value.chars() {
            if c == '\\' || c.is_whitespace() {
                out.push('\\');
            }
            out.push(c);
        }
    }
    if let Some(d) = timeout {
        // Whole milliseconds, rounded up: 0 would disable the timeout.
        out.push_str(&format!(
            " -c statement_timeout={}",
            d.as_micros().div_ceil(1000).max(1)
        ));
    }
    out
}

/// How long a connect, from TCP through auth, may take before it fails; without it a host
/// that drops SYNs or a server that never answers the startup hangs for minutes or forever.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// TCP keepalive: probe a connection idle this long, then every interval, and drop it after
/// this many unanswered probes, so a silent network drop ends an idle session within about
/// a minute rather than the OS default of over two hours.
const KEEPALIVE_IDLE: Duration = Duration::from_secs(30);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
const KEEPALIVE_RETRIES: u32 = 3;

/// Host (a leading `/` is a socket directory), port, user, dbname, password, options, ssl mode.
fn pg_config(
    conn: &ConnectionConfig,
    secret: Option<&Secret>,
    timeout: Option<Duration>,
) -> Config {
    let mut config = Config::new();
    config
        .ssl_mode(SslMode::Disable)
        .connect_timeout(CONNECT_TIMEOUT)
        .keepalives(true)
        .keepalives_idle(KEEPALIVE_IDLE)
        .keepalives_interval(KEEPALIVE_INTERVAL)
        .keepalives_retries(KEEPALIVE_RETRIES)
        .options(startup_options(conn.settings(), timeout));
    if let Some(host) = conn.host() {
        config.host(host);
    }
    if let Some(port) = conn.port() {
        config.port(port);
    }
    if let Some(user) = conn.user() {
        config.user(user);
    }
    if let Some(database) = conn.database() {
        config.dbname(database);
    }
    if let Some(secret) = secret {
        config.password(secret.expose());
    }
    config
}

/// The server's message for a server error; a TLS failure as `Tls`; else the target plus the
/// error and its sources.
fn connect_error(conn: &ConnectionConfig, e: tokio_postgres::Error) -> ConnectError {
    if let Some(db) = e.as_db_error() {
        return ConnectError::Open {
            message: db.message().to_owned(),
        };
    }
    let mut chain = String::new();
    let mut source = std::error::Error::source(&e);
    while let Some(s) = source {
        if !chain.is_empty() {
            chain.push_str(": ");
        }
        chain.push_str(&s.to_string());
        source = s.source();
    }
    // The connect timeout surfaces as an io TimedOut at the end of the source chain.
    let mut cause = std::error::Error::source(&e);
    while let Some(s) = cause {
        if s.downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::TimedOut)
        {
            return timed_out(conn);
        }
        cause = s.source();
    }
    // tokio-postgres has no public error kind; every TLS failure, including a server that
    // refuses TLS, displays exactly this text, with the cause in the source chain.
    if e.to_string() == "error performing TLS handshake" {
        return tls_error(conn, chain);
    }
    let mut message = format!("{}: {e}", conn.display_target());
    if !chain.is_empty() {
        message.push_str(": ");
        message.push_str(&chain);
    }
    ConnectError::Open { message }
}

/// The connect did not finish within `CONNECT_TIMEOUT`, naming the host and port.
fn timed_out(conn: &ConnectionConfig) -> ConnectError {
    let host = conn.host().unwrap_or_default();
    let port = conn.port().map(|p| format!(":{p}")).unwrap_or_default();
    ConnectError::Open {
        message: format!(
            "{}: host {host}{port} did not answer within {} s",
            conn.display_target(),
            CONNECT_TIMEOUT.as_secs()
        ),
    }
}

/// The two ways out of a refused server certificate, one wording for every engine's hint.
pub(crate) const TLS_WAYS_OUT: &str = "trust the server's CA with tls_ca, or set tls = \"off\"";

pub(crate) fn tls_error(conn: &ConnectionConfig, message: String) -> ConnectError {
    ConnectError::Tls {
        host: conn.host().unwrap_or_default().to_owned(),
        message,
    }
}

/// The client certificate chain and key from `tls_cert` and `tls_key`, when both are set.
/// An encrypted PKCS#8 key is decrypted with `passphrase`; a missing or wrong one is an error
/// that names the key file and where a passphrase goes, never the passphrase itself.
pub(crate) fn client_auth(
    conn: &ConnectionConfig,
    passphrase: Option<&Secret>,
) -> Result<Option<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)>, ConnectError> {
    let (Some(cert_path), Some(key_path)) = (conn.tls_cert(), conn.tls_key()) else {
        return Ok(None);
    };
    let fail = |what: String| tls_error(conn, format!("{}: {what}", key_path.display()));
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert_path)
        .and_then(|it| it.collect())
        .map_err(|e| tls_error(conn, format!("{}: {e}", cert_path.display())))?;
    if chain.is_empty() {
        return Err(tls_error(
            conn,
            format!("no certificates in {}", cert_path.display()),
        ));
    }
    let pem = std::fs::read(key_path).map_err(|e| fail(e.to_string()))?;
    let key = if is_encrypted_pem(&pem) {
        let name = conn.name();
        let Some(passphrase) = passphrase else {
            return Err(fail(format!(
                "the key is encrypted and no passphrase was found: run `dbento secret set \
                 --tls-key {name}` or set {}",
                key_passphrase_env_var(name)
            )));
        };
        let text = std::str::from_utf8(&pem).map_err(|_| fail("not a PEM file".to_owned()))?;
        let (_, encrypted) = pkcs8::SecretDocument::from_pem(text)
            .map_err(|e| fail(format!("not an encrypted PKCS#8 key: {e}")))?;
        let info = pkcs8::EncryptedPrivateKeyInfoRef::try_from(encrypted.as_bytes())
            .map_err(|e| fail(format!("not an encrypted PKCS#8 key: {e}")))?;
        // A wrong passphrase decrypts to garbage that fails the padding or DER check.
        let plain = info
            .decrypt(passphrase.expose())
            .map_err(|_| fail("cannot decrypt the key: wrong passphrase".to_owned()))?;
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(plain.as_bytes().to_vec()))
    } else if pem.windows(10).any(|w| w == b"Proc-Type:") {
        return Err(fail(
            "legacy encrypted PEM keys are not supported; convert it with \
             `openssl pkcs8 -topk8 -v2 aes-256-cbc`"
                .to_owned(),
        ));
    } else {
        PrivateKeyDer::from_pem_slice(&pem).map_err(|e| fail(e.to_string()))?
    };
    Ok(Some((chain, key)))
}

/// A rustls connector that trusts the platform's roots plus the certificates in `tls_ca`,
/// presenting the client certificate when one is configured.
/// There is deliberately no non-verifying mode.
fn rustls_connector(
    conn: &ConnectionConfig,
    passphrase: Option<&Secret>,
) -> Result<MakeRustlsConnect, ConnectError> {
    rustls_config(conn, passphrase).map(MakeRustlsConnect::new)
}

/// The client configuration behind `rustls_connector`, also used by SurrealDB's WebSocket.
pub(crate) fn rustls_config(
    conn: &ConnectionConfig,
    passphrase: Option<&Secret>,
) -> Result<rustls::ClientConfig, ConnectError> {
    let client = client_auth(conn, passphrase)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let extra: Vec<CertificateDer<'static>> = match conn.tls_ca() {
        Some(path) => CertificateDer::pem_file_iter(path)
            .and_then(|it| it.collect())
            .map_err(|e| tls_error(conn, format!("{}: {e}", path.display())))?,
        None => Vec::new(),
    };
    // A CA file that adds no roots is a mistake the user should hear about, not ignore.
    if let Some(path) = conn.tls_ca()
        && extra.is_empty()
    {
        return Err(tls_error(
            conn,
            format!("no certificates in {}", path.display()),
        ));
    }
    let verifier = Verifier::new_with_extra_roots(extra, Arc::clone(&provider))
        .map_err(|e| tls_error(conn, e.to_string()))?;
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| tls_error(conn, e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier));
    match client {
        None => Ok(builder.with_no_client_auth()),
        Some((chain, key)) => builder
            .with_client_auth_cert(chain, key)
            .map_err(|e| tls_error(conn, format!("client certificate: {e}"))),
    }
}

/// The connector a session was opened with; its cancel request must use the same one.
#[derive(Clone)]
enum Connector {
    Plain,
    Rustls(MakeRustlsConnect),
}

/// Where a cancel request connects: the session's first host and port.
#[derive(Clone)]
enum CancelTarget {
    Tcp {
        host: String,
        port: u16,
    },
    /// The socket file itself, not its directory.
    #[cfg(unix)]
    Unix(PathBuf),
}

impl CancelTarget {
    fn of(config: &Config) -> Option<Self> {
        let port = config.get_ports().first().copied().unwrap_or(5432);
        match config.get_hosts().first()? {
            Host::Tcp(host) => Some(CancelTarget::Tcp {
                host: host.clone(),
                port,
            }),
            #[cfg(unix)]
            Host::Unix(dir) => Some(CancelTarget::Unix(dir.join(format!(".s.PGSQL.{port}")))),
        }
    }
}

/// How long a cancel request may take before the next request stops waiting for it.
const CANCEL_WAIT: Duration = Duration::from_secs(2);

/// Cancels whatever the session's connection is running; usable from any thread.
pub(crate) struct PgCancel {
    token: CancelToken,
    connector: Connector,
    runtime: Handle,
    /// Set by `cancel`, cleared when `execute` starts and when `settle` ends: a 57014 with it
    /// set was a cancel.
    requested: AtomicBool,
    target: Option<CancelTarget>,
    /// Cancel requests not yet known to have reached the server.
    inflight: Mutex<Vec<JoinHandle<()>>>,
}

impl PgCancel {
    /// Sends a cancel request from the session's runtime, over TLS when the session uses it.
    /// A no-op once that runtime has shut down: the spawned task is dropped unrun.
    pub(crate) fn cancel(&self) {
        self.requested.store(true, Ordering::SeqCst);
        // The cancel request opens its own connection, so it runs on the session's runtime.
        let (token, connector, target) = (
            self.token.clone(),
            self.connector.clone(),
            self.target.clone(),
        );
        let task = self.runtime.spawn(async move {
            let _ = tokio::time::timeout(CANCEL_WAIT, send_cancel(token, connector, target)).await;
        });
        self.inflight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(task);
    }

    /// Waits until every earlier cancel request has reached the server, so none of them can
    /// land on the request that follows.
    async fn spend(&self) {
        let tasks =
            std::mem::take(&mut *self.inflight.lock().unwrap_or_else(PoisonError::into_inner));
        for task in tasks {
            let _ = task.await;
        }
    }
}

/// Sends one cancel request and, where the stream can be wrapped, returns only once the
/// server has hung up. A Unix socket under Rustls, or no target, falls back to tokio-postgres's
/// own request, which returns after the write. Errors are ignored: the statement runs on.
async fn send_cancel(token: CancelToken, connector: Connector, target: Option<CancelTarget>) {
    let _ = match (connector, target) {
        (Connector::Plain, Some(CancelTarget::Tcp { host, port })) => {
            match TcpStream::connect((host.as_str(), port)).await {
                Ok(s) => token.cancel_query_raw(UntilEof::new(s), NoTls).await,
                Err(_) => return,
            }
        }
        #[cfg(unix)]
        (Connector::Plain, Some(CancelTarget::Unix(path))) => match UnixStream::connect(path).await
        {
            Ok(s) => token.cancel_query_raw(UntilEof::new(s), NoTls).await,
            Err(_) => return,
        },
        (Connector::Rustls(mut tls), Some(CancelTarget::Tcp { host, port })) => {
            let Ok(s) = TcpStream::connect((host.as_str(), port)).await else {
                return;
            };
            let Ok(connect) =
                MakeTlsConnect::<UntilEof<TcpStream>>::make_tls_connect(&mut tls, &host);
            token.cancel_query_raw(UntilEof::new(s), connect).await
        }
        (Connector::Plain, None) => token.cancel_query(NoTls).await,
        (Connector::Rustls(tls), _) => token.cancel_query(tls).await,
    };
}

/// A cancel connection whose shutdown waits for the server to hang up. The server signals
/// the backend before closing, so the cancel has landed once shutdown returns.
struct UntilEof<S> {
    inner: S,
    shut: bool,
}

impl<S> UntilEof<S> {
    fn new(inner: S) -> Self {
        UntilEof { inner, shut: false }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for UntilEof<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for UntilEof<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if !this.shut {
            ready!(Pin::new(&mut this.inner).poll_shutdown(cx))?;
            this.shut = true;
        }
        let mut scratch = [0u8; 64];
        loop {
            let mut buf = ReadBuf::new(&mut scratch);
            match ready!(Pin::new(&mut this.inner).poll_read(cx, &mut buf)) {
                Ok(()) if buf.filled().is_empty() => return Poll::Ready(Ok(())),
                Ok(()) => {}
                // A reset is a hang-up too.
                Err(_) => return Poll::Ready(Ok(())),
            }
        }
    }
}

impl fmt::Debug for PgCancel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PgCancel").finish_non_exhaustive()
    }
}

/// A transaction block the session opened and owes a close for.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Block {
    #[default]
    None,
    /// The read-only wrapper; its close also drops advisory locks, which survive a rollback.
    ReadOnly,
    /// An explain's own `BEGIN` on a writable session.
    Begin,
    /// An explain's savepoint inside the user's block, so only the explain is rolled back.
    Savepoint,
}

impl Block {
    fn open(self) -> &'static str {
        match self {
            Block::None => "",
            Block::ReadOnly => "BEGIN READ ONLY; SELECT 1",
            Block::Begin => "BEGIN",
            Block::Savepoint => "SAVEPOINT dbento_explain",
        }
    }

    fn close(self) -> Option<&'static str> {
        match self {
            Block::None => None,
            Block::ReadOnly => Some("ROLLBACK; SELECT pg_advisory_unlock_all()"),
            Block::Begin => Some("ROLLBACK"),
            Block::Savepoint => {
                Some("ROLLBACK TO SAVEPOINT dbento_explain; RELEASE SAVEPOINT dbento_explain")
            }
        }
    }
}

/// Work a statement leaves for the session to finish before its next request.
#[derive(Default)]
struct Owed {
    /// The open block whose close is owed; set before its open is sent.
    block: Block,
    /// An explain's request was sent and may still run: it is cancelled before the close.
    request_running: bool,
    /// The unread rest of a truncated or dropped result. Kept rather than dropped, since
    /// tokio-postgres would drain a dropped stream where no cap reaches it.
    leftover: Option<Pin<Box<SimpleQueryStream>>>,
    /// The leftover's statement cannot write, so aborting it loses nothing.
    abortable: bool,
    /// A possible write's drain error that no caller has reported; `close` or `Session::settle` does.
    unreported: Option<ExecuteError>,
}

/// One per `PgDriver`, shared with its scripts and streams.
type SharedOwed = Arc<Mutex<Owed>>;

/// Reads the rest of an unfinished result for at most `DRAIN_CAP`, closing the session past
/// it, then sends the open block's close. ROLLBACK discards the statement's transactional
/// effects, and the read-only wrapper's unlock drops session-level advisory locks.
/// The drain's first error is returned once the block has ended; a closed session wins.
/// A read-write statement's drain error is kept as unreported instead, so a quiet settle cannot lose it.
///
/// Earlier cancel requests are spent first; the cancel flag is cleared only at the end, so a
/// cancel during the drain still reads as one.
async fn settle(
    client: &Client,
    owed: &SharedOwed,
    abort: &AbortHandle,
    cancel: &PgCancel,
) -> Result<(), ExecuteError> {
    cancel.spend().await;
    let result = settle_owed(client, owed, abort, cancel).await;
    cancel.requested.store(false, Ordering::SeqCst);
    result
}

/// `settle` for a caller that reports the statement's own result: a drain error belongs to an
/// earlier statement and only `Session::settle` surfaces it, so only a closed session counts.
async fn settle_quietly(
    client: &Client,
    owed: &SharedOwed,
    abort: &AbortHandle,
    cancel: &PgCancel,
) -> Result<(), ExecuteError> {
    match settle(client, owed, abort, cancel).await {
        Err(e) if e.closes_session() => Err(e),
        _ => Ok(()),
    }
}

async fn settle_owed(
    client: &Client,
    owed: &SharedOwed,
    abort: &AbortHandle,
    cancel: &PgCancel,
) -> Result<(), ExecuteError> {
    // The block and the running flag stay owed through every await, so a settle dropped
    // midway leaves them for the next one; each is cleared only once its work is done.
    let (leftover, abortable, running) = {
        let mut o = owed.lock().unwrap_or_else(PoisonError::into_inner);
        (o.leftover.take(), o.abortable, o.request_running)
    };
    // A dropped explain may still be running: stop it, and let the cancel land before the close.
    if running {
        cancel.cancel();
        cancel.spend().await;
        owed.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .request_running = false;
    }
    let mut drain_error = None;
    if let Some(mut rows) = leftover {
        // Read to the end even past an error, so the connection is ready for the next request.
        let drain = async {
            while let Some(item) = rows.next().await {
                if let Err(e) = item {
                    drain_error.get_or_insert_with(|| execute_error(e, cancel, None));
                }
            }
        };
        if tokio::time::timeout(DRAIN_CAP, drain).await.is_err() {
            if abortable {
                abort.abort();
                return Err(ExecuteError::SessionClosed);
            }
            stop_at_cap(cancel, abort).await;
            return Err(ExecuteError::StoppedAtDrainCap);
        }
    }
    // Taken in the same poll that sends the close, since `batch_execute` sends on first poll.
    let block = std::mem::take(&mut owed.lock().unwrap_or_else(PoisonError::into_inner).block);
    if let Some(close) = block.close()
        && client.batch_execute(close).await.is_err()
    {
        // The transaction state is unknown, so the session must not run anything else.
        abort.abort();
        return Err(ExecuteError::SessionClosed);
    }
    match drain_error {
        Some(e) if !abortable => {
            owed.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .unreported
                .get_or_insert(e);
            Ok(())
        }
        other => other.map_or(Ok(()), Err),
    }
}

/// Stops a statement that may write and outlasted the drain cap. The cancel lands before the
/// connection drops, so the statement fails and rolls back instead of running on to commit
/// unseen by a server that has not yet noticed the client is gone.
async fn stop_at_cap(cancel: &PgCancel, abort: &AbortHandle) {
    cancel.cancel();
    cancel.spend().await;
    abort.abort();
}

/// True when nothing in a read's reach is volatile. Postgres refuses a write inside a STABLE
/// or IMMUTABLE function, so then the read cannot write. Every name must resolve; a view,
/// foreign table, row-security table, domain, or any error counts as possibly writing.
/// An attribute name only needs no volatile function of its name. Implicit casts and the
/// operators a clause implies are found without a name, so any volatile implicit or
/// assignment cast, operator-family operator or support function in the database counts.
/// So does any domain CHECK whose stored tree has a number that is the oid of a volatile
/// function or of an operator over one, since coercion into a domain is unnamed too.
const READ_CHECK_SQL: &str = "
SELECT NOT EXISTS (
    SELECT FROM unnest($1::text[], $2::text[]) AS f(s, n),
      LATERAL (SELECT array_agg(oid) AS nss FROM pg_catalog.pg_namespace WHERE nspname = ANY (
        CASE WHEN f.s = '' THEN pg_catalog.current_schemas(true) ELSE ARRAY[f.s]::name[] END)) ns
    WHERE NOT EXISTS (SELECT FROM pg_catalog.pg_proc p
                      WHERE p.proname = f.n AND p.pronamespace = ANY (ns.nss))
       OR EXISTS (SELECT FROM pg_catalog.pg_proc p
                  WHERE p.proname = f.n AND p.pronamespace = ANY (ns.nss)
                    AND (p.provolatile = 'v' OR p.prokind = 'a' AND EXISTS (
                      SELECT FROM pg_catalog.pg_aggregate a JOIN pg_catalog.pg_proc q
                        ON q.oid = ANY (ARRAY[a.aggtransfn, a.aggfinalfn, a.aggcombinefn,
                          a.aggserialfn, a.aggdeserialfn, a.aggmtransfn, a.aggminvtransfn,
                          a.aggmfinalfn]::oid[])
                      WHERE a.aggfnoid = p.oid AND q.provolatile = 'v'))))
AND NOT EXISTS (
    SELECT FROM unnest($3::text[], $4::text[]) AS o(s, n),
      LATERAL (SELECT array_agg(oid) AS nss FROM pg_catalog.pg_namespace WHERE nspname = ANY (
        CASE WHEN o.s = '' THEN pg_catalog.current_schemas(true) ELSE ARRAY[o.s]::name[] END)) ns
    WHERE NOT EXISTS (SELECT FROM pg_catalog.pg_operator x
                      WHERE x.oprname = o.n AND x.oprnamespace = ANY (ns.nss))
       OR EXISTS (SELECT FROM pg_catalog.pg_operator x JOIN pg_catalog.pg_proc q ON q.oid = x.oprcode
                  WHERE x.oprname = o.n AND x.oprnamespace = ANY (ns.nss) AND q.provolatile = 'v'))
AND NOT EXISTS (
    SELECT FROM unnest($5::text[], $6::text[]) AS r(s, n),
      LATERAL (SELECT pg_catalog.to_regclass(CASE WHEN r.s = '' THEN format('%I', r.n)
                                                  ELSE format('%I.%I', r.s, r.n) END) AS rel) y
    WHERE NOT EXISTS (SELECT FROM pg_catalog.pg_class c WHERE c.oid = y.rel
                        AND c.relkind IN ('r', 'p', 'm') AND NOT c.relrowsecurity))
AND NOT EXISTS (
    SELECT FROM unnest($7::text[], $8::text[]) AS t(s, n),
      LATERAL (SELECT pg_catalog.to_regtype(CASE WHEN t.s = '' THEN format('%I', t.n)
                                                 ELSE format('%I.%I', t.s, t.n) END) AS typ) y
    WHERE NOT EXISTS (SELECT FROM pg_catalog.pg_type ty WHERE ty.oid = y.typ AND ty.typtype <> 'd')
       OR EXISTS (SELECT FROM pg_catalog.pg_cast c JOIN pg_catalog.pg_proc q ON q.oid = c.castfunc
                  WHERE c.casttarget = y.typ AND q.provolatile = 'v'))
AND NOT EXISTS (
    SELECT FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
    WHERE p.proname = ANY ($9::text[]) AND n.nspname = ANY (pg_catalog.current_schemas(true))
      AND p.provolatile = 'v')
AND NOT EXISTS (SELECT FROM pg_catalog.pg_cast c JOIN pg_catalog.pg_proc q ON q.oid = c.castfunc
                WHERE c.castcontext IN ('i', 'a') AND q.provolatile = 'v')
AND NOT EXISTS (SELECT FROM pg_catalog.pg_amop a
                  JOIN pg_catalog.pg_operator x ON x.oid = a.amopopr
                  JOIN pg_catalog.pg_proc q ON q.oid = x.oprcode
                WHERE q.provolatile = 'v')
AND NOT EXISTS (SELECT FROM pg_catalog.pg_amproc a JOIN pg_catalog.pg_proc q ON q.oid = a.amproc
                WHERE q.provolatile = 'v')
AND NOT EXISTS (
    SELECT FROM pg_catalog.pg_constraint c,
         pg_catalog.regexp_matches(c.conbin::text, '\\d+', 'g') AS m(t),
         LATERAL (SELECT CASE WHEN length(m.t[1]) > 10 THEN NULL
                              WHEN m.t[1]::int8 <= 4294967295 THEN m.t[1]::oid END AS o) y
    WHERE c.contype = 'c' AND c.contypid <> 0
      AND (EXISTS (SELECT FROM pg_catalog.pg_proc q WHERE q.oid = y.o AND q.provolatile = 'v')
        OR EXISTS (SELECT FROM pg_catalog.pg_operator x JOIN pg_catalog.pg_proc q ON q.oid = x.oprcode
                   WHERE x.oid = y.o AND q.provolatile = 'v')))";

/// Whether aborting a truncated read loses nothing. Inside a transaction block close never
/// commits, so it is abortable and the check, whose failure would abort the user's
/// transaction, is never sent; elsewhere `nothing_volatile` decides.
async fn cannot_write(client: &Client, check: &ReadCheckState, reach: Option<PgReach>) -> bool {
    if check.maybe_in_tx.load(Ordering::SeqCst) {
        match in_transaction_block(client).await {
            Some(true) => return true,
            Some(false) => check.maybe_in_tx.store(false, Ordering::SeqCst),
            None => return false,
        }
    }
    nothing_volatile(client, check, reach).await
}

/// Whether a transaction block is open: within one, now() is the block's start, not this
/// statement's. `None` when the probe fails, as it does in an aborted block.
async fn in_transaction_block(client: &Client) -> Option<bool> {
    let probe = "SELECT pg_catalog.now() = pg_catalog.statement_timestamp()";
    let messages = client.simple_query(probe).await.ok()?;
    messages.iter().find_map(|m| match m {
        SimpleQueryMessage::Row(row) => row.get(0).map(|same| same == "f"),
        SimpleQueryMessage::CommandComplete(_) | SimpleQueryMessage::RowDescription(_) | _ => None,
    })
}

/// Runs `READ_CHECK_SQL` for `reach`, preparing it on first use; anything but a proof of no
/// volatile object, including a statement the parser could not walk, is false.
async fn nothing_volatile(client: &Client, check: &ReadCheckState, reach: Option<PgReach>) -> bool {
    // Even a read that names nothing can reach an implicit cast or implied operator, so
    // every read is checked.
    let Some(reach) = reach else { return false };
    let cache = &check.statement;
    let cached = cache.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let statement = match cached {
        Some(statement) => statement,
        None => match client
            .prepare_typed(READ_CHECK_SQL, &[Type::TEXT_ARRAY; 9])
            .await
        {
            Ok(statement) => {
                let mut slot = cache.lock().unwrap_or_else(PoisonError::into_inner);
                slot.get_or_insert(statement).clone()
            }
            Err(_) => return false,
        },
    };
    let split =
        |pairs: Vec<(String, String)>| -> (Vec<String>, Vec<String>) { pairs.into_iter().unzip() };
    let (fs, fns) = split(reach.funcs);
    let (os, ons) = split(reach.ops);
    let (rs, rns) = split(reach.rels);
    let (ts, tns) = split(reach.types);
    let attrs = reach.attrs;
    let params: [&(dyn ToSql + Sync); 9] = [&fs, &fns, &os, &ons, &rs, &rns, &ts, &tns, &attrs];
    match client.query(&statement, &params).await {
        Ok(rows) => rows.first().and_then(|r| r.try_get::<_, bool>(0).ok()) == Some(true),
        Err(_) => false,
    }
}

/// Takes the write drain error no caller has reported yet.
fn take_unreported(owed: &SharedOwed) -> Option<ExecuteError> {
    owed.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .unreported
        .take()
}

pub(crate) struct PgDriver {
    client: Arc<Client>,
    connection: JoinHandle<()>,
    /// Errs on `changed` once the connection task has ended.
    ended: watch::Receiver<()>,
    cancel: Arc<PgCancel>,
    owed: SharedOwed,
    read_only: bool,
    /// Set once a SET or RESET is sent on this session; see `Session::settings_changed`.
    settings_changed: Arc<AtomicBool>,
    read_check: ReadCheck,
    /// Stops unread rows the session still owes when it is dropped without `close`.
    orphans: OrphanGuard,
}

/// What a session dropped without `close` still owes, which tokio-postgres would otherwise
/// read to its end with no cap. A read that cannot write is aborted at once; anything else
/// drains under `DRAIN_CAP` on the session's runtime and is stopped past it.
struct OrphanGuard {
    owed: SharedOwed,
    cancel: Arc<PgCancel>,
    abort: AbortHandle,
}

impl Drop for OrphanGuard {
    fn drop(&mut self) {
        let (leftover, abortable) = {
            let mut o = self.owed.lock().unwrap_or_else(PoisonError::into_inner);
            (o.leftover.take(), o.abortable)
        };
        let Some(mut rows) = leftover else { return };
        if abortable {
            self.abort.abort();
            return;
        }
        let (cancel, abort) = (Arc::clone(&self.cancel), self.abort.clone());
        self.cancel.runtime.spawn(async move {
            let drain = async { while rows.next().await.is_some() {} };
            if tokio::time::timeout(DRAIN_CAP, drain).await.is_err() {
                stop_at_cap(&cancel, &abort).await;
            }
        });
    }
}

/// One per session, shared with its scripts.
type ReadCheck = Arc<ReadCheckState>;

#[derive(Default)]
struct ReadCheckState {
    /// `READ_CHECK_SQL`, prepared on first use so later reads only bind and execute it.
    statement: Mutex<Option<Statement>>,
    /// A transaction block may be open: only transaction control, which the classifier flags,
    /// or an edit's `run_bound` can open one. Cleared once a probe finds none.
    maybe_in_tx: AtomicBool,
}

/// One script: its text, the pieces not yet prepared, and the statement prepared last.
pub(crate) struct PgScript {
    client: Arc<Client>,
    sql: String,
    pieces: std::vec::IntoIter<Range<usize>>,
    current: Option<(String, Statement)>,
    /// The prepared statement cannot write: it runs inside the read-only wrapper, or the
    /// catalog shows that nothing volatile is reachable from it.
    current_abortable: bool,
    /// The prepared statement is a top-level SET or RESET, which marks the session once sent.
    current_sets: bool,
    settings_changed: Arc<AtomicBool>,
    ended: bool,
    /// Aborts the connection task: the socket drops, so the server stops the query, and
    /// every later request on the shared `Client` fails as closed.
    close: AbortHandle,
    owed: SharedOwed,
    read_only: bool,
    cancel: Arc<PgCancel>,
    read_check: ReadCheck,
}

/// A running statement. Truncating or dropping it leaves the unread rows owed, for the next
/// request to drain under the cap; under `OnTruncate::Close` it closes the session instead.
/// A row cut under `OnTruncate::Park` keeps the statement open until `extend` or drop.
pub(crate) struct PgStream {
    /// `None` once the result has ended or been truncated.
    rows: Option<Pin<Box<SimpleQueryStream>>>,
    /// The row that proved a parked row cut; it is delivered first once the cap is raised.
    peeked: Option<Box<SimpleQueryRow>>,
    builder: BatchBuilder,
    delivered: u64,
    done: Option<Outcome>,
    limits: Limits,
    client: Arc<Client>,
    owed: SharedOwed,
    cancel: Arc<PgCancel>,
    /// Closes the session: under `OnTruncate::Close`, or when the wrapper cannot be ended.
    abort: AbortHandle,
    close_on_truncate: bool,
    abortable: bool,
    /// The statement's text as sent, which an error's position counts into.
    sql: String,
}

impl Drop for PgStream {
    fn drop(&mut self) {
        self.leave_unread();
    }
}

impl Driver for PgDriver {
    type Script = PgScript;
    type Stream = PgStream;

    async fn connect(params: ConnectParams<'_>) -> Result<Self, ConnectError> {
        let conn = params.conn;
        let mut config = pg_config(conn, params.secret, params.timeout);
        // Each connector yields its own Connection type, so each branch spawns its task. The
        // connection future ends once every Client handle is gone. Verify uses Require, so a
        // server without TLS is an error and never a plaintext fallback.
        let target = CancelTarget::of(&config);
        // The connection task holds the only sender, so `closed` learns when it ends.
        let (alive, ended) = watch::channel(());
        // tokio-postgres bounds only the TCP connect; a server that accepts and then stays
        // silent (a paused container) would hang the startup and auth, so bound them all.
        let handshake = async {
            Ok::<_, ConnectError>(if conn.tls() == Some(TlsMode::Verify) {
                let tls = rustls_connector(conn, params.key_passphrase)?;
                config.ssl_mode(SslMode::Require);
                let (client, connection) = config
                    .connect(tls.clone())
                    .await
                    .map_err(|e| connect_error(conn, e))?;
                let task = tokio::spawn(async move {
                    let _alive = alive;
                    let _ = connection.await;
                });
                (client, task, Connector::Rustls(tls))
            } else {
                let (client, connection) = config
                    .connect(NoTls)
                    .await
                    .map_err(|e| connect_error(conn, e))?;
                let task = tokio::spawn(async move {
                    let _alive = alive;
                    let _ = connection.await;
                });
                (client, task, Connector::Plain)
            })
        };
        let (client, connection, connector) = tokio::time::timeout(CONNECT_TIMEOUT, handshake)
            .await
            .map_err(|_| timed_out(conn))??;
        let cancel = Arc::new(PgCancel {
            token: client.cancel_token(),
            connector,
            runtime: Handle::current(),
            requested: AtomicBool::new(false),
            target,
            inflight: Mutex::default(),
        });
        let owed = SharedOwed::default();
        let orphans = OrphanGuard {
            owed: Arc::clone(&owed),
            cancel: Arc::clone(&cancel),
            abort: connection.abort_handle(),
        };
        Ok(PgDriver {
            client: Arc::new(client),
            connection,
            ended,
            cancel,
            owed,
            orphans,
            read_only: params.read_only,
            settings_changed: Arc::default(),
            read_check: ReadCheck::default(),
        })
    }

    fn start_script(&mut self, sql: &str) -> PgScript {
        let pieces: Vec<_> = split(sql)
            .into_iter()
            .filter(|p| !p.blank)
            .map(|p| p.range)
            .collect();
        PgScript {
            client: Arc::clone(&self.client),
            sql: sql.to_owned(),
            pieces: pieces.into_iter(),
            current: None,
            current_abortable: false,
            current_sets: false,
            settings_changed: Arc::clone(&self.settings_changed),
            ended: false,
            close: self.connection.abort_handle(),
            owed: Arc::clone(&self.owed),
            read_only: self.read_only,
            cancel: Arc::clone(&self.cancel),
            read_check: Arc::clone(&self.read_check),
        }
    }

    async fn prepare_next(script: &mut PgScript) -> Option<Result<Prepared, ExecuteError>> {
        script.current = None;
        script.current_sets = false;
        // Unread rows that cannot write are drained only for a next statement: at the end of
        // the script they stay owed for `close` to abort or a later script to settle, unless a
        // wrapper is open. Anything else may write, so it drains here, since an abort could roll it back.
        let last = script.ended || script.pieces.as_slice().is_empty();
        let (abortable_leftover, wrapper) = {
            let o = script.owed.lock().unwrap_or_else(PoisonError::into_inner);
            (
                o.leftover.is_some() && o.abortable,
                o.block == Block::ReadOnly,
            )
        };
        if last && abortable_leftover {
            script.ended = true;
            // An open wrapper would hold its snapshot and locks until the next request, so its
            // read is cancelled, which loses nothing, and the wrapper ended now.
            if wrapper {
                script.cancel.cancel();
                if let Err(e) =
                    settle_quietly(&script.client, &script.owed, &script.close, &script.cancel)
                        .await
                {
                    return Some(Err(e));
                }
            }
            return None;
        }
        // A skipped, truncated or dropped statement's wrapper is ended before anything else.
        if let Err(e) =
            settle_quietly(&script.client, &script.owed, &script.close, &script.cancel).await
        {
            script.ended = true;
            return Some(Err(e));
        }
        if script.ended {
            return None;
        }
        let range = script.pieces.next()?;
        let sql = script.sql[range].trim().to_owned();
        let class = pg_classification(&sql);
        // A parser refusal outranks transaction control; either one ends the script.
        let refusal = match class.refusal {
            Some(reason) => Some(ExecuteError::Unsupported {
                reason: reason.to_owned(),
            }),
            None if script.read_only && class.classification.transaction_control => {
                Some(ExecuteError::TransactionControl {
                    reason: "transaction control is not available in a read-only session; \
                             each statement runs in its own read-only transaction"
                        .to_owned(),
                })
            }
            None => None,
        };
        if let Some(e) = refusal {
            script.ended = true;
            return Some(Err(e));
        }
        let classification = class.classification;
        if classification.transaction_control {
            script.read_check.maybe_in_tx.store(true, Ordering::SeqCst);
        }
        script.current_sets = class.sets_session;
        script.current_abortable = script.read_only;
        // A read in a read-write session can still write through a volatile function, so its
        // reach is checked against the catalog alongside the prepare, pipelined.
        let reach =
            (!script.read_only && classification.writes == Writes::No).then(|| pg_reach(&sql));
        // The wrapper travels alone, before the user's text: its SELECT takes the snapshot,
        // so the statement cannot make the transaction read-write. It is owed from before
        // it is sent, so any failure after this point ends it.
        if script.read_only {
            script
                .owed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .block = Block::ReadOnly;
            if let Err(e) = script
                .client
                .batch_execute("BEGIN READ ONLY; SELECT 1")
                .await
            {
                return Some(Err(fail_script(script, e, None).await));
            }
        }
        // Prepare runs nothing and rejects multi-statement text, so a failure ends the script
        // before any of the statement's effects.
        let (prepared, abortable) = match reach {
            Some(reach) => {
                futures_util::future::join(
                    script.client.prepare(&sql),
                    cannot_write(&script.client, &script.read_check, reach),
                )
                .await
            }
            None => (script.client.prepare(&sql).await, false),
        };
        script.current_abortable |= abortable;
        match prepared {
            Ok(statement) => {
                script.current = Some((sql.clone(), statement));
                Some(Ok(Prepared {
                    sql,
                    classification,
                }))
            }
            Err(e) => Some(Err(fail_script(script, e, Some(&sql)).await)),
        }
    }

    async fn is_last(script: &mut PgScript) -> bool {
        script.ended || script.pieces.as_slice().is_empty()
    }

    async fn execute(
        script: &mut PgScript,
        limits: Limits,
    ) -> Result<(Vec<ColumnInfo>, PgStream), ExecuteError> {
        let (sql, statement) = script.current.take().ok_or(ExecuteError::SessionClosed)?;
        // Earlier cancel requests land before this statement is sent. One still flagged here
        // came after `prepare_next`'s settle, so it was aimed at this statement: it never runs.
        script.cancel.spend().await;
        if script.cancel.requested.swap(false, Ordering::SeqCst) {
            let _ = settle(&script.client, &script.owed, &script.close, &script.cancel).await;
            return Err(ExecuteError::Cancelled);
        }
        // Marked before sending: a SET that then fails may still have changed settings.
        if script.current_sets {
            script.settings_changed.store(true, Ordering::SeqCst);
        }
        let mut rows = match script.client.simple_query_raw(&sql).await {
            Ok(rows) => Box::pin(rows),
            Err(e) => {
                let err = execute_error(e, &script.cancel, Some(&sql));
                let _ = settle(&script.client, &script.owed, &script.close, &script.cancel).await;
                return Err(err);
            }
        };
        let mut stream = PgStream {
            rows: None,
            peeked: None,
            builder: BatchBuilder::new(0),
            delivered: 0,
            done: None,
            limits,
            client: Arc::clone(&script.client),
            owed: Arc::clone(&script.owed),
            cancel: Arc::clone(&script.cancel),
            abort: script.close.clone(),
            close_on_truncate: limits.on_truncate == OnTruncate::Close,
            abortable: script.current_abortable,
            sql,
        };
        // The first message tells a row statement from one without rows, and carries a
        // failure that comes before any row.
        loop {
            match rows.next().await {
                Some(Ok(SimpleQueryMessage::RowDescription(described))) => {
                    let names: Vec<&str> = described.iter().map(|d| d.name()).collect();
                    let types: Vec<Type> = statement
                        .columns()
                        .iter()
                        .map(|c| c.type_().clone())
                        .collect();
                    let mut columns = column_infos(&names, &types);
                    if columns.len() == statement.columns().len() {
                        columns = columns
                            .into_iter()
                            .zip(statement.columns())
                            .map(|(info, c)| info.with_source(pg_source(c)))
                            .collect();
                    }
                    stream.builder = BatchBuilder::new(described.len());
                    stream.rows = Some(rows);
                    return Ok((columns, stream));
                }
                // A result without rows ends here, so its wrapper ends before it is reported.
                // The query stream drops first: tokio-postgres discards its last message.
                Some(Ok(SimpleQueryMessage::CommandComplete(n))) => {
                    drop(rows);
                    stream.settle().await?;
                    // A tag without a count reads as 0, so only DML-shaped text keeps a 0.
                    let counted = n > 0 || crate::classify::counts_rows(&stream.sql);
                    stream.done = Some(outcome(0, None, counted.then_some(n), 0));
                    return Ok((Vec::new(), stream));
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    drop(rows);
                    return Err(stream.fail(e).await);
                }
                None => {
                    stream.settle().await?;
                    stream.done = Some(outcome(0, None, None, 0));
                    return Ok((Vec::new(), stream));
                }
            }
        }
    }

    async fn next_event(stream: &mut PgStream) -> Result<StreamItem, ExecuteError> {
        // Hand over what has arrived: once the batch holds a row, a message that is not
        // ready yet closes it, so a slow result still streams.
        while stream.done.is_none() {
            let Some(rows) = stream.rows.as_mut() else {
                break;
            };
            let next = if let Some(row) = stream.peeked.take() {
                Some(Ok(SimpleQueryMessage::Row(*row)))
            } else if stream.builder.is_empty() {
                rows.next().await
            } else {
                match rows.next().now_or_never() {
                    Some(next) => next,
                    None => return Ok(StreamItem::Batch(stream.builder.finish())),
                }
            };
            match next {
                Some(Ok(SimpleQueryMessage::Row(row))) => {
                    // A row past a reached bound proves the result was cut short; the rest
                    // is owed to the next request or the session closes, unless Park holds it.
                    let cut = stream
                        .limits
                        .cut(stream.delivered, stream.builder.total_bytes());
                    // Under Park a row cut keeps that row and the open statement for `extend`.
                    if cut == Some(TruncatedBy::Rows)
                        && stream.limits.on_truncate == OnTruncate::Park
                    {
                        stream.peeked = Some(Box::new(row));
                        stream.done = Some(outcome(stream.delivered, cut, None, 0));
                        break;
                    }
                    if cut.is_some() {
                        stream.leave_unread();
                        stream.done = Some(outcome(stream.delivered, cut, None, 0));
                        break;
                    }
                    // `get` panics on text it cannot decode; `try_get` reports it. The rows
                    // stay set, so under Close the session closes: the result was cut short.
                    let cells = (0..row.len())
                        .map(|i| {
                            row.try_get(i)
                                .map(|v| v.map_or(CellRef::Null, CellRef::Text))
                        })
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|_| not_utf8())?;
                    stream.delivered += 1;
                    if let Some(full) = stream.builder.push_row(&cells) {
                        return Ok(StreamItem::Batch(full));
                    }
                    if stream.builder.is_full() {
                        return Ok(StreamItem::Batch(stream.builder.finish()));
                    }
                }
                Some(Ok(SimpleQueryMessage::CommandComplete(_))) | None => {
                    stream.rows = None;
                    stream.settle().await?;
                    stream.done = Some(outcome(stream.delivered, None, None, 0));
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    stream.rows = None;
                    return Err(stream.fail(e).await);
                }
            }
        }
        if !stream.builder.is_empty() {
            return Ok(StreamItem::Batch(stream.builder.finish()));
        }
        Ok(StreamItem::Done(stream.done.take().unwrap_or_else(|| {
            outcome(stream.delivered, None, None, 0)
        })))
    }

    fn extend(stream: &mut PgStream, more: Option<u64>) -> Result<(), ExtendError> {
        if stream.peeked.is_none() {
            return Err(ExtendError::NotParked);
        }
        stream.limits.max_rows =
            more.and_then(|m| stream.limits.max_rows.map(|r| r.saturating_add(m)));
        Ok(())
    }

    fn cancel_handle(&self) -> CancelHandle {
        CancelHandle::postgres(Arc::clone(&self.cancel))
    }

    async fn settle(&mut self) -> Result<(), ExecuteError> {
        self.settle_session().await?;
        take_unreported(&self.owed).map_or(Ok(()), Err)
    }

    async fn close(self) -> Result<(), ExecuteError> {
        let PgDriver {
            client,
            connection,
            owed,
            cancel,
            // Held to the end, so it finds nothing owed once the leftover is taken below.
            orphans: _orphans,
            ..
        } = self;
        // Unread rows that cannot write would keep the connection busy until the whole result
        // arrived, so they are aborted. Any other statement may write, so it is drained under
        // the cap and its failed commit is reported; past the cap it is reported stopped.
        let (leftover, abortable, mut failure) = {
            let mut o = owed.lock().unwrap_or_else(PoisonError::into_inner);
            (o.leftover.take(), o.abortable, o.unreported.take())
        };
        if let Some(mut rows) = leftover {
            let drained = if abortable {
                None
            } else {
                tokio::time::timeout(DRAIN_CAP, async {
                    loop {
                        match rows.next().await {
                            Some(Ok(_)) => {}
                            Some(Err(e)) => break Some(e),
                            None => break None,
                        }
                    }
                })
                .await
                .ok()
            };
            match drained {
                Some(error) => {
                    if let Some(e) = error {
                        failure.get_or_insert(execute_error(e, &cancel, None));
                    }
                }
                None if abortable => connection.abort(),
                None => {
                    stop_at_cap(&cancel, &connection.abort_handle()).await;
                    failure.get_or_insert(ExecuteError::StoppedAtDrainCap);
                }
            }
        }
        drop(client);
        let _ = connection.await;
        failure.map_or(Ok(()), Err)
    }

    async fn schemas(&mut self) -> Result<Vec<SchemaInfo>, IntrospectError> {
        let rows = self.catalog(SCHEMAS_SQL, &[]).await?;
        rows.iter()
            .map(|r| Ok(SchemaInfo::new(cell::<String>(r, 0)?)))
            .collect()
    }

    async fn relations(&mut self, schema: &str) -> Result<RelationList, IntrospectError> {
        let limit = i64::try_from(RELATION_CAP + 1).unwrap_or(i64::MAX);
        let rows = self.catalog(RELATIONS_SQL, &[&schema, &limit]).await?;
        let items = rows
            .iter()
            .map(|r| {
                let kind = match cell::<String>(r, 1)?.as_str() {
                    "r" | "p" => RelationKind::Table,
                    "v" => RelationKind::View,
                    "m" => RelationKind::MaterializedView,
                    "f" => RelationKind::ForeignTable,
                    _ => RelationKind::Other,
                };
                Ok(Relation::new(cell::<String>(r, 0)?, kind))
            })
            .collect::<Result<Vec<_>, IntrospectError>>()?;
        Ok(capped(items))
    }

    async fn columns(&mut self, rel: &RelationRef) -> Result<Vec<ColumnMeta>, IntrospectError> {
        let rows = self
            .catalog(COLUMNS_SQL, &[&rel.schema(), &rel.name()])
            .await?;
        rows.iter()
            .map(|r| {
                let pk = cell::<Option<i32>>(r, 3)?
                    .map(|p| {
                        u16::try_from(p).map_err(|_| IntrospectError::Unexpected {
                            message: format!("primary key position {p}"),
                        })
                    })
                    .transpose()?;
                Ok(ColumnMeta::new(
                    cell::<String>(r, 0)?,
                    cell::<String>(r, 1)?,
                    cell::<bool>(r, 2)?,
                    pk,
                    cell::<Option<String>>(r, 4)?,
                ))
            })
            .collect()
    }
}

/// The search path's first schema, and the one its value at session start resolves to: the
/// first entry, `$user` read as the role, that names an existing schema.
const CURRENT_SCHEMA_SQL: &str = "\
SELECT current_schema()::text, (
    SELECT n.nspname::text
    FROM unnest(string_to_array(s.reset_val, ',')) WITH ORDINALITY AS p(entry, ord)
    JOIN pg_catalog.pg_namespace n ON n.nspname = CASE
        WHEN btrim(p.entry) IN ('$user', '\"$user\"') THEN current_user::text
        ELSE btrim(btrim(p.entry), '\"') END
    ORDER BY p.ord LIMIT 1)
FROM pg_catalog.pg_settings s WHERE s.name = 'search_path'";

const SCHEMAS_SQL: &str = "SELECT nspname::text FROM pg_catalog.pg_namespace \
WHERE nspname !~ '^pg_' AND nspname <> 'information_schema' ORDER BY nspname";

/// The databases a session could open: no templates and none that refuse connections, with
/// the server's own `postgres` after the rest.
const DATABASES_SQL: &str = "SELECT datname::text FROM pg_catalog.pg_database \
WHERE NOT datistemplate AND datallowconn ORDER BY datname = 'postgres', datname";

/// `$1` is the schema, `$2` is `RELATION_CAP + 1`.
const RELATIONS_SQL: &str = "SELECT c.relname::text, c.relkind::text \
FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
WHERE n.nspname = $1::text AND c.relkind IN ('r', 'p', 'v', 'm', 'f') \
ORDER BY c.relname LIMIT $2";

/// `$1` is the schema, `$2` the relation. `indkey` is an int2vector with lower bound 0, so
/// the key position comes from ORDINALITY; INCLUDE columns past `indnkeyatts` are not keys.
const COLUMNS_SQL: &str = "\
SELECT a.attname::text, pg_catalog.format_type(a.atttypid, a.atttypmod), NOT a.attnotnull,
       pk.pos::int4, pg_catalog.pg_get_expr(d.adbin, d.adrelid)
FROM pg_catalog.pg_attribute a
JOIN pg_catalog.pg_class c ON c.oid = a.attrelid
JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
LEFT JOIN LATERAL (
    SELECT k.ord AS pos
    FROM pg_catalog.pg_index i, unnest(i.indkey::int2[]) WITH ORDINALITY AS k(attnum, ord)
    WHERE i.indrelid = c.oid AND i.indisprimary AND k.attnum = a.attnum
      AND k.ord <= i.indnkeyatts
) pk ON true
WHERE n.nspname = $1::text AND c.relname = $2::text AND a.attnum > 0 AND NOT a.attisdropped
ORDER BY a.attnum";

impl PgDriver {
    /// Settles what earlier statements owe, leaving a write's drain error unreported for
    /// `Session::settle` or `close`.
    async fn settle_session(&self) -> Result<(), ExecuteError> {
        let abort = self.connection.abort_handle();
        let result = settle(&self.client, &self.owed, &abort, &self.cancel).await;
        // A connection that ended on its own (a terminated backend) owes nothing, so only the
        // client's own state can tell the caller the session is gone; a capped drain says why.
        if self.client.is_closed() && !matches!(result, Err(ExecuteError::StoppedAtDrainCap)) {
            return Err(ExecuteError::SessionClosed);
        }
        result
    }

    /// Resolves once the connection task has ended: the socket closed or failed.
    pub(crate) async fn closed(&self) {
        let mut ended = self.ended.clone();
        while ended.changed().await.is_ok() {}
    }

    pub(crate) fn settings_changed(&self) -> bool {
        self.settings_changed.load(Ordering::SeqCst)
    }

    /// The current schema once a search path change moved it off the session's first one;
    /// None on failure, since the database itself never changes on Postgres.
    pub(crate) async fn moved_schema(&mut self) -> Option<String> {
        let rows = self.catalog(CURRENT_SCHEMA_SQL, &[]).await.ok()?;
        let row = rows.first()?;
        let now = cell::<Option<String>>(row, 0).ok()??;
        let opened = cell::<Option<String>>(row, 1).ok()?;
        (Some(&now) != opened.as_ref()).then_some(now)
    }

    /// The server's databases this login could open, `postgres` last; see DATABASES_SQL.
    pub(crate) async fn databases(&mut self) -> Result<Vec<String>, IntrospectError> {
        let rows = self.catalog(DATABASES_SQL, &[]).await?;
        rows.iter().map(|r| cell::<String>(r, 0)).collect()
    }

    /// Runs one fixed catalog query after settling what earlier statements owe. A read-only
    /// session wraps it in `BEGIN READ ONLY` and ends the wrapper at once; a read-write one
    /// runs it bare, so a user's open transaction is neither committed nor rolled back.
    pub(crate) async fn catalog(
        &mut self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>, IntrospectError> {
        // An earlier statement's drain error is that statement's; only a lost session counts.
        if let Err(e) = self.settle_session().await
            && e.closes_session()
        {
            return Err(ExecuteError::SessionClosed.into());
        }
        let result = async {
            if self.read_only {
                self.owed
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .block = Block::ReadOnly;
                self.client.batch_execute("BEGIN READ ONLY").await?;
            }
            self.client.query(sql, params).await
        }
        .await
        .map_err(|e| self.catalog_error(e));
        // The settle ends the wrapper, also after a failed BEGIN; a lost session outranks
        // whatever the query itself reported.
        if let Err(e) = self.settle_session().await
            && e.closes_session()
        {
            return Err(ExecuteError::SessionClosed.into());
        }
        Ok(result?)
    }

    /// `catalog` with `search_path` pinned to `pg_catalog` for its one query, so every name the
    /// `pg_get_*` functions print is schema-qualified. The pin is transaction-local and ends with
    /// the query: in the read-only wrapper, in a block of its own, or under a savepoint that is
    /// rolled back inside the user's open block, so the user's settings and work survive.
    pub(crate) async fn catalog_pinned(
        &mut self,
        sql: &str,
        params: &[&(dyn ToSql + Sync)],
    ) -> Result<Vec<Row>, IntrospectError> {
        if let Err(e) = self.settle_session().await
            && e.closes_session()
        {
            return Err(ExecuteError::SessionClosed.into());
        }
        // Set when a block this lookup opened may still be open on a live connection.
        let mut unsettled = false;
        let result = async {
            if self.read_only {
                self.owed
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .block = Block::ReadOnly;
                self.client
                    .batch_execute("BEGIN READ ONLY; SET LOCAL search_path = pg_catalog")
                    .await?;
                return self.client.query(sql, params).await;
            }
            // An aborted block (probe None) counts as open: its SAVEPOINT fails and reports it.
            let in_block = self.read_check.maybe_in_tx.load(Ordering::SeqCst)
                && match in_transaction_block(&self.client).await {
                    Some(false) => {
                        self.read_check.maybe_in_tx.store(false, Ordering::SeqCst);
                        false
                    }
                    _ => true,
                };
            let (open, close) = if in_block {
                (
                    "SAVEPOINT dbento_catalog; SET LOCAL search_path = pg_catalog",
                    "ROLLBACK TO SAVEPOINT dbento_catalog; RELEASE SAVEPOINT dbento_catalog",
                )
            } else {
                (
                    "BEGIN READ ONLY; SET LOCAL search_path = pg_catalog",
                    "ROLLBACK",
                )
            };
            // A failed open may have got as far as its BEGIN or SAVEPOINT, so the close always runs.
            let opened = self.client.batch_execute(open).await;
            let rows = match &opened {
                Ok(()) => self.client.query(sql, params).await,
                Err(_) => Ok(Vec::new()),
            };
            let closed = self.client.batch_execute(close).await;
            // In the user's block a failed SAVEPOINT leaves nothing to close, so that close may
            // fail harmlessly; any other failed close leaves a block nobody tracks.
            unsettled =
                closed.is_err() && !(in_block && opened.is_err()) && !self.client.is_closed();
            opened?;
            let rows = rows?;
            closed?;
            Ok(rows)
        }
        .await
        .map_err(|e| self.catalog_error(e));
        if unsettled {
            // As in `settle_owed`: the transaction state is unknown, so the session must end.
            self.connection.abort_handle().abort();
            return Err(ExecuteError::SessionClosed.into());
        }
        if let Err(e) = self.settle_session().await
            && e.closes_session()
        {
            return Err(ExecuteError::SessionClosed.into());
        }
        Ok(result?)
    }

    /// Runs an EXPLAIN text in a block that is always rolled back and returns its one cell:
    /// the read-only wrapper, a `BEGIN` of its own, or a savepoint inside the user's block.
    /// The close is owed from before the open is sent, so a drop or cancel cannot skip it.
    pub(crate) async fn explain(&mut self, sql: &str) -> Result<String, ExecuteError> {
        if let Err(e) = self.settle_session().await
            && e.closes_session()
        {
            return Err(ExecuteError::SessionClosed);
        }
        self.cancel.requested.store(false, Ordering::SeqCst);
        // An aborted block (probe None) counts as open: its SAVEPOINT fails and reports it.
        let block = if self.read_only {
            Block::ReadOnly
        } else if self.read_check.maybe_in_tx.load(Ordering::SeqCst)
            && match in_transaction_block(&self.client).await {
                Some(false) => {
                    self.read_check.maybe_in_tx.store(false, Ordering::SeqCst);
                    false
                }
                _ => true,
            }
        {
            Block::Savepoint
        } else {
            Block::Begin
        };
        self.owed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .block = block;
        let result = match self.client.batch_execute(block.open()).await {
            Err(e) => {
                let err = execute_error(e, &self.cancel, None);
                if block == Block::Savepoint {
                    // The savepoint may not exist, so its close is sent here and may fail
                    // harmlessly; it stays owed until that close is sent.
                    self.cancel.spend().await;
                    self.owed
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .block = Block::None;
                    let _ = self
                        .client
                        .batch_execute(Block::Savepoint.close().unwrap_or_default())
                        .await;
                }
                Err(err)
            }
            // A cancel that came during the open was aimed at this explain: the request never runs.
            Ok(()) if self.cancel.requested.load(Ordering::SeqCst) => Err(ExecuteError::Cancelled),
            Ok(()) => {
                self.owed
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .request_running = true;
                let sent = self.client.simple_query(sql).await;
                self.owed
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .request_running = false;
                sent.map_err(|e| execute_error(e, &self.cancel, Some(sql)))
                    .and_then(|messages| first_cell(&messages))
            }
        };
        // The settle spends any cancel still in flight, then sends the close.
        if let Err(e) = self.settle_session().await
            && e.closes_session()
        {
            return Err(ExecuteError::SessionClosed);
        }
        result
    }

    /// Runs one statement with every parameter sent as TEXT and returns the rows it changed.
    pub(crate) async fn run_bound(&self, sql: &str, params: &[Param]) -> Result<u64, ExecuteError> {
        // An edit's BEGIN bypasses the classifier, so any bound statement may open a block.
        self.read_check.maybe_in_tx.store(true, Ordering::SeqCst);
        // Postgres reads bytes as `\x` hex text, the same text the generator writes.
        let texts = bound_texts(params);
        let types = vec![Type::TEXT; texts.len()];
        let result = async {
            let statement = self.client.prepare_typed(sql, &types).await?;
            let refs: Vec<&(dyn ToSql + Sync)> =
                texts.iter().map(|t| t as &(dyn ToSql + Sync)).collect();
            self.client.execute(&statement, &refs).await
        }
        .await;
        result.map_err(|e| self.catalog_error(e))
    }

    /// Runs a bound statement whose every result column is text, and returns its rows.
    pub(crate) async fn query_bound(
        &self,
        sql: &str,
        params: &[Param],
    ) -> Result<Vec<Vec<Option<String>>>, ExecuteError> {
        let texts = bound_texts(params);
        let types = vec![Type::TEXT; texts.len()];
        let result: Result<Vec<Vec<Option<String>>>, tokio_postgres::Error> = async {
            let statement = self.client.prepare_typed(sql, &types).await?;
            let refs: Vec<&(dyn ToSql + Sync)> =
                texts.iter().map(|t| t as &(dyn ToSql + Sync)).collect();
            self.client
                .query(&statement, &refs)
                .await?
                .iter()
                .map(|row| (0..row.len()).map(|i| row.try_get(i)).collect())
                .collect()
        }
        .await;
        result.map_err(|e| self.catalog_error(e))
    }

    /// Maps a catalog query's error: a closed client or a server that ended the session
    /// (57P01 to 57P03) reads as `SessionClosed`, since a FATAL arrives as a database error.
    fn catalog_error(&self, e: tokio_postgres::Error) -> ExecuteError {
        let ended = e
            .as_db_error()
            .is_some_and(|db| matches!(db.code().code(), "57P01" | "57P02" | "57P03"));
        if self.client.is_closed() || ended {
            ExecuteError::SessionClosed
        } else {
            execute_error(e, &self.cancel, None)
        }
    }
}

/// Bound parameters as the TEXT values Postgres receives. `bound` already sends bytes to
/// Postgres as `\x` hex text, so a `Param::Bytes` never reaches this driver.
fn bound_texts(params: &[Param]) -> Vec<&str> {
    params
        .iter()
        .map(|p| match p {
            Param::Text(t) => t.as_str(),
            Param::Bytes(_) => unreachable!("bound() hex-encodes bytes for Postgres"),
        })
        .collect()
}

/// Decodes one catalog cell; a type the query did not promise is `Unexpected`.
pub(crate) fn cell<'a, T: FromSql<'a>>(row: &'a Row, ix: usize) -> Result<T, IntrospectError> {
    row.try_get(ix).map_err(|e| IntrospectError::Unexpected {
        message: e.to_string(),
    })
}

impl PgStream {
    /// Hands an unfinished result's rows to the session as owed, or closes the session
    /// under `OnTruncate::Close`.
    fn leave_unread(&mut self) {
        let Some(rows) = self.rows.take() else {
            return;
        };
        if self.close_on_truncate {
            self.abort.abort();
        } else {
            let mut o = self.owed.lock().unwrap_or_else(PoisonError::into_inner);
            o.leftover = Some(rows);
            o.abortable = self.abortable;
        }
    }

    async fn settle(&self) -> Result<(), ExecuteError> {
        settle_quietly(&self.client, &self.owed, &self.abort, &self.cancel).await
    }

    /// The statement's error, after its wrapper has ended; a failed ending has already
    /// closed the session, so the next request reports that.
    async fn fail(&self, e: tokio_postgres::Error) -> ExecuteError {
        let err = execute_error(e, &self.cancel, Some(&self.sql));
        let _ = self.settle().await;
        err
    }
}

/// The first row's first cell, as an EXPLAIN returns its whole plan.
fn first_cell(messages: &[SimpleQueryMessage]) -> Result<String, ExecuteError> {
    let cell = messages.iter().find_map(|m| match m {
        SimpleQueryMessage::Row(row) => Some(row.try_get(0).map(|c| c.map(str::to_owned))),
        SimpleQueryMessage::CommandComplete(_) | SimpleQueryMessage::RowDescription(_) | _ => None,
    });
    match cell {
        Some(Ok(Some(text))) => Ok(text),
        Some(Err(_)) => Err(not_utf8()),
        _ => Err(ExecuteError::Unsupported {
            reason: "the EXPLAIN returned no plan".to_owned(),
        }),
    }
}

/// Ends the script on a failed wrapper or prepare: the wrapper ends, and the statement's
/// own error is what the caller sees.
async fn fail_script(
    script: &mut PgScript,
    e: tokio_postgres::Error,
    sql: Option<&str>,
) -> ExecuteError {
    script.ended = true;
    let err = execute_error(e, &script.cancel, sql);
    let _ = settle(&script.client, &script.owed, &script.close, &script.cancel).await;
    err
}

fn outcome(
    rows: u64,
    truncated_by: Option<TruncatedBy>,
    rows_affected: Option<u64>,
    extra_results: u32,
) -> Outcome {
    Outcome {
        rows,
        truncated: truncated_by.is_some(),
        rows_affected,
        truncated_by,
        extra_results,
        rolled_back: false,
        consumed_capacity: None,
    }
}

/// Bare for `pg_catalog`, `schema.name` otherwise; an array is its element's name plus `[]`.
fn type_name(t: &Type) -> String {
    match t.kind() {
        Kind::Array(elem) => format!("{}[]", type_name(elem)),
        _ if t.schema() == "pg_catalog" => t.name().to_owned(),
        Kind::Simple
        | Kind::Enum(_)
        | Kind::Pseudo
        | Kind::Range(_)
        | Kind::Multirange(_)
        | Kind::Domain(_)
        | Kind::Composite(_)
        | _ => format!("{}.{}", t.schema(), t.name()),
    }
}

/// Typed columns when the prepared types match the RowDescription in number, else names only
/// (empty type, `None` oid): the result shape changed between prepare and run.
fn column_infos(names: &[&str], types: &[Type]) -> Vec<ColumnInfo> {
    if names.len() == types.len() {
        names
            .iter()
            .zip(types)
            .map(|(n, t)| ColumnInfo::new(*n, type_name(t), Some(t.oid())))
            .collect()
    } else {
        names
            .iter()
            .map(|n| ColumnInfo::new(*n, "", None))
            .collect()
    }
}

/// The table column a prepared column reads; Postgres reports 0 for a computed one.
fn pg_source(c: &Column) -> Option<ColumnSource> {
    match (c.table_oid(), c.column_id()) {
        (Some(table_oid), Some(column_id)) if table_oid != 0 && column_id > 0 => {
            Some(ColumnSource::Postgres {
                table_oid,
                column_id,
            })
        }
        _ => None,
    }
}

/// A cell the session cannot decode as UTF-8, which only a changed client_encoding causes.
fn not_utf8() -> ExecuteError {
    ExecuteError::Unsupported {
        reason: "a value is not valid UTF-8; the session's client_encoding was changed from UTF8"
            .to_owned(),
    }
}

/// A server error keeps its SQLSTATE and message (its `Display` is only "db error");
/// anything else means the connection is gone. 25006 is a read-only violation in every
/// session, so a hot standby reports the same way as the wrapper. 57014 is a cancel when
/// one was requested since the statement started, else the statement timeout. `sql` is the
/// statement as sent, so the server's 1-based character position becomes a byte offset.
fn execute_error(e: tokio_postgres::Error, cancel: &PgCancel, sql: Option<&str>) -> ExecuteError {
    match e.as_db_error() {
        Some(db) if db.code().code() == "57014" => {
            if cancel.requested.load(Ordering::SeqCst) {
                ExecuteError::Cancelled
            } else {
                ExecuteError::Timeout
            }
        }
        Some(db) if db.code().code() == "25006" => ExecuteError::ReadOnlyViolation {
            message: db.message().to_owned(),
        },
        Some(db) => {
            let position = match (db.position(), sql) {
                (Some(ErrorPosition::Original(p)), Some(sql)) => Some(
                    sql.char_indices()
                        .nth((*p as usize).saturating_sub(1))
                        .map_or(sql.len(), |(b, _)| b),
                ),
                _ => None,
            };
            ExecuteError::Database {
                code: db.code().code().to_owned(),
                sqlstate: None,
                class: None,
                state: None,
                message: db.message().to_owned(),
                detail: db.detail().map(str::to_owned),
                hint: db.hint().map(str::to_owned),
                position,
            }
        }
        None => ExecuteError::SessionClosed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_postgres::types::Kind;

    #[test]
    fn setting_values_escape_whitespace_and_backslashes() {
        let mut s = BTreeMap::new();
        s.insert("search_path".to_owned(), "app, public".to_owned());
        s.insert("dbento.tag".to_owned(), "a\\b\tc".to_owned());
        assert_eq!(
            startup_options(&s, Some(Duration::from_millis(1500))),
            "-c client_encoding=UTF8 -c bytea_output=hex -c application_name=dbento -c dbento.tag=a\\\\b\\\tc -c search_path=app,\\ public -c statement_timeout=1500"
        );
    }

    #[test]
    fn a_sub_millisecond_timeout_rounds_up_instead_of_disabling() {
        assert!(
            startup_options(&BTreeMap::new(), Some(Duration::from_micros(10)))
                .ends_with(" -c statement_timeout=1")
        );
        assert!(!startup_options(&BTreeMap::new(), None).contains("statement_timeout"));
    }

    #[cfg(unix)]
    #[test]
    fn a_socket_directory_host_becomes_a_unix_socket() {
        use tokio_postgres::config::Host;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("dbento.toml"), "dbento = 1\n[connections.s]\nengine = \"postgres\"\nhost = \"/run/postgresql\"\nport = 5433\ndatabase = \"d\"\nuser = \"u\"\nsecret = \"none\"\n").unwrap();
        let ws = crate::Workspace::open(dir.path()).unwrap();
        let config = pg_config(ws.connection("s").unwrap(), None, None);
        assert_eq!(config.get_hosts(), &[Host::Unix("/run/postgresql".into())]);
        assert_eq!(config.get_ports(), &[5433]);
        assert_eq!(
            config.get_options(),
            Some(startup_options(&BTreeMap::new(), None).as_str())
        );
    }

    #[test]
    fn type_names_qualify_non_catalog_types_and_mark_arrays() {
        let citext = Type::new("citext".into(), 16386, Kind::Simple, "public".into());
        let citexts = Type::new(
            "_citext".into(),
            16391,
            Kind::Array(citext.clone()),
            "public".into(),
        );
        assert_eq!(type_name(&Type::TIMESTAMPTZ), "timestamptz");
        assert_eq!(type_name(&Type::INT4_ARRAY), "int4[]");
        assert_eq!(type_name(&citext), "public.citext");
        assert_eq!(type_name(&citexts), "public.citext[]");
    }

    #[test]
    fn a_result_shape_that_changed_since_prepare_keeps_names_and_drops_types() {
        let typed = column_infos(&["a", "b"], &[Type::INT4, Type::TEXT]);
        assert_eq!(
            (
                typed[1].name(),
                typed[1].type_name(),
                typed[1].pg_type_oid()
            ),
            ("b", "text", Some(25))
        );
        let untyped = column_infos(&["a", "b", "c"], &[Type::INT4, Type::TEXT]);
        let got: Vec<_> = untyped
            .iter()
            .map(|c| (c.name(), c.type_name(), c.pg_type_oid()))
            .collect();
        assert_eq!(got, vec![("a", "", None), ("b", "", None), ("c", "", None)]);
    }
}
