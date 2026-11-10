//! MySQL and MariaDB harness: every `MyDb` is a fresh `utf8mb4` database, on its own container
//! or on the server a URL variable names. Mirrors the Postgres harness in the crate root.

use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};

use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Opts, OptsBuilder, Row};
use testcontainers_modules::mariadb::Mariadb;
use testcontainers_modules::mysql::Mysql;
use testcontainers_modules::testcontainers::{
    ContainerAsync, ContainerRequest, Image, ImageExt, core::Mount, runners::AsyncRunner,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::{
    ATTEMPT_LABEL, CERT_USER, ClientCert, NEXT_ATTEMPT, NEXT_DB, ServerTls, client_cert,
    remove_labelled, retry_start, server_tls, sweep_orphans,
};

/// The environment variable the generated connection reads its password from.
pub const MY_SECRET_VAR: &str = "DBENTO_TEST_MY_SECRET";

/// The MySQL versions the opt-in matrix covers.
pub const MYSQL_MATRIX: [&str; 2] = ["8.4", "9.7"];

/// The MariaDB versions the opt-in matrix covers.
pub const MARIADB_MATRIX: [&str; 3] = ["10.11", "11.4", "12.3"];

/// 1,000 rows, n = 0..=999; `nums a, nums b` gives 1e6 rows and three copies 1e9.
pub const NUMS_SQL: &str = "CREATE TABLE nums (n INT PRIMARY KEY) AS WITH RECURSIVE r(n) AS \
    (SELECT 0 UNION ALL SELECT n + 1 FROM r WHERE n < 999) SELECT n FROM r";

/// Which server of the MySQL family a database lives on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MyFlavor {
    /// Oracle MySQL.
    Mysql,
    /// MariaDB.
    Mariadb,
}

// One field per image type, so the handle can hold either container.
enum Held {
    Mysql(#[allow(dead_code)] ContainerAsync<Mysql>),
    Mariadb(#[allow(dead_code)] ContainerAsync<Mariadb>),
}

/// A fresh database. One holding a container must be dropped inside a tokio runtime; one on
/// a URL server is dropped from that server when the handle drops.
pub struct MyDb {
    host: String,
    port: u16,
    user: String,
    password: String,
    database: String,
    flavor: MyFlavor,
    // Declared before the permit, so the container is gone before another may start.
    container: Option<Held>,
    _permit: Option<OwnedSemaphorePermit>,
}

impl MyDb {
    /// The server's host.
    pub fn host(&self) -> &str {
        &self.host
    }
    /// The server's TCP port.
    pub fn port(&self) -> u16 {
        self.port
    }
    /// The login user.
    pub fn user(&self) -> &str {
        &self.user
    }
    /// The login password, empty on a container.
    pub fn password(&self) -> &str {
        &self.password
    }
    /// This test database's name, `dbento_t_<pid>_<n>`.
    pub fn database(&self) -> &str {
        &self.database
    }
    /// Whether the server is MySQL or MariaDB, from its `VERSION()`.
    pub fn flavor(&self) -> MyFlavor {
        self.flavor
    }

    /// `[connections.<name>]` with engine "mysql", host, port, database, user,
    /// `secret = { env = MY_SECRET_VAR }`, then `extra` on its own line.
    pub fn connection_toml(&self, name: &str, extra: &str) -> String {
        format!(
            "[connections.{name}]\nengine = \"mysql\"\nhost = \"{}\"\nport = {}\ndatabase = \"{}\"\nuser = \"{}\"\nsecret = {{ env = \"{MY_SECRET_VAR}\" }}\n{extra}\n",
            self.host, self.port, self.database, self.user
        )
    }

    /// A plain connection to this database.
    pub async fn admin(&self) -> Conn {
        connect(&self.server_opts().db_name(Some(&self.database))).await
    }

    /// The first cell of the first row, as text; `None` for no row or NULL.
    pub async fn scalar(&self, sql: &str) -> Option<String> {
        let mut conn = self.admin().await;
        let row: Option<Row> = conn.query_first(sql).await.expect("scalar query failed");
        let _ = conn.disconnect().await;
        row?.get::<Option<String>, usize>(0).flatten()
    }

    fn server_opts(&self) -> OptsBuilder {
        server_opts(&self.host, self.port, &self.user, &self.password)
    }
}

impl Drop for MyDb {
    // A container takes its databases with it. On a URL server the drop runs on its own thread
    // and runtime, as `PgDb::drop` does, and never panics.
    fn drop(&mut self) {
        if self.container.is_some() {
            return;
        }
        let opts = self.server_opts();
        let name = self.database.clone();
        let dropped = std::thread::spawn(move || -> Result<(), String> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            runtime.block_on(async {
                let mut conn = Conn::new(opts).await.map_err(|e| e.to_string())?;
                // A session left holding a lock on the database makes the drop fail, not hang.
                conn.query_drop("SET SESSION lock_wait_timeout = 5")
                    .await
                    .map_err(|e| e.to_string())?;
                conn.query_drop(format!("DROP DATABASE IF EXISTS `{name}`"))
                    .await
                    .map_err(|e| e.to_string())?;
                conn.disconnect().await.map_err(|e| e.to_string())
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

/// Server-level options. `prefer_socket(false)` keeps a loopback login on TCP instead of
/// re-opening through the server's `@@socket` path.
fn server_opts(host: &str, port: u16, user: &str, password: &str) -> OptsBuilder {
    OptsBuilder::default()
        .ip_or_hostname(host)
        .tcp_port(port)
        .user(Some(user))
        .pass(Some(password))
        .prefer_socket(false)
}

async fn connect(opts: &OptsBuilder) -> Conn {
    Conn::new(opts.clone())
        .await
        .unwrap_or_else(|e| panic!("cannot connect to the MySQL test server: {e}"))
}

/// `DBENTO_MYSQL_URL` when set, otherwise a new `mysql:8.4` container.
pub async fn mysql_db() -> MyDb {
    match std::env::var("DBENTO_MYSQL_URL") {
        Ok(url) if !url.is_empty() => my_db_at(&url).await,
        _ => container(MyFlavor::Mysql, None).await,
    }
}

/// `DBENTO_MARIADB_URL` when set, otherwise a new `mariadb:11.4` container.
pub async fn mariadb_db() -> MyDb {
    match std::env::var("DBENTO_MARIADB_URL") {
        Ok(url) if !url.is_empty() => my_db_at(&url).await,
        _ => container(MyFlavor::Mariadb, None).await,
    }
}

/// ("mysql84", url) for each non-empty DBENTO_MYSQL_<v without dot>_URL, then ("mariadb114", ...).
pub fn my_matrix_urls(env: impl Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    let versions = MYSQL_MATRIX
        .iter()
        .map(|v| ("mysql", v))
        .chain(MARIADB_MATRIX.iter().map(|v| ("mariadb", v)));
    versions
        .filter_map(|(flavor, v)| {
            let v = v.replace('.', "");
            let var = format!("DBENTO_{}_{v}_URL", flavor.to_uppercase());
            let url = env(&var).filter(|u| !u.is_empty())?;
            Some((format!("{flavor}{v}"), url))
        })
        .collect()
}

/// A fresh database on each matrix server; with none set, [("mysql", ..), ("mariadb", ..)].
pub async fn my_matrix() -> Vec<(String, MyDb)> {
    let urls = my_matrix_urls(|k| std::env::var(k).ok());
    if urls.is_empty() {
        return vec![
            ("mysql".to_owned(), mysql_db().await),
            ("mariadb".to_owned(), mariadb_db().await),
        ];
    }
    let mut out = Vec::new();
    for (label, url) in urls {
        out.push((label, my_db_at(&url).await));
    }
    out
}

/// A fresh database on the server at `url`, dropped when the handle drops.
async fn my_db_at(url: &str) -> MyDb {
    let opts = Opts::from_url(url).unwrap_or_else(|e| panic!("bad MySQL test URL: {e}"));
    fresh_database(
        opts.ip_or_hostname(),
        opts.tcp_port(),
        opts.user().unwrap_or("root"),
        opts.pass().unwrap_or_default(),
    )
    .await
}

/// Containers of one flavor allowed at once in this process; each MySQL server takes most of
/// a gigabyte.
fn slots(flavor: MyFlavor) -> Arc<Semaphore> {
    static MYSQL: OnceLock<Arc<Semaphore>> = OnceLock::new();
    static MARIADB: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let cell = match flavor {
        MyFlavor::Mysql => &MYSQL,
        MyFlavor::Mariadb => &MARIADB,
    };
    Arc::clone(cell.get_or_init(|| Arc::new(Semaphore::new(2))))
}

/// A fresh container of `flavor`, URL variables ignored, serving TLS with a "localhost"
/// certificate from a fresh CA. The `MyDb` is on host "localhost", and [`CERT_USER`] logs in
/// only with the returned client certificate, holding every privilege on the database.
pub async fn my_tls(flavor: MyFlavor) -> (MyDb, ClientCert) {
    let tls = server_tls();
    let client = client_cert(&tls);
    let db = container(flavor, Some(&tls)).await;
    let mut admin = db.admin().await;
    let user = format!("'{CERT_USER}'@'%'");
    for sql in [
        format!("CREATE USER {user} REQUIRE X509"),
        format!("GRANT ALL PRIVILEGES ON `{}`.* TO {user}", db.database),
    ] {
        admin
            .query_drop(sql)
            .await
            .expect("create the certificate user");
    }
    let _ = admin.disconnect().await;
    (db, client)
}

/// Mounts the server certificate, its key and the CA, and starts the server with them; both
/// entrypoints put the server binary in front of `-` arguments.
fn serve_tls<I: Image>(
    request: ContainerRequest<I>,
    tls: Option<&ServerTls>,
) -> ContainerRequest<I> {
    let Some(tls) = tls else { return request };
    request
        .with_copy_to("/etc/dbento-tls/ca.pem", tls.ca_pem.clone().into_bytes())
        .with_copy_to(
            "/etc/dbento-tls/server.pem",
            tls.cert_pem.clone().into_bytes(),
        )
        .with_copy_to(
            "/etc/dbento-tls/server-key.pem",
            tls.key_pem.clone().into_bytes(),
        )
        .with_cmd([
            "--ssl-ca=/etc/dbento-tls/ca.pem",
            "--ssl-cert=/etc/dbento-tls/server.pem",
            "--ssl-key=/etc/dbento-tls/server-key.pem",
        ])
}

/// A fresh container; with `tls`, serving it and reached through "localhost", which its
/// certificate names.
async fn container(flavor: MyFlavor, tls: Option<&ServerTls>) -> MyDb {
    let permit = slots(flavor)
        .acquire_owned()
        .await
        .expect("the container semaphore is never closed");
    sweep_orphans().await;
    let (held, port) = match flavor {
        MyFlavor::Mysql => {
            let c = start::<Mysql>("8.4", flavor, |r| serve_tls(r, tls)).await;
            let port = c.get_host_port_ipv4(3306).await;
            (Held::Mysql(c), port)
        }
        MyFlavor::Mariadb => {
            let c = start::<Mariadb>("11.4", flavor, |r| serve_tls(r, tls)).await;
            let port = c.get_host_port_ipv4(3306).await;
            (Held::Mariadb(c), port)
        }
    };
    let port = port.expect("no mapped MySQL port");
    let host = if tls.is_some() {
        "localhost"
    } else {
        "127.0.0.1"
    };
    let mut db = fresh_database(host, port, "root", "").await;
    db.container = Some(held);
    db._permit = Some(permit);
    db
}

/// Starts the module image at `tag`, labelled and on tmpfs like the Postgres containers, then
/// shaped by `with`, and retried when its host port turns out to be taken.
async fn start<I>(
    tag: &str,
    flavor: MyFlavor,
    with: impl Fn(ContainerRequest<I>) -> ContainerRequest<I>,
) -> ContainerAsync<I>
where
    I: Image + Default + Send + Sync + 'static,
{
    retry_start(|_| {
        let attempt = format!(
            "{}-{}",
            std::process::id(),
            NEXT_ATTEMPT.fetch_add(1, Ordering::Relaxed)
        );
        let request = I::default()
            .with_tag(tag)
            // A tmpfs over the image's VOLUME keeps an anonymous volume from outliving a kill.
            .with_mount(Mount::tmpfs_mount("/var/lib/mysql"))
            .with_label("dbento.role", "test")
            .with_label("dbento.pid", std::process::id().to_string())
            .with_label(ATTEMPT_LABEL, attempt.clone());
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
    .unwrap_or_else(|e| panic!("{}", start_hint(flavor, &e)))
}

/// The panic text when a container cannot start.
fn start_hint(flavor: MyFlavor, error: &str) -> String {
    let (name, var) = match flavor {
        MyFlavor::Mysql => ("MySQL", "DBENTO_MYSQL_URL"),
        MyFlavor::Mariadb => ("MariaDB", "DBENTO_MARIADB_URL"),
    };
    format!(
        "cannot start the {name} test container: {error}\n\
         Rootless podman: set DOCKER_HOST=unix://$XDG_RUNTIME_DIR/podman/podman.sock\n\
         Or skip containers and point {var} at a running server."
    )
}

/// The server-wide `GET_LOCK` name every sweep holds while it lists and drops.
pub const MY_SWEEP_LOCK: &str = "dbento_testkit_sweep";

/// Drops test databases on the server at `url` left by runs that are gone: the pid in the
/// name is not this process and not alive, and no session uses the database. Errors are
/// ignored. Liveness is judged by pids on this host, so a run elsewhere looks dead. Linux
/// only, since liveness is read from /proc; elsewhere it does nothing.
pub async fn my_sweep_stale(url: &str) {
    let Ok(opts) = Opts::from_url(url) else {
        return;
    };
    sweep(&OptsBuilder::from_opts(opts).prefer_socket(false)).await;
}

#[cfg(not(target_os = "linux"))]
async fn sweep(_opts: &OptsBuilder) {}

#[cfg(target_os = "linux")]
async fn sweep(opts: &OptsBuilder) {
    let Ok(mut conn) = Conn::new(opts.clone()).await else {
        return;
    };
    // Sweeps run one at a time per server, so a test holding the lock can stage a database
    // that no sweep may touch until it has a session; a sweep that cannot get it skips.
    let locked: Option<Option<i64>> = conn
        .exec_first("SELECT GET_LOCK(?, 30)", (MY_SWEEP_LOCK,))
        .await
        .unwrap_or_default();
    // A stray metadata lock makes a drop fail fast instead of stalling every first fixture.
    if locked.flatten() == Some(1)
        && conn
            .query_drop("SET SESSION lock_wait_timeout = 5")
            .await
            .is_ok()
    {
        drop_dead_runs(&mut conn).await;
    }
    let _ = conn.disconnect().await;
}

/// Drops each `dbento_t_<pid>_...` database whose pid is dead and that no session uses.
#[cfg(target_os = "linux")]
async fn drop_dead_runs(conn: &mut Conn) {
    let Ok(names) = conn
        .query::<String, _>(
            "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA \
             WHERE SCHEMA_NAME LIKE 'dbento\\_t\\_%' AND SCHEMA_NAME NOT IN \
             (SELECT DB FROM information_schema.PROCESSLIST WHERE DB IS NOT NULL)",
        )
        .await
    else {
        return;
    };
    for name in names {
        let pid = name
            .strip_prefix("dbento_t_")
            .and_then(|rest| rest.split('_').next())
            .and_then(|pid| pid.parse::<u32>().ok());
        let Some(pid) = pid else { continue };
        if pid == std::process::id() || std::path::Path::new(&format!("/proc/{pid}")).exists() {
            continue;
        }
        let quoted = name.replace('`', "``");
        let _ = conn
            .query_drop(format!("DROP DATABASE IF EXISTS `{quoted}`"))
            .await;
    }
}

/// Servers already swept by this process, so each is swept once, before its first database.
static SWEPT: std::sync::Mutex<Vec<(String, u16)>> = std::sync::Mutex::new(Vec::new());

/// Creates `dbento_t_<pid>_<n>` in utf8mb4 from a server-level login and reads the flavor.
async fn fresh_database(host: &str, port: u16, user: &str, password: &str) -> MyDb {
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
        sweep(&server_opts(host, port, user, password)).await;
    }
    let name = format!(
        "dbento_t_{}_{}",
        std::process::id(),
        NEXT_DB.fetch_add(1, Ordering::Relaxed)
    );
    let mut admin = connect(&server_opts(host, port, user, password)).await;
    let version: Option<String> = admin
        .query_first("SELECT VERSION()")
        .await
        .expect("SELECT VERSION()");
    let flavor = if version.unwrap_or_default().contains("MariaDB") {
        MyFlavor::Mariadb
    } else {
        MyFlavor::Mysql
    };
    admin
        .query_drop(format!("DROP DATABASE IF EXISTS `{name}`"))
        .await
        .expect("drop database");
    admin
        .query_drop(format!("CREATE DATABASE `{name}` CHARACTER SET utf8mb4"))
        .await
        .expect("create database");
    let _ = admin.disconnect().await;
    MyDb {
        host: host.to_owned(),
        port,
        user: user.to_owned(),
        password: password.to_owned(),
        database: name,
        flavor,
        container: None,
        _permit: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_urls_read_each_versions_variable_in_order() {
        let env = |k: &str| match k {
            "DBENTO_MARIADB_114_URL" => Some("mysql://root@h:2/".to_owned()),
            "DBENTO_MYSQL_84_URL" => Some("mysql://root@h:1/".to_owned()),
            "DBENTO_MYSQL_97_URL" => Some(String::new()),
            _ => None,
        };
        assert_eq!(
            my_matrix_urls(env),
            vec![
                ("mysql84".to_owned(), "mysql://root@h:1/".to_owned()),
                ("mariadb114".to_owned(), "mysql://root@h:2/".to_owned()),
            ]
        );
    }
}
