//! The stale-database sweep on the shared MySQL and MariaDB servers.
#![cfg(target_os = "linux")]

use dbento_testkit::{MY_SWEEP_LOCK, mariadb_db, my_sweep_stale, mysql_db};
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Opts, OptsBuilder};

/// ("mysql" | "mariadb", url) for each shared server set; containers are never swept, so
/// with neither variable set the tests print why and check nothing.
fn shared_urls() -> Vec<(&'static str, String)> {
    let urls: Vec<_> = [
        ("mysql", "DBENTO_MYSQL_URL"),
        ("mariadb", "DBENTO_MARIADB_URL"),
    ]
    .into_iter()
    .filter_map(|(label, var)| {
        let url = std::env::var(var).ok().filter(|u| !u.is_empty())?;
        Some((label, url))
    })
    .collect();
    if urls.is_empty() {
        eprintln!("skipped: neither DBENTO_MYSQL_URL nor DBENTO_MARIADB_URL is set");
    }
    urls
}

/// A server-level connection, or one using database `name`.
async fn connect(url: &str, name: Option<&str>) -> Conn {
    let opts = Opts::from_url(url).expect("server URL");
    let opts = OptsBuilder::from_opts(opts)
        .prefer_socket(false)
        .db_name(name);
    Conn::new(opts).await.expect("connect")
}

async fn database_count(url: &str, name: &str) -> u64 {
    let mut conn = connect(url, None).await;
    let count: Option<u64> = conn
        .exec_first(
            "SELECT COUNT(*) FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = ?",
            (name,),
        )
        .await
        .expect("count databases");
    let _ = conn.disconnect().await;
    count.unwrap_or_default()
}

/// Creates a database named for a run whose pid cannot be alive; `tag` keeps it unique to
/// this process and test, so concurrent sweeps can only drop it, never mistake it.
async fn dead_runs_database(conn: &mut Conn, tag: &str) -> String {
    let pid_max: u64 = std::fs::read_to_string("/proc/sys/kernel/pid_max")
        .expect("pid_max")
        .trim()
        .parse()
        .expect("pid_max is a number");
    let name = format!("dbento_t_{}_{}{tag}", pid_max + 1, std::process::id());
    conn.query_drop(format!("CREATE DATABASE `{name}`"))
        .await
        .expect("create the dead run's database");
    name
}

#[tokio::test]
async fn the_sweep_drops_a_dead_runs_database() {
    for (label, url) in shared_urls() {
        let mut admin = connect(&url, None).await;
        let name = dead_runs_database(&mut admin, "dead").await;
        let _ = admin.disconnect().await;
        my_sweep_stale(&url).await;
        assert_eq!(
            database_count(&url, &name).await,
            0,
            "{label}: {name} survived the sweep"
        );
    }
}

#[tokio::test]
async fn the_sweep_keeps_a_database_with_a_session() {
    for (label, url) in shared_urls() {
        // Every sweep takes the server-wide sweep lock, so holding it from the create until
        // the session is in keeps any concurrent sweep from dropping the database first.
        let mut admin = connect(&url, None).await;
        let locked: Option<i64> = admin
            .exec_first("SELECT GET_LOCK(?, 60)", (MY_SWEEP_LOCK,))
            .await
            .expect("take the sweep lock");
        assert_eq!(locked, Some(1), "{label}: the sweep lock was not granted");
        let name = dead_runs_database(&mut admin, "busy").await;
        let session = connect(&url, Some(&name)).await;
        admin
            .exec_drop("SELECT RELEASE_LOCK(?)", (MY_SWEEP_LOCK,))
            .await
            .expect("release the sweep lock");
        my_sweep_stale(&url).await;
        let survived = database_count(&url, &name).await;
        let _ = session.disconnect().await;
        admin
            .query_drop(format!("DROP DATABASE IF EXISTS `{name}`"))
            .await
            .expect("drop the busy database");
        let _ = admin.disconnect().await;
        assert_eq!(
            survived, 1,
            "{label}: the sweep dropped {name} while a session held it"
        );
    }
}

#[tokio::test]
async fn the_sweep_keeps_this_process_s_databases() {
    for (label, url) in shared_urls() {
        let db = match label {
            "mysql" => mysql_db().await,
            _ => mariadb_db().await,
        };
        my_sweep_stale(&url).await;
        assert_eq!(
            database_count(&url, db.database()).await,
            1,
            "{label}: the sweep dropped this process's {}",
            db.database()
        );
    }
}

#[tokio::test]
async fn the_sweep_keeps_a_live_runs_database() {
    for (label, url) in shared_urls() {
        // The parent (the test runner) is alive but is not this process, so only the /proc
        // check can keep its database.
        let name = format!(
            "dbento_t_{}_{}live",
            std::os::unix::process::parent_id(),
            std::process::id()
        );
        let mut admin = connect(&url, None).await;
        admin
            .query_drop(format!("CREATE DATABASE `{name}`"))
            .await
            .expect("create the live run's database");
        my_sweep_stale(&url).await;
        let survived = database_count(&url, &name).await;
        admin
            .query_drop(format!("DROP DATABASE IF EXISTS `{name}`"))
            .await
            .expect("drop the live run's database");
        let _ = admin.disconnect().await;
        assert_eq!(survived, 1, "{label}: the sweep dropped live {name}");
    }
}
