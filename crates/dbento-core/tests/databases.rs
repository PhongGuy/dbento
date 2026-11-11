//! A server's databases, as the connection dialog offers them after Test: the ones a session
//! could open, by name, with the server's own last.

use std::future::Future;
use std::time::Duration;

use crate::support::{ms_off, my_fixtures, pg_fixture, sqlite_fixture};
use dbento_core::{IntrospectError, SessionOptions};

async fn bounded(test: impl Future<Output = ()>) {
    tokio::time::timeout(Duration::from_secs(120), test)
        .await
        .expect("test timed out");
}

#[tokio::test]
async fn postgres_lists_the_openable_databases_with_postgres_last() {
    bounded(async {
        let fx = pg_fixture("").await;
        // A second database on the same server; its name keeps the testkit's prefix, so the
        // sweep of a dead run's databases takes it too.
        let other = format!("{}_other", fx.db.database());
        let admin = fx.db.admin().await;
        admin
            .batch_execute(&format!("CREATE DATABASE \"{other}\""))
            .await
            .unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        let names = s.databases().await.unwrap();
        assert!(names.iter().any(|n| n == fx.db.database()), "{names:?}");
        assert!(names.contains(&other), "{names:?}");
        assert!(
            !names.iter().any(|n| n.starts_with("template")),
            "templates are left out: {names:?}"
        );
        assert_eq!(
            names.last().map(String::as_str),
            Some("postgres"),
            "{names:?}"
        );
        let user = &names[..names.len() - 1];
        let mut sorted = user.to_vec();
        sorted.sort();
        assert_eq!(user, sorted, "the user's databases are in name order");
        admin
            .batch_execute(&format!("DROP DATABASE \"{other}\""))
            .await
            .unwrap();
    })
    .await;
}

#[tokio::test]
async fn postgres_leaves_out_a_database_that_refuses_connections() {
    bounded(async {
        let fx = pg_fixture("").await;
        let shut = format!("{}_shut", fx.db.database());
        let admin = fx.db.admin().await;
        admin
            .batch_execute(&format!(
                "CREATE DATABASE \"{shut}\" ALLOW_CONNECTIONS false"
            ))
            .await
            .unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        let names = s.databases().await.unwrap();
        assert!(!names.contains(&shut), "{names:?}");
        admin
            .batch_execute(&format!("DROP DATABASE \"{shut}\""))
            .await
            .unwrap();
    })
    .await;
}

#[tokio::test]
async fn mysql_lists_its_schemas_as_databases_with_the_servers_own_last() {
    for (label, fx) in my_fixtures("").await {
        let mut s = fx.session(SessionOptions::default()).await;
        let names = s.databases().await.unwrap();
        let schemas: Vec<String> = s
            .schemas()
            .await
            .unwrap()
            .iter()
            .map(|x| x.name().to_owned())
            .collect();
        assert_eq!(names, schemas, "{label}: a database is a schema");
        assert!(
            names.iter().any(|n| n == fx.db.database()),
            "{label}: {names:?}"
        );
        let n = names.len();
        assert_eq!(
            &names[n - 4..],
            ["information_schema", "mysql", "performance_schema", "sys"],
            "{label}"
        );
    }
}

#[tokio::test]
async fn sql_server_lists_the_databases_it_can_enter_with_the_system_ones_last() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    // A second database on the same server: mssql_db would start another server, and it waits
    // for the slot this fixture holds.
    let other = format!("{}_other", fx.db.database());
    let create = format!("DROP DATABASE IF EXISTS [{other}]; CREATE DATABASE [{other}]");
    fx.db.scalar(&create).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let names = s.databases().await.unwrap();
    assert!(names.iter().any(|n| n == fx.db.database()), "{names:?}");
    assert!(names.contains(&other), "{names:?}");
    let n = names.len();
    assert!(n > 4, "{names:?}");
    let mut system = names[n - 4..].to_vec();
    system.sort();
    assert_eq!(system, ["master", "model", "msdb", "tempdb"], "{names:?}");
    fx.db.scalar(&format!("DROP DATABASE [{other}]")).await;
}

#[tokio::test]
async fn sqlite_has_no_databases_to_list() {
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    assert!(matches!(
        s.databases().await,
        Err(IntrospectError::Unsupported { what: "databases" })
    ));
}
