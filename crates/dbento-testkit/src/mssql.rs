//! SQL Server harness: every `MsDb` is a fresh database, on its own container or on the server
//! `DBENTO_MSSQL_URL` names. A container runs only once `DBENTO_MSSQL_ACCEPT_EULA=Y` accepts
//! Microsoft's licence; a CI run without that variable skips them, see `skip_without_mssql!`.
//! Mirrors the MySQL harness beside it.

use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};

use testcontainers_modules::mssql_server::MssqlServer;
use testcontainers_modules::testcontainers::{
    ContainerAsync, ContainerRequest, ImageExt, core::Mount, runners::AsyncRunner,
};
use tiberius::{AuthMethod, Client, ColumnData, Config, EncryptionLevel};
use tokio::net::TcpStream;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use super::{
    ATTEMPT_LABEL, NEXT_ATTEMPT, NEXT_DB, ServerTls, engine, remove_labelled, retry_start,
    server_tls, sweep_orphans,
};

/// The environment variable the generated connection reads its password from.
pub const MS_SECRET_VAR: &str = "DBENTO_TEST_MS_SECRET";

/// The gate image's tag: SQL Server 2022 CU27 (16.0.4295.3).
pub const MSSQL_TAG: &str = "2022-CU27-ubuntu-22.04";

/// The test-only `sa` password of every container.
pub const MS_SA_PASSWORD: &str = "DbentoPass2022";

const URL_VAR: &str = "DBENTO_MSSQL_URL";
const EULA_VAR: &str = "DBENTO_MSSQL_ACCEPT_EULA";
const URL_FORM: &str = "mssql://user:password@host:port";

/// Where the SQL Server tests get their server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MsSource {
    /// A running server at this URL.
    Url(String),
    /// A container of the gate image, the licence accepted.
    Container,
}

/// A non-empty `DBENTO_MSSQL_URL` wins; else `DBENTO_MSSQL_ACCEPT_EULA` exactly "Y" means a
/// container; else an error naming both variables.
pub fn ms_source(env: impl Fn(&str) -> Option<String>) -> Result<MsSource, String> {
    if let Some(url) = env(URL_VAR).filter(|u| !u.is_empty()) {
        return Ok(MsSource::Url(url));
    }
    if env(EULA_VAR).as_deref() == Some("Y") {
        return Ok(MsSource::Container);
    }
    Err(format!(
        "the SQL Server tests need a server: set {URL_VAR} to a running server \
         ({URL_FORM}), or set {EULA_VAR}=Y to accept Microsoft's SQL Server licence and run \
         the {MSSQL_TAG} container"
    ))
}

/// Whether a server-backed test skips: only in a CI run with the licence variable unset. Every
/// other combination runs or fails loudly. Like the Makefile, a `ci` that spells false or is
/// blank is no CI run.
pub fn ms_skips(ci: Option<&str>, eula: Option<&str>) -> bool {
    const FALSE: [&str; 5] = ["false", "False", "FALSE", "0", "no"];
    let in_ci = ci.is_some_and(|c| c.split_whitespace().any(|w| !FALSE.contains(&w)));
    in_ci && eula.is_none()
}

/// Why server-backed tests skip under `env`, or `None` when they run or must fail. A server
/// named by URL needs no licence from this run, so it keeps them running.
pub fn ms_skip_reason(env: impl Fn(&str) -> Option<String>) -> Option<String> {
    let skips = ms_skips(env("CI").as_deref(), env(EULA_VAR).as_deref());
    (skips && ms_source(&env).is_err()).then(|| {
        format!(
            "CI is set and {EULA_VAR} is not; Microsoft's SQL Server licence is only accepted \
             by hand, so CI runs no SQL Server"
        )
    })
}

/// Reports a skip for the current test and returns true when [`ms_skip_reason`] gives one.
pub fn ms_skipped() -> bool {
    let Some(why) = ms_skip_reason(|k| std::env::var(k).ok()) else {
        return false;
    };
    let thread = std::thread::current();
    let test = thread.name().unwrap_or("a test");
    // The real stderr, as libtest hides eprintln! for a passing test; one write, because
    // writeln! makes several and parallel tests would splice their lines together.
    let line = format!("dbento-testkit: SKIPPED {test}: {why}\n");
    let _ = std::io::stderr().write_all(line.as_bytes());
    true
}

/// The first line of a test that needs a SQL Server: returns from it when [`ms_skipped`] does.
/// A test without it panics at [`mssql_db`] in that case, so a missed line fails, never passes.
/// It sits in each test because libtest has no run-time skip, and exiting the process from
/// [`mssql_db`] would discard the failures of every other test in the binary.
#[macro_export]
macro_rules! skip_without_mssql {
    () => {
        if $crate::ms_skipped() {
            return;
        }
    };
}

/// Host, port, user and password of `mssql://user:password@host:port[/...]`, taken literally
/// (no percent-decoding).
pub fn ms_url(url: &str) -> Result<(String, u16, String, String), String> {
    let bad = || format!("bad SQL Server test URL: expected {URL_FORM}");
    let rest = url.strip_prefix("mssql://").ok_or_else(bad)?;
    let (login, server) = rest.rsplit_once('@').ok_or_else(bad)?;
    let (user, password) = login.split_once(':').ok_or_else(bad)?;
    let server = server.split('/').next().unwrap_or_default();
    let (host, port) = server.rsplit_once(':').ok_or_else(bad)?;
    let port = port.parse::<u16>().map_err(|_| bad())?;
    if user.is_empty() || host.is_empty() {
        return Err(bad());
    }
    Ok((host.to_owned(), port, user.to_owned(), password.to_owned()))
}

/// A fresh database. One holding a container must be dropped inside a tokio runtime; one on
/// a URL server is dropped from that server when the handle drops.
pub struct MsDb {
    host: String,
    port: u16,
    user: String,
    password: String,
    database: String,
    // Declared before the permit, so the container is gone before another may start.
    container: Option<ContainerAsync<MssqlServer>>,
    _permit: Option<OwnedSemaphorePermit>,
}

impl MsDb {
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
    /// The login password.
    pub fn password(&self) -> &str {
        &self.password
    }
    /// This test database's name, `dbento_t_<pid>_<n>`.
    pub fn database(&self) -> &str {
        &self.database
    }

    /// `[connections.<name>]` with engine "mssql", host, port, database, user,
    /// `secret = { env = MS_SECRET_VAR }`, then `extra` on its own line.
    pub fn connection_toml(&self, name: &str, extra: &str) -> String {
        format!(
            "[connections.{name}]\nengine = \"mssql\"\nhost = \"{}\"\nport = {}\ndatabase = \"{}\"\nuser = \"{}\"\nsecret = {{ env = \"{MS_SECRET_VAR}\" }}\n{extra}\n",
            self.host, self.port, self.database, self.user
        )
    }

    /// A client on this database: encryption required, the certificate trusted, no command
    /// timeout.
    pub async fn client(&self) -> Client<Compat<TcpStream>> {
        connect(self.config(&self.database)).await
    }

    /// The first cell of the first row as text; `None` for no row or NULL. Panics on a type
    /// other than the string, integer and bit types.
    pub async fn scalar(&self, sql: &str) -> Option<String> {
        let mut client = self.client().await;
        let row = client
            .simple_query(sql)
            .await
            .expect("scalar query failed")
            .into_row()
            .await
            .expect("scalar query failed")?;
        let cell = row.into_iter().next()?;
        match cell {
            ColumnData::String(v) => v.map(|s| s.into_owned()),
            ColumnData::U8(v) => v.map(|n| n.to_string()),
            ColumnData::I16(v) => v.map(|n| n.to_string()),
            ColumnData::I32(v) => v.map(|n| n.to_string()),
            ColumnData::I64(v) => v.map(|n| n.to_string()),
            ColumnData::Bit(v) => v.map(|b| u8::from(b).to_string()),
            other => panic!("scalar cannot render {other:?}"),
        }
    }

    fn config(&self, database: &str) -> Config {
        config(&self.host, self.port, &self.user, &self.password, database)
    }
}

impl Drop for MsDb {
    // Both paths run on their own thread and runtime and never panic. A container is killed
    // first, since SQL Server ignores SIGTERM and a plain remove waits out the stop timeout;
    // its databases go with it. On a URL server the database is dropped.
    fn drop(&mut self) {
        let container = self.container.as_ref().map(|c| c.id().to_owned());
        let config = self.config("master");
        let name = self.database.clone();
        let done = std::thread::spawn(move || -> Result<(), String> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            runtime.block_on(async {
                if let Some(id) = container {
                    return engine()?
                        .kill_container(&id, None)
                        .await
                        .map_err(|e| format!("cannot kill container {id}: {e}"));
                }
                let mut client = try_connect(config).await?;
                let sql = format!(
                    "ALTER DATABASE [{name}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; \
                     DROP DATABASE [{name}]"
                );
                client
                    .simple_query(sql)
                    .await
                    .map_err(|e| e.to_string())?
                    .into_results()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(())
            })
        })
        .join();
        match done {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("dbento-testkit: cannot drop {}: {e}", self.database),
            Err(_) => eprintln!("dbento-testkit: dropping {} panicked", self.database),
        }
    }
}

fn config(host: &str, port: u16, user: &str, password: &str, database: &str) -> Config {
    let mut config = Config::new();
    config.host(host);
    config.port(port);
    config.database(database);
    config.authentication(AuthMethod::sql_server(user, password));
    config.encryption(EncryptionLevel::Required);
    config.trust_cert();
    config.command_timeout(None);
    config
}

async fn try_connect(config: Config) -> Result<Client<Compat<TcpStream>>, String> {
    let tcp = TcpStream::connect(config.get_addr())
        .await
        .map_err(|e| e.to_string())?;
    tcp.set_nodelay(true).map_err(|e| e.to_string())?;
    Client::connect(config, tcp.compat_write())
        .await
        .map_err(|e| e.to_string())
}

async fn connect(config: Config) -> Client<Compat<TcpStream>> {
    try_connect(config)
        .await
        .unwrap_or_else(|e| panic!("cannot connect to the SQL Server test server: {e}"))
}

/// A fresh database on the server [`ms_source`] picks; panics with its error text.
pub async fn mssql_db() -> MsDb {
    match ms_source(|k| std::env::var(k).ok()) {
        Ok(MsSource::Url(url)) => {
            let (host, port, user, password) = ms_url(&url).unwrap_or_else(|e| panic!("{e}"));
            fresh_database(&host, port, &user, &password).await
        }
        Ok(MsSource::Container) => container(None).await,
        Err(e) => panic!("{e}"),
    }
}

/// A fresh container, `DBENTO_MSSQL_URL` ignored, serving TLS with a "localhost" certificate
/// from a fresh CA. The `MsDb` is on host "localhost"; the string is the CA's PEM.
pub async fn mssql_tls() -> (MsDb, String) {
    let source = ms_source(|k| (k != URL_VAR).then(|| std::env::var(k).ok()).flatten());
    if let Err(e) = source {
        panic!("{e}");
    }
    let tls = server_tls();
    let db = container(Some(&tls)).await;
    (db, tls.ca_pem)
}

/// One SQL Server container at a time in this process; each takes 3 GB.
fn slots() -> Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    Arc::clone(SLOTS.get_or_init(|| Arc::new(Semaphore::new(1))))
}

/// Copies the certificate, its key and an mssql.conf naming them in before first start, so
/// the server loads them without a restart.
fn serve_tls(
    request: ContainerRequest<MssqlServer>,
    tls: Option<&ServerTls>,
) -> ContainerRequest<MssqlServer> {
    let Some(tls) = tls else { return request };
    request
        .with_copy_to(
            "/var/opt/mssql/certs/server.pem",
            tls.cert_pem.clone().into_bytes(),
        )
        .with_copy_to(
            "/var/opt/mssql/certs/server.key",
            tls.key_pem.clone().into_bytes(),
        )
        .with_copy_to(
            "/var/opt/mssql/mssql.conf",
            b"[network]\ntlscert = /var/opt/mssql/certs/server.pem\ntlskey = /var/opt/mssql/certs/server.key\n"
                .to_vec(),
        )
}

/// A fresh container; with `tls`, serving it and reached through "localhost".
async fn container(tls: Option<&ServerTls>) -> MsDb {
    let permit = slots()
        .acquire_owned()
        .await
        .expect("the container semaphore is never closed");
    sweep_orphans().await;
    let c = start(|r| serve_tls(r, tls)).await;
    let port = c
        .get_host_port_ipv4(1433)
        .await
        .expect("no mapped SQL Server port");
    let host = if tls.is_some() {
        "localhost"
    } else {
        "127.0.0.1"
    };
    let mut db = fresh_database(host, port, "sa", MS_SA_PASSWORD).await;
    db.container = Some(c);
    db._permit = Some(permit);
    db
}

/// Starts the gate image labelled, memory-capped and on tmpfs, shaped by `with`, and retried
/// when its host port turns out to be taken.
async fn start(
    with: impl Fn(ContainerRequest<MssqlServer>) -> ContainerRequest<MssqlServer>,
) -> ContainerAsync<MssqlServer> {
    retry_start(|_| {
        let attempt = format!(
            "{}-{}",
            std::process::id(),
            NEXT_ATTEMPT.fetch_add(1, Ordering::Relaxed)
        );
        let request = MssqlServer::default()
            .with_accept_eula()
            .with_sa_password(MS_SA_PASSWORD)
            .with_tag(MSSQL_TAG)
            .with_env_var("MSSQL_MEMORY_LIMIT_MB", "2048")
            .with_mount(Mount::tmpfs_mount("/var/opt/mssql/data").with_mode(0o1777))
            .with_host_config_modifier(|h| h.memory = Some(3 << 30))
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
    .unwrap_or_else(|e| {
        panic!(
            "cannot start the SQL Server test container: {e}\n\
             Rootless podman: set DOCKER_HOST=unix://$XDG_RUNTIME_DIR/podman/podman.sock\n\
             Or skip containers and point {URL_VAR} at a running server."
        )
    })
}

/// Creates `dbento_t_<pid>_<n>` from a `master` login.
async fn fresh_database(host: &str, port: u16, user: &str, password: &str) -> MsDb {
    let name = format!(
        "dbento_t_{}_{}",
        std::process::id(),
        NEXT_DB.fetch_add(1, Ordering::Relaxed)
    );
    let mut admin = connect(config(host, port, user, password, "master")).await;
    admin
        .simple_query(format!(
            "DROP DATABASE IF EXISTS [{name}]; CREATE DATABASE [{name}]"
        ))
        .await
        .expect("create database")
        .into_results()
        .await
        .expect("create database");
    MsDb {
        host: host.to_owned(),
        port,
        user: user.to_owned(),
        password: password.to_owned(),
        database: name,
        container: None,
        _permit: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    // Anything but a real CI with the licence variable truly unset must fail loudly, so a
    // forgotten env file can never read as a pass.
    #[test]
    fn only_a_ci_run_with_the_licence_variable_unset_skips() {
        let unset = [
            "false", "False", "FALSE", "0", "no", " false ", "no\n", "", " ",
        ];
        let set = ["true", "1", "yes", " true "];
        for ci in std::iter::once(None).chain(unset.iter().chain(&set).map(|c| Some(*c))) {
            for eula in [None, Some(""), Some("N"), Some("y"), Some("Y")] {
                let in_ci = ci.is_some_and(|c| set.contains(&c));
                assert_eq!(
                    ms_skips(ci, eula),
                    in_ci && eula.is_none(),
                    "{ci:?} {eula:?}"
                );
            }
        }
    }

    #[test]
    fn the_skip_says_why_and_a_url_keeps_the_tests_running() {
        let m = ms_skip_reason(env(&[("CI", "true")])).unwrap();
        assert!(
            m.contains("CI") && m.contains("DBENTO_MSSQL_ACCEPT_EULA"),
            "{m}"
        );
        assert!(m.contains("licen"), "{m}");
        let url = ("DBENTO_MSSQL_URL", "mssql://sa:pw@127.0.0.1:1433");
        assert_eq!(ms_skip_reason(env(&[("CI", "true"), url])), None);
        assert_eq!(ms_skip_reason(env(&[])), None);
        assert_eq!(
            ms_skip_reason(env(&[("CI", "true"), ("DBENTO_MSSQL_ACCEPT_EULA", "Y")])),
            None
        );
    }

    #[test]
    fn a_url_wins_then_only_an_exact_y_accepts_the_licence() {
        let url = "mssql://sa:pw@127.0.0.1:1433";
        assert_eq!(
            ms_source(env(&[
                ("DBENTO_MSSQL_URL", url),
                ("DBENTO_MSSQL_ACCEPT_EULA", "Y")
            ])),
            Ok(MsSource::Url(url.to_owned()))
        );
        assert_eq!(
            ms_source(env(&[
                ("DBENTO_MSSQL_URL", ""),
                ("DBENTO_MSSQL_ACCEPT_EULA", "Y")
            ])),
            Ok(MsSource::Container)
        );
        for refused in [
            &[][..],
            &[("DBENTO_MSSQL_ACCEPT_EULA", "y")][..],
            &[("DBENTO_MSSQL_ACCEPT_EULA", "yes")][..],
        ] {
            let m = ms_source(env(refused)).unwrap_err();
            assert!(
                m.contains("DBENTO_MSSQL_URL") && m.contains("DBENTO_MSSQL_ACCEPT_EULA=Y"),
                "{m}"
            );
            assert!(m.contains("licen"), "{m}");
        }
    }

    #[test]
    fn a_url_gives_host_port_user_and_password() {
        assert_eq!(
            ms_url("mssql://sa:DbentoPass2022@127.0.0.1:41433/").unwrap(),
            (
                "127.0.0.1".to_owned(),
                41433,
                "sa".to_owned(),
                "DbentoPass2022".to_owned()
            )
        );
        for bad in [
            "postgres://a:b@h:1",
            "mssql://sa@h:1",
            "mssql://sa:pw@h",
            "mssql://sa:pw@h:x",
        ] {
            assert!(
                ms_url(bad)
                    .unwrap_err()
                    .contains("mssql://user:password@host:port"),
                "{bad}"
            );
        }
    }
}
