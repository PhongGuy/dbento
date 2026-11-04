use dbento_testkit::{
    chinook_pg_sql, matrix_urls, pg_container, pg_db, pg_db_at, start_hint, sweep_stale,
};
use std::time::Duration;

#[tokio::test]
async fn each_database_is_fresh_and_isolated() {
    let a = pg_db().await;
    a.admin()
        .await
        .batch_execute("CREATE TABLE marker(x int)")
        .await
        .unwrap();
    let b = pg_db_at(&a.server_url()).await;
    assert_ne!(a.database(), b.database());
    let marker = "SELECT count(*) FROM pg_tables WHERE tablename = 'marker'";
    assert_eq!(a.scalar(marker).await.as_deref(), Some("1"));
    assert_eq!(b.scalar(marker).await.as_deref(), Some("0"));
    assert_eq!(
        b.scalar("SELECT current_database()").await.as_deref(),
        Some(b.database())
    );
}

#[tokio::test]
async fn a_container_runs_postgres_17_and_is_removed_on_drop() {
    let db = pg_container().await;
    let major = "SELECT current_setting('server_version_num')::int / 10000";
    assert_eq!(db.scalar(major).await.as_deref(), Some("17"));
    let addr = (db.host().to_owned(), db.port());
    drop(db);
    assert!(
        std::net::TcpStream::connect(addr).is_err(),
        "the port still answers after drop"
    );
}

#[test]
fn a_start_failure_names_the_podman_socket_and_the_override() {
    let msg = start_hint("SocketNotFoundError(\"/var/run/docker.sock\")");
    assert!(msg.contains("SocketNotFoundError"), "{msg}");
    assert!(
        msg.contains("DOCKER_HOST=unix://$XDG_RUNTIME_DIR/podman/podman.sock"),
        "{msg}"
    );
    assert!(msg.contains("DBENTO_PG_URL"), "{msg}");
}

#[test]
fn matrix_urls_follow_version_order_and_skip_empty_values() {
    let env = |k: &str| match k {
        "DBENTO_PG_18_URL" => Some("postgres://b".to_owned()),
        "DBENTO_PG_14_URL" => Some("postgres://a".to_owned()),
        "DBENTO_PG_16_URL" => Some(String::new()),
        _ => None,
    };
    assert_eq!(
        matrix_urls(env),
        vec![
            ("pg14".to_owned(), "postgres://a".to_owned()),
            ("pg18".to_owned(), "postgres://b".to_owned())
        ]
    );
    assert!(matrix_urls(|_| None).is_empty());
}

#[test]
fn chinook_pg_sql_starts_after_the_database_switch() {
    let sql = chinook_pg_sql();
    assert!(!sql.contains("\\c chinook") && !sql.contains("CREATE DATABASE"));
    assert!(sql.contains("CREATE TABLE album"));
}

#[tokio::test]
async fn a_listener_whose_connection_died_fails_instead_of_hearing_nothing() {
    use futures_util::FutureExt;
    let db = pg_db().await;
    let mut listener = db.listen("dead_end").await;
    let killed = db
        .scalar(
            "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity \
             WHERE datname = current_database() AND query = 'LISTEN dead_end'",
        )
        .await;
    assert_eq!(killed.as_deref(), Some("1"));
    let heard = std::panic::AssertUnwindSafe(listener.received(Duration::from_millis(500)))
        .catch_unwind()
        .await;
    let panic = heard.expect_err("received() returned although the listener is dead");
    let text = panic.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(text.contains("listener connection"), "{text}");
}

/// The shared server the drop and sweep tests use, or None (with the reason printed) to skip:
/// container-backed databases are not affected by either.
fn shared_url() -> Option<String> {
    let url = std::env::var("DBENTO_PG_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .or_else(|| {
            matrix_urls(|k| std::env::var(k).ok())
                .into_iter()
                .next()
                .map(|(_, u)| u)
        });
    if url.is_none() {
        eprintln!("skipped: neither DBENTO_PG_URL nor a DBENTO_PG_<NN>_URL is set");
    }
    url
}

/// How many databases named `name` the server at `url` has, over a separate connection.
async fn database_count(url: &str, name: &str) -> i64 {
    let (client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls)
        .await
        .expect("connect to the shared server");
    tokio::spawn(connection);
    client
        .query_one(
            "SELECT count(*) FROM pg_database WHERE datname = $1",
            &[&name],
        )
        .await
        .expect("count databases")
        .get(0)
}

#[tokio::test]
async fn a_shared_server_database_is_dropped_when_its_handle_drops() {
    let Some(url) = shared_url() else { return };
    let db = pg_db_at(&url).await;
    let name = db.database().to_owned();
    let open = db.admin().await;
    open.batch_execute("CREATE TABLE kept(x int)")
        .await
        .unwrap();
    assert_eq!(database_count(&url, &name).await, 1);
    drop(db);
    assert_eq!(
        database_count(&url, &name).await,
        0,
        "{name} outlived its handle"
    );
    drop(open);
}

#[tokio::test]
async fn a_panicking_test_still_drops_its_database() {
    let Some(url) = shared_url() else { return };
    let (tx, rx) = std::sync::mpsc::channel();
    let thread_url = url.clone();
    let outcome = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let db = pg_db_at(&thread_url).await;
            tx.send(db.database().to_owned()).unwrap();
            panic!("the test body fails while holding {}", db.database());
        });
    })
    .join();
    assert!(outcome.is_err(), "the test body did not panic");
    let name = rx.recv().expect("the database name");
    assert_eq!(
        database_count(&url, &name).await,
        0,
        "{name} outlived a panic"
    );
}

/// A connection to database `name` on the server at `url`.
async fn connect_db(url: &str, name: &str) -> tokio_postgres::Client {
    let mut config: tokio_postgres::Config = url.parse().expect("server URL");
    let (client, connection) = config
        .dbname(name)
        .connect(tokio_postgres::NoTls)
        .await
        .expect("connect");
    tokio::spawn(connection);
    client
}

/// Creates a database named for a run whose pid cannot be alive; `tag` keeps it unique to
/// this process and test, so concurrent sweeps can only drop it, never mistake it. `options`
/// are appended to the CREATE DATABASE statement.
async fn dead_runs_database(url: &str, tag: &str, options: &str) -> String {
    let pid_max: u64 = std::fs::read_to_string("/proc/sys/kernel/pid_max")
        .expect("pid_max")
        .trim()
        .parse()
        .expect("pid_max is a number");
    let name = format!("dbento_t_{}_{}{tag}", pid_max + 1, std::process::id());
    connect_db(url, "postgres")
        .await
        .batch_execute(&format!("CREATE DATABASE {name} {options}"))
        .await
        .expect("create the dead run's database");
    name
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_sweep_drops_a_dead_runs_database() {
    let Some(url) = shared_url() else { return };
    let name = dead_runs_database(&url, "dead", "").await;
    sweep_stale(&url).await;
    assert_eq!(
        database_count(&url, &name).await,
        0,
        "{name} survived the sweep"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_sweep_keeps_a_database_with_a_session() {
    let Some(url) = shared_url() else { return };
    // Postgres refuses to drop a template database, so no concurrent sweep (another test's,
    // the first-use one, or another process's) can drop it before the session holds it.
    let name = dead_runs_database(&url, "busy", "IS_TEMPLATE true").await;
    let session = connect_db(&url, &name).await;
    connect_db(&url, "postgres")
        .await
        .batch_execute(&format!("ALTER DATABASE {name} IS_TEMPLATE false"))
        .await
        .expect("unmark the busy database as a template");
    sweep_stale(&url).await;
    let survived = database_count(&url, &name).await;
    drop(session);
    connect_db(&url, "postgres")
        .await
        .batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
        .await
        .expect("drop the busy database");
    assert_eq!(
        survived, 1,
        "the sweep dropped {name} while a session held it"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_sweep_keeps_this_process_s_databases() {
    let Some(url) = shared_url() else { return };
    let db = pg_db_at(&url).await;
    sweep_stale(&url).await;
    assert_eq!(database_count(&url, db.database()).await, 1);
}
