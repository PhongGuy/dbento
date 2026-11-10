//! Test-only Postgres, MySQL and SQL Server harness: every `PgDb`, `MyDb` or `MsDb` is a fresh
//! database, on its own container or on the server a URL variable names. Never depends on
//! dbento-core.

// A harness that cannot start its server has nothing to report but the panic.
#![allow(clippy::expect_used)]

use std::collections::{BTreeSet, HashMap};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::bollard::query_parameters::{
    InspectContainerOptions, ListContainersOptions, ListVolumesOptions, RemoveContainerOptions,
};
use testcontainers_modules::testcontainers::bollard::{API_DEFAULT_VERSION, Docker};
use testcontainers_modules::testcontainers::{
    ContainerAsync, ContainerRequest, ImageExt, core::Mount, runners::AsyncRunner,
};
use tokio_postgres::{Client, Config, NoTls, config::Host};

#[cfg(feature = "duckdb")]
mod duck;
#[cfg(feature = "mongodb")]
mod mongo;
mod mssql;
mod mysql;
#[cfg(feature = "redis")]
mod redis;
mod surreal;
#[cfg(feature = "redis")]
pub use self::redis::*;
#[cfg(feature = "duckdb")]
pub use duck::duckdb_file;
#[cfg(feature = "mongodb")]
pub use mongo::*;
pub use mssql::*;
pub use mysql::*;
pub use surreal::*;

/// The environment variable the generated connection reads its password from.
pub const SECRET_VAR: &str = "DBENTO_TEST_PG_SECRET";

static NEXT_DB: AtomicU64 = AtomicU64::new(0);

/// A fresh database. One holding a container must be dropped inside a tokio runtime; one on
/// a shared server is dropped from that server when the handle drops.
pub struct PgDb {
    host: String,
    port: u16,
    user: String,
    password: String,
    database: String,
    // Held so the container lives exactly as long as the database handle.
    _container: Option<ContainerAsync<Postgres>>,
}

impl PgDb {
    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn user(&self) -> &str {
        &self.user
    }
    pub fn password(&self) -> &str {
        &self.password
    }
    pub fn database(&self) -> &str {
        &self.database
    }
    /// The engine's id of the container this database lives on, if it has one.
    pub fn container_id(&self) -> Option<&str> {
        self._container.as_ref().map(|c| c.id())
    }

    /// URL of the server's `postgres` database, not of this test database.
    pub fn server_url(&self) -> String {
        format!(
            "postgres://{}:{}@{}:{}/postgres",
            self.user, self.password, self.host, self.port
        )
    }

    /// `[connections.<name>]` for this database, with the secret read from `SECRET_VAR`
    /// and `extra` on its own line.
    pub fn connection_toml(&self, name: &str, extra: &str) -> String {
        format!(
            "[connections.{name}]\nengine = \"postgres\"\nhost = \"{}\"\nport = {}\ndatabase = \"{}\"\nuser = \"{}\"\nsecret = {{ env = \"{SECRET_VAR}\" }}\n{extra}\n",
            self.host, self.port, self.database, self.user
        )
    }

    /// A plain connection to this database.
    pub async fn admin(&self) -> Client {
        connect_to(
            &self.host,
            self.port,
            &self.user,
            &self.password,
            &self.database,
        )
        .await
    }

    /// The first cell of the first row, as text.
    pub async fn scalar(&self, sql: &str) -> Option<String> {
        let client = self.admin().await;
        let rows = client.simple_query(sql).await.expect("scalar query failed");
        rows.into_iter().find_map(|m| match m {
            tokio_postgres::SimpleQueryMessage::Row(r) => r.get(0).map(str::to_owned),
            _ => None,
        })
    }
}

impl Drop for PgDb {
    // Removing a container removes its databases. On a shared server the drop runs on its own
    // thread and runtime, so joining it is safe inside a test's runtime; it never panics,
    // since a panic during an unwinding test would abort.
    fn drop(&mut self) {
        if self._container.is_some() {
            return;
        }
        let mut config = Config::new();
        config
            .host(&self.host)
            .port(self.port)
            .user(&self.user)
            .password(&self.password)
            .dbname("postgres")
            .connect_timeout(std::time::Duration::from_secs(10));
        let name = self.database.clone();
        let dropped = std::thread::spawn(move || -> Result<(), String> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            runtime.block_on(async {
                let (client, connection) =
                    config.connect(NoTls).await.map_err(|e| e.to_string())?;
                tokio::spawn(connection);
                client
                    .batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
                    .await
                    .map_err(|e| e.to_string())
            })
        })
        .join();
        match dropped {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("dbento-testkit: cannot drop {}: {e}", self.database),
            Err(_) => eprintln!("dbento-testkit: dropping {} panicked", self.database),
        }
    }
}

async fn connect_to(host: &str, port: u16, user: &str, password: &str, database: &str) -> Client {
    let (client, connection) = Config::new()
        .host(host)
        .port(port)
        .user(user)
        .password(password)
        .dbname(database)
        .connect(NoTls)
        .await
        .unwrap_or_else(|e| panic!("cannot connect to {host}:{port}/{database}: {e}"));
    tokio::spawn(connection);
    client
}

/// `DBENTO_PG_URL` when set, otherwise a new container.
pub async fn pg_db() -> PgDb {
    match std::env::var("DBENTO_PG_URL") {
        Ok(url) if !url.is_empty() => pg_db_at(&url).await,
        _ => pg_container().await,
    }
}

/// The Postgres majors the matrix covers.
pub const MATRIX_VERSIONS: [u8; 5] = [14, 15, 16, 17, 18];

/// ("pg<NN>", url) for every DBENTO_PG_<NN>_URL that `env` gives a non-empty value, in version order.
pub fn matrix_urls(env: impl Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    MATRIX_VERSIONS
        .iter()
        .filter_map(|v| {
            let url = env(&format!("DBENTO_PG_{v}_URL")).filter(|u| !u.is_empty())?;
            Some((format!("pg{v}"), url))
        })
        .collect()
}

/// A fresh database on each matrix server; with none set, one on `pg_db()` labelled "default".
pub async fn matrix() -> Vec<(String, PgDb)> {
    let urls = matrix_urls(|k| std::env::var(k).ok());
    if urls.is_empty() {
        return vec![("default".to_owned(), pg_db().await)];
    }
    let mut out = Vec::new();
    for (label, url) in urls {
        out.push((label, pg_db_at(&url).await));
    }
    out
}

/// Chinook's Postgres script after its `\c chinook;` line: tables and rows, no database commands.
pub fn chinook_pg_sql() -> &'static str {
    let full = include_str!("../../../datasets/chinook/Chinook_PostgreSql.sql");
    let (_, rest) = full
        .split_once("\\c chinook;")
        .expect("Chinook script has a \\c chinook; line");
    rest
}

/// Chinook's SQLite script, byte for byte.
pub fn chinook_sqlite_sql() -> &'static str {
    include_str!("../../../datasets/chinook/Chinook_Sqlite.sql")
}

/// A new `postgres:17-alpine` container with a fresh database on it.
pub async fn pg_container() -> PgDb {
    start(|| Postgres::default().into(), "127.0.0.1").await
}

/// A fresh CA and a "localhost" server certificate it signed.
pub(crate) struct ServerTls {
    issuer: Issuer<'static, KeyPair>,
    pub(crate) ca_pem: String,
    pub(crate) cert_pem: String,
    pub(crate) key_pem: String,
}

pub(crate) fn server_tls() -> ServerTls {
    let ca_key = KeyPair::generate().expect("CA key");
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA params");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_cert = ca_params.self_signed(&ca_key).expect("CA certificate");
    let issuer = Issuer::new(ca_params, ca_key);
    let key = KeyPair::generate().expect("server key");
    let cert = CertificateParams::new(vec!["localhost".to_owned()])
        .expect("server params")
        .signed_by(&key, &issuer)
        .expect("server certificate");
    ServerTls {
        issuer,
        ca_pem: ca_cert.pem(),
        cert_pem: cert.pem(),
        key_pem: key.serialize_pem(),
    }
}

/// The init script that installs the server certificate and turns `ssl` on. The entrypoint
/// sources *.sh as the postgres user after initdb, so the key gets the owner and mode the
/// server demands; the final server start then reads ssl = on.
fn tls_script(tls: &ServerTls) -> String {
    format!(
        "cat > \"$PGDATA/server.crt\" <<'EOF'\n{}EOF\ncat > \"$PGDATA/server.key\" <<'EOF'\n{}EOF\nchmod 600 \"$PGDATA/server.key\"\necho 'ssl = on' >> \"$PGDATA/postgresql.conf\"\n",
        tls.cert_pem, tls.key_pem
    )
}

/// A container with `ssl = on` and a server certificate for "localhost", signed by a fresh CA.
/// Returns the database (host "localhost") and the CA's PEM.
pub async fn pg_tls() -> (PgDb, String) {
    let tls = server_tls();
    let script = tls_script(&tls);
    let image = || {
        Postgres::default().with_copy_to(
            "/docker-entrypoint-initdb.d/tls.sh",
            script.as_bytes().to_vec(),
        )
    };
    (start(image, "localhost").await, tls.ca_pem)
}

/// The role `pg_client_cert` admits only with a client certificate.
pub const CERT_USER: &str = "certuser";

/// The CA behind `pg_client_cert` or `my_tls` and a client certificate it signed for [`CERT_USER`].
pub struct ClientCert {
    /// The CA's PEM, which signed the server and the client certificate.
    pub ca_pem: String,
    /// The client certificate's PEM, common name [`CERT_USER`].
    pub cert_pem: String,
    /// The client key as an unencrypted PKCS#8 PEM.
    pub key_pem: String,
    /// The same key as PKCS#8 DER, for a test that encrypts it.
    pub key_der: Vec<u8>,
}

/// Like [`pg_tls`], plus a login role [`CERT_USER`] that `pg_hba.conf` admits only over TLS
/// with a client certificate the same CA signed (`hostssl ... cert`). The `postgres` admin
/// keeps its password login, so the harness still makes the fresh database.
pub async fn pg_client_cert() -> (PgDb, ClientCert) {
    let tls = server_tls();
    let client = client_cert(&tls);
    // The cert line goes first, since the first matching pg_hba line decides.
    let script = format!(
        "{}cat > \"$PGDATA/root.crt\" <<'EOF'\n{}EOF\n\
         echo \"ssl_ca_file = 'root.crt'\" >> \"$PGDATA/postgresql.conf\"\n\
         {{ echo 'hostssl all {CERT_USER} all cert'; cat \"$PGDATA/pg_hba.conf\"; }} > /tmp/hba\n\
         cat /tmp/hba > \"$PGDATA/pg_hba.conf\"\n\
         psql -v ON_ERROR_STOP=1 --username \"$POSTGRES_USER\" -c 'CREATE ROLE {CERT_USER} LOGIN'\n",
        tls_script(&tls),
        tls.ca_pem
    );
    let image = || {
        Postgres::default().with_copy_to(
            "/docker-entrypoint-initdb.d/tls.sh",
            script.as_bytes().to_vec(),
        )
    };
    (start(image, "localhost").await, client)
}

/// A client certificate for [`CERT_USER`] that the CA behind `tls` signed.
pub(crate) fn client_cert(tls: &ServerTls) -> ClientCert {
    let key = KeyPair::generate().expect("client key");
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("client params");
    params
        .distinguished_name
        .push(DnType::CommonName, CERT_USER);
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let cert = params
        .signed_by(&key, &tls.issuer)
        .expect("client certificate");
    ClientCert {
        ca_pem: tls.ca_pem.clone(),
        cert_pem: cert.pem(),
        key_pem: key.serialize_pem(),
        key_der: key.serialize_der(),
    }
}

/// Starts the request `image` builds at tag 17-alpine and makes a fresh database reached
/// through `host`. A request is single-use, so each attempt builds its own.
async fn start(image: impl Fn() -> ContainerRequest<Postgres>, host: &str) -> PgDb {
    sweep_orphans().await;
    let container = retry_start(|_| {
        // Each attempt carries its own label, so a container left behind by a failed start
        // can be found and removed.
        let attempt = format!(
            "{}-{}",
            std::process::id(),
            NEXT_ATTEMPT.fetch_add(1, Ordering::Relaxed)
        );
        let request = image()
            .with_tag("17-alpine")
            // A tmpfs over the image's declared VOLUME keeps the runtime from creating an
            // anonymous volume, which outlives a run that is killed before drop.
            .with_mount(Mount::tmpfs_mount("/var/lib/postgresql/data"))
            .with_label("dbento.role", "test")
            .with_label("dbento.pid", std::process::id().to_string())
            .with_label(ATTEMPT_LABEL, attempt.clone());
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
    .unwrap_or_else(|e| panic!("{}", start_hint(&e)));
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("no mapped Postgres port");
    let mut db = fresh_database(host, port, "postgres", "postgres").await;
    db._container = Some(container);
    db
}

/// How many times a container start is tried when its host port turns out to be taken.
const START_ATTEMPTS: u32 = 3;

const ATTEMPT_LABEL: &str = "dbento.attempt";

static NEXT_ATTEMPT: AtomicU64 = AtomicU64::new(0);

/// Runs `attempt` (numbered from 1) until it succeeds, fails for another reason, or the
/// attempts run out. Rootless podman picks a container's host port at create time but binds
/// it only at start, so another socket can take it in between; a fresh attempt gets a new one.
async fn retry_start<T, F, Fut>(mut attempt: F) -> Result<T, String>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let mut n = 1;
    loop {
        match attempt(n).await {
            Err(e) if n < START_ATTEMPTS && e.contains("address already in use") => {
                eprintln!("dbento-testkit: start attempt {n} found its host port taken; retrying");
                n += 1;
            }
            done => return done,
        }
    }
}

/// Ids of test containers whose `dbento.pid` names a process for which `alive` is false.
/// Containers of another role, or without a numeric pid label, are never listed.
fn orphans(
    containers: impl IntoIterator<Item = (String, HashMap<String, String>)>,
    alive: impl Fn(u32) -> bool,
) -> Vec<String> {
    containers
        .into_iter()
        .filter(|(_, labels)| labels.get("dbento.role").map(String::as_str) == Some("test"))
        .filter_map(|(id, labels)| {
            let pid: u32 = labels.get("dbento.pid")?.parse().ok()?;
            (!alive(pid)).then_some(id)
        })
        .collect()
}

/// Once per process, removes the test containers of runs that died before their drop ran,
/// such as a killed suite. Only Linux can tell, from `/proc`; elsewhere nothing is swept.
async fn sweep_orphans() {
    static SWEPT: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    if !cfg!(target_os = "linux") {
        return;
    }
    SWEPT
        .get_or_init(|| async {
            let alive = |pid: u32| std::path::Path::new(&format!("/proc/{pid}")).exists();
            for id in orphans(labelled("dbento.role=test").await, alive) {
                remove(&id).await;
            }
        })
        .await;
}

/// Every container, running or not, carrying `label` (`key=value`), with its labels. Empty
/// when the runtime cannot be reached; the start that follows reports that.
async fn labelled(label: &str) -> Vec<(String, HashMap<String, String>)> {
    let Ok(docker) = engine() else {
        return Vec::new();
    };
    let options = ListContainersOptions {
        all: true,
        filters: Some(HashMap::from([(
            "label".to_owned(),
            vec![label.to_owned()],
        )])),
        ..Default::default()
    };
    docker
        .list_containers(Some(options))
        .await
        .unwrap_or_default()
        .into_iter()
        .filter_map(|c| Some((c.id?, c.labels.unwrap_or_default())))
        .collect()
}

/// Force-removes every container carrying `label`; failures are reported, never raised.
async fn remove_labelled(label: &str) {
    for (id, _) in labelled(label).await {
        remove(&id).await;
    }
}

async fn remove(id: &str) {
    let Ok(docker) = engine() else {
        return;
    };
    let options = RemoveContainerOptions {
        force: true,
        v: true,
        ..Default::default()
    };
    if let Err(e) = docker.remove_container(id, Some(options)).await {
        eprintln!("dbento-testkit: cannot remove container {id}: {e}");
    }
}

/// The engine address testcontainers 0.27 connects to, in its order: `DOCKER_HOST`, then the
/// first Docker socket that exists, then Docker's default. Its `~/.testcontainers.properties`
/// lookup sits behind the `properties-config` feature, which this workspace leaves off.
fn docker_host(env: impl Fn(&str) -> Option<String>, exists: impl Fn(&str) -> bool) -> String {
    if let Some(host) = env("DOCKER_HOST") {
        return host;
    }
    let runtime = env("XDG_RUNTIME_DIR").map(|dir| format!("{dir}/.docker/run/docker.sock"));
    let home = env("HOME");
    let home_run = home
        .as_ref()
        .map(|h| format!("{h}/.docker/run/docker.sock"));
    let desktop = home
        .as_ref()
        .map(|h| format!("{h}/.docker/desktop/docker.sock"));
    [
        Some("/var/run/docker.sock".to_owned()),
        runtime,
        home_run,
        desktop,
    ]
    .into_iter()
    .flatten()
    .find(|path| exists(path))
    .map_or_else(
        || "unix:///var/run/docker.sock".to_owned(),
        |p| format!("unix://{p}"),
    )
}

/// A client for the engine that testcontainers starts the containers on, Docker or podman,
/// through the Docker-compatible API both expose.
fn engine() -> Result<Docker, String> {
    let host = docker_host(
        |key| std::env::var(key).ok(),
        |path| std::path::Path::new(path).exists(),
    );
    let connected = if host.starts_with("unix://") {
        Docker::connect_with_unix(&host, 120, API_DEFAULT_VERSION)
    } else {
        Docker::connect_with_http(&host, 120, API_DEFAULT_VERSION)
    };
    connected.map_err(|e| format!("cannot reach the container engine at {host}: {e}"))
}

/// The names of every volume on the engine testcontainers uses.
pub async fn volumes() -> Result<BTreeSet<String>, String> {
    let listed = engine()?
        .list_volumes(None::<ListVolumesOptions>)
        .await
        .map_err(|e| format!("cannot list volumes: {e}"))?;
    Ok(listed
        .volumes
        .unwrap_or_default()
        .into_iter()
        .map(|v| v.name)
        .collect())
}

/// One mount of a container, as the engine reports it.
#[derive(Debug, Clone)]
pub struct ContainerMount {
    /// `volume`, `bind`, `tmpfs` and so on.
    pub kind: String,
    /// The volume's name, for a mount of kind `volume`.
    pub name: Option<String>,
    pub destination: Option<String>,
}

/// The mounts of the container with id `container`, from the engine's inspect.
pub async fn mounts(container: &str) -> Result<Vec<ContainerMount>, String> {
    let inspected = engine()?
        .inspect_container(container, None::<InspectContainerOptions>)
        .await
        .map_err(|e| format!("cannot inspect container {container}: {e}"))?;
    Ok(inspected
        .mounts
        .unwrap_or_default()
        .into_iter()
        .map(|m| ContainerMount {
            kind: m.typ.map(|t| t.to_string()).unwrap_or_default(),
            name: m.name,
            destination: m.destination,
        })
        .collect())
}

/// A fresh database on the server at `url`, dropped when the handle drops.
pub async fn pg_db_at(url: &str) -> PgDb {
    let (host, port, user, password) = server_of(url);
    fresh_database(&host, port, &user, &password).await
}

/// Host, port, user and password from a server URL.
fn server_of(url: &str) -> (String, u16, String, String) {
    let config = Config::from_str(url).unwrap_or_else(|e| panic!("bad DBENTO_PG_URL: {e}"));
    let host = match config.get_hosts().first() {
        Some(Host::Tcp(h)) => h.clone(),
        other => panic!("DBENTO_PG_URL needs a TCP host, got {other:?}"),
    };
    let port = config.get_ports().first().copied().unwrap_or(5432);
    let user = config.get_user().unwrap_or("postgres").to_owned();
    let password = config
        .get_password()
        .map(|p| String::from_utf8(p.to_vec()).expect("password is not UTF-8"))
        .unwrap_or_default();
    (host, port, user, password)
}

/// Drops test databases on the server at `url` left by runs that are gone: the pid in the
/// name is not this process and not alive, and no session uses the database. Errors are
/// ignored. Linux only, since liveness is read from /proc; elsewhere it does nothing.
pub async fn sweep_stale(url: &str) {
    let (host, port, user, password) = server_of(url);
    sweep(&host, port, &user, &password).await;
}

#[cfg(not(target_os = "linux"))]
async fn sweep(_host: &str, _port: u16, _user: &str, _password: &str) {}

#[cfg(target_os = "linux")]
async fn sweep(host: &str, port: u16, user: &str, password: &str) {
    let connected = Config::new()
        .host(host)
        .port(port)
        .user(user)
        .password(password)
        .dbname("postgres")
        .connect_timeout(std::time::Duration::from_secs(10))
        .connect(NoTls)
        .await;
    let Ok((client, connection)) = connected else {
        return;
    };
    tokio::spawn(connection);
    let Ok(rows) = client
        .query(
            "SELECT datname::text FROM pg_database WHERE datname LIKE 'dbento\\_t\\_%' \
             AND datname NOT IN \
             (SELECT datname FROM pg_stat_activity WHERE datname IS NOT NULL)",
            &[],
        )
        .await
    else {
        return;
    };
    for row in rows {
        let name: String = row.get(0);
        let pid = name
            .strip_prefix("dbento_t_")
            .and_then(|rest| rest.split('_').next())
            .and_then(|pid| pid.parse::<u32>().ok());
        let Some(pid) = pid else { continue };
        if pid == std::process::id() || std::path::Path::new(&format!("/proc/{pid}")).exists() {
            continue;
        }
        // No FORCE: a session that connected since the listing makes the drop fail, and the
        // database survives.
        let quoted = name.replace('"', "\"\"");
        let _ = client
            .batch_execute(&format!("DROP DATABASE IF EXISTS \"{quoted}\""))
            .await;
    }
}

/// Servers already swept by this process, so each is swept once, before its first database.
static SWEPT: std::sync::Mutex<Vec<(String, u16)>> = std::sync::Mutex::new(Vec::new());

async fn fresh_database(host: &str, port: u16, user: &str, password: &str) -> PgDb {
    let first_here = {
        let mut swept = SWEPT.lock().unwrap_or_else(|e| e.into_inner());
        let key = (host.to_owned(), port);
        let first = !swept.contains(&key);
        if first {
            swept.push(key);
        }
        first
    };
    if first_here {
        sweep(host, port, user, password).await;
    }
    let name = format!(
        "dbento_t_{}_{}",
        std::process::id(),
        NEXT_DB.fetch_add(1, Ordering::Relaxed)
    );
    let admin = connect_to(host, port, user, password, "postgres").await;
    // Two calls: neither statement may run inside a multi-statement query's implicit transaction.
    admin
        .batch_execute(&format!("DROP DATABASE IF EXISTS {name}"))
        .await
        .expect("drop database");
    admin
        .batch_execute(&format!("CREATE DATABASE {name}"))
        .await
        .expect("create database");
    PgDb {
        host: host.to_owned(),
        port,
        user: user.to_owned(),
        password: password.to_owned(),
        database: name,
        _container: None,
    }
}

/// The panic text when a container cannot start.
pub fn start_hint(error: &str) -> String {
    format!(
        "cannot start the Postgres test container: {error}\n\
         Rootless podman: set DOCKER_HOST=unix://$XDG_RUNTIME_DIR/podman/podman.sock\n\
         Or skip containers and point DBENTO_PG_URL at a running server."
    )
}

/// A connection that LISTENs on one channel and keeps every payload it hears.
pub struct Listener {
    _client: Client,
    payloads: tokio::sync::mpsc::UnboundedReceiver<Result<String, String>>,
}

impl PgDb {
    /// Starts listening on `channel`, an identifier, before returning.
    pub async fn listen(&self, channel: &str) -> Listener {
        let (client, mut connection) = Config::new()
            .host(&self.host)
            .port(self.port)
            .user(&self.user)
            .password(&self.password)
            .dbname(&self.database)
            .connect(NoTls)
            .await
            .unwrap_or_else(|e| panic!("cannot connect the listener: {e}"));
        let (tx, payloads) = tokio::sync::mpsc::unbounded_channel();
        // Notifications surface only through poll_message, so the task drives that stream
        // instead of awaiting the connection future.
        tokio::spawn(async move {
            let mut messages = futures_util::stream::poll_fn(move |cx| connection.poll_message(cx));
            // The connection ends only when the Listener drops or the backend dies; the
            // last message tells received() which, so a dead listener cannot read as silence.
            let end = loop {
                match futures_util::StreamExt::next(&mut messages).await {
                    Some(Ok(tokio_postgres::AsyncMessage::Notification(n))) => {
                        let _ = tx.send(Ok(n.payload().to_owned()));
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => break e.to_string(),
                    None => break "closed by the server".to_owned(),
                }
            };
            let _ = tx.send(Err(end));
        });
        client
            .batch_execute(&format!("LISTEN {channel}"))
            .await
            .expect("LISTEN failed");
        Listener {
            _client: client,
            payloads,
        }
    }
}

impl Listener {
    /// Every payload heard so far, after waiting `wait` for stragglers.
    /// Panics if the listening connection has died, since it may have missed a notification.
    pub async fn received(&mut self, wait: std::time::Duration) -> Vec<String> {
        use tokio::sync::mpsc::error::TryRecvError;
        tokio::time::sleep(wait).await;
        let mut out = Vec::new();
        loop {
            match self.payloads.try_recv() {
                Ok(Ok(p)) => out.push(p),
                Ok(Err(e)) => panic!("the listener connection died: {e}"),
                Err(TryRecvError::Empty) => return out,
                Err(TryRecvError::Disconnected) => {
                    panic!("the listener connection task ended without a reason")
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAKEN: &str = "Docker responded with status code 500: rootlessport listen tcp :34037: bind: address already in use";

    #[tokio::test]
    async fn a_taken_host_port_is_retried_with_a_fresh_attempt() {
        let mut seen = Vec::new();
        let r = retry_start(|n| {
            seen.push(n);
            async move { if n == 1 { Err(TAKEN.to_owned()) } else { Ok(n) } }
        })
        .await;
        assert_eq!(r, Ok(2));
        assert_eq!(seen, vec![1, 2]);
    }

    #[tokio::test]
    async fn another_start_error_is_not_retried() {
        let mut calls = 0;
        let r: Result<(), String> = retry_start(|_| {
            calls += 1;
            async { Err("SocketNotFoundError".to_owned()) }
        })
        .await;
        assert_eq!(r, Err("SocketNotFoundError".to_owned()));
        assert_eq!(calls, 1);
    }

    #[tokio::test]
    async fn a_port_that_stays_taken_gives_up_after_the_last_attempt() {
        let mut calls = 0;
        let r: Result<(), String> = retry_start(|_| {
            calls += 1;
            async { Err(TAKEN.to_owned()) }
        })
        .await;
        assert_eq!(r, Err(TAKEN.to_owned()));
        assert_eq!(calls, START_ATTEMPTS);
    }

    // The engine is resolved the way testcontainers 0.27 resolves it, so these mirror its order.
    fn host_of(env: &[(&str, &str)], sockets: &[&str]) -> String {
        docker_host(
            |key| {
                env.iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.to_string())
            },
            |path| sockets.contains(&path),
        )
    }

    #[test]
    fn docker_host_wins_over_every_socket() {
        let env = [("DOCKER_HOST", "unix:///run/user/1000/podman/podman.sock")];
        assert_eq!(
            host_of(&env, &["/var/run/docker.sock"]),
            "unix:///run/user/1000/podman/podman.sock"
        );
        assert_eq!(
            host_of(&[("DOCKER_HOST", "tcp://10.0.0.1:2375")], &[]),
            "tcp://10.0.0.1:2375"
        );
    }

    #[test]
    fn without_docker_host_the_system_docker_socket_is_used() {
        // A GitHub ubuntu runner: no DOCKER_HOST, the Docker daemon on its default socket.
        assert_eq!(
            host_of(&[("HOME", "/home/runner")], &["/var/run/docker.sock"]),
            "unix:///var/run/docker.sock"
        );
    }

    #[test]
    fn rootless_docker_sockets_are_probed_in_order() {
        let env = [("XDG_RUNTIME_DIR", "/run/user/7"), ("HOME", "/home/u")];
        let all = [
            "/run/user/7/.docker/run/docker.sock",
            "/home/u/.docker/run/docker.sock",
            "/home/u/.docker/desktop/docker.sock",
        ];
        assert_eq!(
            host_of(&env, &all),
            "unix:///run/user/7/.docker/run/docker.sock"
        );
        assert_eq!(
            host_of(&env, &all[1..]),
            "unix:///home/u/.docker/run/docker.sock"
        );
        assert_eq!(
            host_of(&env, &all[2..]),
            "unix:///home/u/.docker/desktop/docker.sock"
        );
    }

    #[test]
    fn with_no_socket_found_the_default_is_returned() {
        assert_eq!(host_of(&[], &[]), "unix:///var/run/docker.sock");
    }

    fn labels(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn only_test_containers_of_exited_processes_are_orphans() {
        let containers = vec![
            (
                "dead".to_owned(),
                labels(&[("dbento.role", "test"), ("dbento.pid", "11")]),
            ),
            (
                "live".to_owned(),
                labels(&[("dbento.role", "test"), ("dbento.pid", "22")]),
            ),
            (
                "matrix".to_owned(),
                labels(&[("dbento.role", "matrix"), ("dbento.pid", "11")]),
            ),
            ("nopid".to_owned(), labels(&[("dbento.role", "test")])),
            (
                "badpid".to_owned(),
                labels(&[("dbento.role", "test"), ("dbento.pid", "x")]),
            ),
        ];
        assert_eq!(
            orphans(containers, |pid| pid == 22),
            vec!["dead".to_owned()]
        );
    }

    /// Creates a stopped Postgres container carrying `label` on the engine the cleanup uses,
    /// pulling the image first when the engine lacks it, as a fresh Docker runner does.
    async fn create_labelled(label: &str) {
        use futures_util::TryStreamExt;
        use testcontainers_modules::testcontainers::bollard::models::ContainerCreateBody;
        use testcontainers_modules::testcontainers::bollard::query_parameters::CreateImageOptionsBuilder;

        const IMAGE: &str = "docker.io/library/postgres:17-alpine";
        let docker = engine().expect("reaching the container engine");
        if docker.inspect_image(IMAGE).await.is_err() {
            let pull = CreateImageOptionsBuilder::new().from_image(IMAGE).build();
            docker
                .create_image(Some(pull), None, None)
                .try_collect::<Vec<_>>()
                .await
                .expect("pulling the test image");
        }
        // The role and pid labels let the orphan sweep reclaim it if this test dies mid-way.
        let (key, value) = label.split_once('=').expect("label is key=value");
        let labels = HashMap::from([
            ("dbento.role".to_owned(), "test".to_owned()),
            ("dbento.pid".to_owned(), std::process::id().to_string()),
            (key.to_owned(), value.to_owned()),
        ]);
        let body = ContainerCreateBody {
            image: Some(IMAGE.to_owned()),
            labels: Some(labels),
            ..Default::default()
        };
        docker
            .create_container(None, body)
            .await
            .expect("creating the labelled container");
    }

    #[tokio::test]
    async fn the_container_of_a_failed_attempt_is_removed_by_its_label() {
        let label = format!("{ATTEMPT_LABEL}=unit-{}", std::process::id());
        create_labelled(&label).await;
        assert_eq!(labelled(&label).await.len(), 1);
        remove_labelled(&label).await;
        assert!(labelled(&label).await.is_empty());
    }
}
