//! SurrealDB harness: every `SrDb` is a fresh namespace, on its own container or on the server
//! `DBENTO_SURREAL_URL` names. The image asks for no licence acceptance. Setup talks to the
//! documented HTTP `/sql` endpoint, because no SurrealDB client crate may be used and this crate
//! never depends on dbento-core. Mirrors the SQL Server harness beside it.

use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};

use base64::Engine as _;
use testcontainers_modules::testcontainers::core::ContainerPort;
use testcontainers_modules::testcontainers::{
    ContainerAsync, ContainerRequest, GenericImage, ImageExt, runners::AsyncRunner,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::{
    ATTEMPT_LABEL, NEXT_ATTEMPT, NEXT_DB, RETRY_BACKOFF, remove_labelled, retry, retry_start,
    server_tls, wait_ready,
};

/// The gate image's tag.
pub const SURREAL_TAG: &str = "v3.3.5";

/// The nine logins of the protocol and fence probes, as (user, auth_level, role).
pub const SR_LOGINS: [(&str, &str, &str); 9] = [
    ("ro", "root", "OWNER"),
    ("re", "root", "EDITOR"),
    ("rv", "root", "VIEWER"),
    ("no", "namespace", "OWNER"),
    ("ne", "namespace", "EDITOR"),
    ("nv", "namespace", "VIEWER"),
    ("do", "database", "OWNER"),
    ("de", "database", "EDITOR"),
    ("dv", "database", "VIEWER"),
];

const IMAGE: &str = "surrealdb/surrealdb";
const SERVER_PORT: ContainerPort = ContainerPort::Tcp(8000);
const URL_VAR: &str = "DBENTO_SURREAL_URL";
const URL_33_VAR: &str = "DBENTO_SURREAL_33_URL";
const URL_FORM: &str = "surreal://user:password@host:port";

/// Where the SurrealDB tests get their server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SrSource {
    /// A running server at this URL.
    Url(String),
    /// A container of the gate image.
    Container,
}

/// `DBENTO_SURREAL_33_URL`, the opt-in run against v3.3.0, wins over `DBENTO_SURREAL_URL`; with
/// neither the tests start the gate image.
pub fn sr_source(env: impl Fn(&str) -> Option<String>) -> SrSource {
    [URL_33_VAR, URL_VAR]
        .into_iter()
        .find_map(|var| env(var).filter(|url| !url.is_empty()))
        .map_or(SrSource::Container, SrSource::Url)
}

/// Host, port, user and password of `surreal://user:password@host:port`, taken literally.
pub fn sr_url(url: &str) -> Result<(String, u16, String, String), String> {
    let bad = || format!("bad SurrealDB test URL {url:?}: expected {URL_FORM}");
    let rest = url.strip_prefix("surreal://").ok_or_else(bad)?;
    let (login, server) = rest.rsplit_once('@').ok_or_else(bad)?;
    let (user, password) = login.split_once(':').ok_or_else(bad)?;
    let (host, port) = server.rsplit_once(':').ok_or_else(bad)?;
    let port = port.parse::<u16>().map_err(|_| bad())?;
    if user.is_empty() || host.is_empty() {
        return Err(bad());
    }
    Ok((host.to_owned(), port, user.to_owned(), password.to_owned()))
}

/// Where and as whom the harness reaches a server.
#[derive(Debug, Clone)]
struct Endpoint {
    host: String,
    port: u16,
    user: String,
    password: String,
}

/// A fresh namespace. One holding a container must be dropped inside a tokio runtime.
pub struct SrDb {
    server: Endpoint,
    namespace: String,
    database: String,
    // Declared before the permit, so the container is gone before another may start.
    container: Option<ContainerAsync<GenericImage>>,
    _permit: Option<OwnedSemaphorePermit>,
}

impl SrDb {
    /// The server's host.
    pub fn host(&self) -> &str {
        &self.server.host
    }
    /// The server's WebSocket and HTTP port.
    pub fn port(&self) -> u16 {
        self.server.port
    }
    /// The root user's password, also the password of every login in [`SR_LOGINS`].
    pub fn password(&self) -> &str {
        &self.server.password
    }
    /// This test's namespace.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }
    /// This test's database.
    pub fn database(&self) -> &str {
        &self.database
    }

    /// `[connections.<name>]` for `tls = "off"` on this namespace; `login` is `(user, level)`,
    /// `None` for anonymous, and `extra` goes on its own line.
    pub fn connection_toml(&self, name: &str, login: Option<(&str, &str)>, extra: &str) -> String {
        let login = login.map_or(String::new(), |(user, level)| {
            format!("user = \"{user}\"\nauth_level = \"{level}\"\n")
        });
        format!(
            "[connections.{name}]\nengine = \"surrealdb\"\nhost = \"{}\"\nport = {}\nnamespace = \"{}\"\ndatabase = \"{}\"\ntls = \"off\"\n{login}{extra}\n",
            self.server.host, self.server.port, self.namespace, self.database
        )
    }

    /// Runs `sql` as the server's root user in this namespace and database, on a connection of
    /// its own, and returns each statement's JSON result. Panics on any statement error.
    pub async fn admin(&self, sql: &str) -> Vec<serde_json::Value> {
        http_sql(&self.server, &self.namespace, &self.database, sql)
            .await
            .unwrap_or_else(|e| panic!("admin {sql:?} failed: {e}"))
    }

    /// The first value of the last statement of `sql`, as [`admin`](Self::admin) runs it.
    pub async fn scalar(&self, sql: &str) -> serde_json::Value {
        let last = self.admin(sql).await.pop().unwrap_or_default();
        match last {
            serde_json::Value::Array(mut rows) if !rows.is_empty() => rows.swap_remove(0),
            other @ (serde_json::Value::Null
            | serde_json::Value::Bool(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::String(_)
            | serde_json::Value::Array(_)
            | serde_json::Value::Object(_)) => other,
        }
    }

    /// How many rows `table` holds; a table that does not exist holds none.
    pub async fn count(&self, table: &str) -> u64 {
        let tables = self.admin("INFO FOR DB").await.pop().unwrap_or_default();
        if tables["tables"].get(table).is_none() {
            return 0;
        }
        self.scalar(&format!("SELECT count() FROM `{table}` GROUP ALL"))
            .await["count"]
            .as_u64()
            .unwrap_or(0)
    }
}

impl Drop for SrDb {
    // A container goes with its guard; on a URL server the namespace is removed, on a thread of
    // its own because a drop cannot await.
    fn drop(&mut self) {
        if self.container.is_some() {
            return;
        }
        let server = self.server.clone();
        let sql = format!("REMOVE NAMESPACE IF EXISTS `{}`", self.namespace);
        let done = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            runtime
                .block_on(http_sql(&server, "main", "main", &sql))
                .map(drop)
        })
        .join();
        if !matches!(done, Ok(Ok(()))) {
            eprintln!("dbento-testkit: cannot remove namespace {}", self.namespace);
        }
    }
}

// One `POST /sql`; `Connection: close` ends the reply at EOF, and the body is one JSON array.
async fn http_sql(
    server: &Endpoint,
    namespace: &str,
    database: &str,
    sql: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let auth = base64::engine::general_purpose::STANDARD
        .encode(format!("{}:{}", server.user, server.password));
    let request = format!(
        "POST /sql HTTP/1.1\r\nHost: {}:{}\r\nAccept: application/json\r\nAuthorization: Basic {auth}\r\nSurreal-NS: {namespace}\r\nSurreal-DB: {database}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sql}",
        server.host,
        server.port,
        sql.len()
    );
    let mut socket = TcpStream::connect((server.host.as_str(), server.port))
        .await
        .map_err(|e| e.to_string())?;
    socket
        .write_all(request.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let mut reply = Vec::new();
    socket
        .read_to_end(&mut reply)
        .await
        .map_err(|e| e.to_string())?;
    let reply = String::from_utf8_lossy(&reply);
    let (head, body) = reply.split_once("\r\n\r\n").ok_or("no HTTP body")?;
    if !head.starts_with("HTTP/1.1 200") {
        return Err(format!("{head}\n{body}"));
    }
    let statements: Vec<serde_json::Value> =
        serde_json::from_str(body).map_err(|e| format!("{e}: {body}"))?;
    statements
        .into_iter()
        .map(|statement| match statement["status"].as_str() {
            Some("OK") => Ok(statement["result"].clone()),
            _ => Err(statement.to_string()),
        })
        .collect()
}

/// A fresh namespace on the server [`sr_source`] picks, holding an empty database `app` and the
/// nine logins of [`SR_LOGINS`].
pub async fn surreal_db() -> SrDb {
    let mut db = match sr_source(|k| std::env::var(k).ok()) {
        SrSource::Url(url) => {
            let (host, port, user, password) = sr_url(&url).unwrap_or_else(|e| panic!("{e}"));
            fresh(
                Endpoint {
                    host,
                    port,
                    user,
                    password,
                },
                None,
                None,
            )
        }
        SrSource::Container => {
            let all = [("SURREAL_CAPS_ALLOW_ALL".to_owned(), "true".to_owned())];
            server(SURREAL_TAG, &all, |r| r).await
        }
    };
    db.namespace = format!(
        "dbento_t_{}_{}",
        std::process::id(),
        NEXT_DB.fetch_add(1, Ordering::Relaxed)
    );
    db.database = "app".to_owned();
    // Every statement overwrites, so a seed retried after a reset that the server had already
    // applied lands the same fixture instead of failing on "already exists".
    let mut sql = format!(
        "DEFINE NAMESPACE OVERWRITE `{0}`; USE NS `{0}`; DEFINE DATABASE OVERWRITE `{1}`; USE DB `{1}`;",
        db.namespace, db.database
    );
    for (user, level, role) in SR_LOGINS {
        let on = level.to_uppercase();
        sql.push_str(&format!(
            "DEFINE USER OVERWRITE {user} ON {on} PASSWORD '{}' ROLES {role};",
            db.password()
        ));
    }
    retry("seed", RETRY_BACKOFF, |_| {
        http_sql(&db.server, "main", "main", &sql)
    })
    .await
    .unwrap_or_else(|e| panic!("cannot create the fixture: {e}"));
    db
}

/// A fresh container of `tag` with `env` set, `DBENTO_SURREAL_URL` ignored, on `main`/`main`, the
/// target a fresh v3 server defines. Nothing else is set up.
pub async fn surreal_server(tag: &str, env: &[(&str, &str)]) -> SrDb {
    let env: Vec<(String, String)> = env
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    server(tag, &env, |r| r).await
}

/// A fresh container serving wss with a "localhost" certificate from a fresh CA, on host
/// "localhost". The string is the CA's PEM. `admin` cannot reach it, as it speaks plain HTTP.
pub async fn surreal_tls() -> (SrDb, String) {
    let tls = server_tls();
    let (cert, key) = (tls.cert_pem.clone(), tls.key_pem.clone());
    let env = [
        ("SURREAL_WEB_CRT".to_owned(), "/tmp/server.crt".to_owned()),
        ("SURREAL_WEB_KEY".to_owned(), "/tmp/server.key".to_owned()),
    ];
    let mut db = server(SURREAL_TAG, &env, move |r| {
        r.with_copy_to("/tmp/server.crt", cert.clone().into_bytes())
            .with_copy_to("/tmp/server.key", key.clone().into_bytes())
    })
    .await;
    db.server.host = "localhost".to_owned();
    (db, tls.ca_pem)
}

/// Four containers at a time in this process; each is capped at 2 GiB.
fn slots() -> Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    Arc::clone(SLOTS.get_or_init(|| Arc::new(Semaphore::new(4))))
}

fn fresh(
    server: Endpoint,
    container: Option<ContainerAsync<GenericImage>>,
    permit: Option<OwnedSemaphorePermit>,
) -> SrDb {
    SrDb {
        server,
        namespace: "main".to_owned(),
        database: "main".to_owned(),
        container,
        _permit: permit,
    }
}

// Starts `tag` labelled and memory-capped on the memory engine, retried when its host port turns
// out to be taken. The password is per run, so none is kept in a file.
async fn server(
    tag: &str,
    env: &[(String, String)],
    with: impl Fn(ContainerRequest<GenericImage>) -> ContainerRequest<GenericImage>,
) -> SrDb {
    let permit = slots()
        .acquire_owned()
        .await
        .expect("the container semaphore is never closed");
    super::sweep_orphans().await;
    let password = format!("test-{}", std::process::id());
    let c = retry_start(|_| {
        let attempt = format!(
            "{}-{}",
            std::process::id(),
            NEXT_ATTEMPT.fetch_add(1, Ordering::Relaxed)
        );
        let mut request = GenericImage::new(IMAGE, tag)
            .with_exposed_port(SERVER_PORT)
            .with_cmd(["start"])
            .with_env_var("SURREAL_USER", "root")
            .with_env_var("SURREAL_PASS", password.clone())
            .with_env_var("SURREAL_PATH", "memory")
            .with_host_config_modifier(|h| h.memory = Some(2 << 30))
            .with_label("dbento.role", "test")
            .with_label("dbento.pid", std::process::id().to_string())
            .with_label(ATTEMPT_LABEL, attempt.clone());
        for (key, value) in env {
            request = request.with_env_var(key.clone(), value.clone());
        }
        let request = with(request);
        async move {
            match request.start().await {
                Ok(container) => Ok(container),
                Err(e) => {
                    remove_labelled(&format!("{ATTEMPT_LABEL}={attempt}")).await;
                    Err(e.to_string())
                }
            }
        }
    })
    .await
    .unwrap_or_else(|e| {
        panic!(
            "cannot start the SurrealDB test container: {e}\n\
             Rootless podman: set DOCKER_HOST=unix://$XDG_RUNTIME_DIR/podman/podman.sock\n\
             Or skip containers and point {URL_VAR} at a running server."
        )
    });
    let port = c
        .get_host_port_ipv4(SERVER_PORT)
        .await
        .expect("no mapped SurrealDB port");
    // No log wait: journald's rate limit can drop the startup log, so the server's own answer
    // on `/health` is the readiness signal.
    let tls = env.iter().any(|(key, _)| key == "SURREAL_WEB_CRT");
    wait_ready(&c, "SurrealDB", || health("127.0.0.1", port, tls)).await;
    let server = Endpoint {
        host: "127.0.0.1".to_owned(),
        port,
        user: "root".to_owned(),
        password,
    };
    fresh(server, Some(c), Some(permit))
}

/// One plaintext `GET /health`, judged by [`answered`].
async fn health(host: &str, port: u16, tls: bool) -> Result<(), String> {
    let request =
        format!("GET /health HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
    let mut socket = TcpStream::connect((host, port))
        .await
        .map_err(|e| e.to_string())?;
    socket
        .write_all(request.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let mut reply = Vec::new();
    // A reset after a TLS alert still leaves the alert in `reply`, so only the bytes count.
    let _ = socket.read_to_end(&mut reply).await;
    answered(&reply, tls)
}

/// Whether a `/health` reply shows a ready server. A plain server answers 200; a TLS server
/// answers plaintext with an alert record (content type 0x15), which proves it is listening.
/// Before the server binds, podman's port proxy accepts and closes with nothing.
fn answered(reply: &[u8], tls: bool) -> Result<(), String> {
    let ready = if tls {
        reply.first() == Some(&0x15)
    } else {
        reply.starts_with(b"HTTP/1.1 200")
    };
    if ready {
        return Ok(());
    }
    let head = String::from_utf8_lossy(reply);
    let head = head.lines().next().unwrap_or("nothing");
    Err(format!("/health answered {head:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        let pairs = pairs.to_vec();
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn the_source_prefers_the_33_url_then_the_url_then_a_container() {
        let url = "surreal://root:p@h:1";
        let old = "surreal://root:p@h:2";
        assert_eq!(sr_source(env(&[])), SrSource::Container);
        assert_eq!(sr_source(env(&[(URL_VAR, "")])), SrSource::Container);
        assert_eq!(
            sr_source(env(&[(URL_VAR, url)])),
            SrSource::Url(url.to_owned())
        );
        assert_eq!(
            sr_source(env(&[(URL_VAR, url), (URL_33_VAR, old)])),
            SrSource::Url(old.to_owned())
        );
    }

    #[test]
    fn a_url_names_its_login_and_server_or_says_what_it_should_look_like() {
        assert_eq!(
            sr_url("surreal://root:p:q@127.0.0.1:8000").unwrap(),
            (
                "127.0.0.1".to_owned(),
                8000,
                "root".to_owned(),
                "p:q".to_owned()
            )
        );
        for bad in [
            "http://a:b@h:1",
            "surreal://h:1",
            "surreal://:p@h:1",
            "surreal://a:b@h",
        ] {
            assert!(sr_url(bad).unwrap_err().contains(URL_FORM), "{bad}");
        }
    }

    #[test]
    fn health_counts_a_200_or_on_tls_an_alert_and_never_a_closed_socket() {
        let ok = b"HTTP/1.1 200 OK\r\nserver: SurrealDB\r\n\r\n";
        let alert = [0x15, 0x03, 0x03, 0x00, 0x02, 0x02, 0x32];
        assert_eq!(answered(ok, false), Ok(()));
        assert_eq!(answered(&alert, true), Ok(()));
        assert!(answered(b"", false).is_err());
        assert!(answered(b"", true).is_err());
        assert!(answered(b"HTTP/1.1 503 Service Unavailable\r\n\r\n", false).is_err());
        assert!(answered(&alert, false).is_err());
        assert!(answered(ok, true).is_err());
    }

    // Journald's rate limit can drop a container's whole startup log, so readiness must come
    // from the server itself: at log level none it never prints "Started web server".
    #[tokio::test]
    async fn a_server_that_logs_nothing_still_starts() {
        let silent = [("SURREAL_LOG".to_owned(), "none".to_owned())];
        let start = server(SURREAL_TAG, &silent, |r| r);
        let db = tokio::time::timeout(std::time::Duration::from_secs(45), start)
            .await
            .expect("the start waited for a log line that never came");
        assert_eq!(db.scalar("RETURN 7").await, 7);
    }

    #[tokio::test]
    async fn a_fresh_namespace_holds_the_nine_logins_and_counts_tables() {
        let db = surreal_db().await;
        assert!(db.namespace().starts_with("dbento_t_"));
        let info = db.admin("INFO FOR ROOT; INFO FOR NS; INFO FOR DB").await;
        for (user, level, _) in SR_LOGINS {
            let at = ["root", "namespace", "database"]
                .iter()
                .position(|l| *l == level)
                .unwrap();
            assert!(info[at]["users"].get(user).is_some(), "{user}");
        }
        assert_eq!(db.count("nothing").await, 0);
        db.admin("CREATE a:1; CREATE a:2").await;
        assert_eq!(db.count("a").await, 2);
        assert_eq!(db.scalar("RETURN 7").await, 7);
    }
}
