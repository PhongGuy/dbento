//! Child connections against real servers: each opens its own database with its parent's
//! secret and safety.
use std::future::Future;
use std::time::Duration;

use crate::support::{ms_reader, run, text};
use dbento_core::{
    ExecuteError, Limits, MemoryStore, Secret, SecretKey, SecretStore, SessionOptions, Workspace,
    connect, resolve_secret,
};
use dbento_testkit::PgDb;

async fn bounded(test: impl Future<Output = ()>) {
    tokio::time::timeout(Duration::from_secs(120), test)
        .await
        .expect("test timed out");
}

fn no_env(_: &str) -> Option<String> {
    None
}

/// A workspace whose parent `pg` reaches `db`'s server with `databases = "all"`, its secret in
/// the keychain and `extra` in its table.
fn parent_of(db: &PgDb, extra: &str) -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!(
            "dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"{}\"\nport = {}\ndatabase = \"{}\"\nuser = \"{}\"\ndatabases = \"all\"\n{extra}\n",
            db.host(),
            db.port(),
            db.database(),
            db.user()
        ),
    )
    .unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    (dir, ws)
}

#[tokio::test]
async fn a_postgres_child_opens_its_own_database_with_the_parents_keychain_secret() {
    bounded(async {
        let db = dbento_testkit::pg_db().await;
        let other = format!("{}_other", db.database());
        let admin = db.admin().await;
        admin
            .batch_execute(&format!("CREATE DATABASE \"{other}\""))
            .await
            .unwrap();
        let (_dir, ws) = parent_of(&db, "");
        let store = MemoryStore::new();
        let key = SecretKey::for_connection(&ws, ws.declared("pg").unwrap());
        store.set(&key, &Secret::new(db.password())).unwrap();
        let child = ws.connection(&format!("pg/{other}")).unwrap();
        let secret = resolve_secret(&ws, &child, &store, &no_env).unwrap();
        assert!(
            secret.is_some(),
            "the parent's keychain entry serves the child"
        );
        let mut s = connect(&ws, &child, secret, SessionOptions::default())
            .await
            .unwrap();
        let r = run(&mut s, "SELECT current_database()", Limits::default()).await;
        assert_eq!(r[0].as_ref().unwrap().rows, vec![vec![text(&other)]]);
        let _ = s.close().await;
        admin
            .batch_execute(&format!("DROP DATABASE \"{other}\" WITH (FORCE)"))
            .await
            .unwrap();
    })
    .await;
}

#[tokio::test]
async fn a_read_only_parent_makes_a_read_only_child() {
    bounded(async {
        let db = dbento_testkit::pg_db().await;
        let other = format!("{}_ro", db.database());
        let admin = db.admin().await;
        admin
            .batch_execute(&format!("CREATE DATABASE \"{other}\""))
            .await
            .unwrap();
        let (_dir, ws) = parent_of(&db, "read_only = true");
        let child = ws.connection(&format!("pg/{other}")).unwrap();
        let mut s = connect(
            &ws,
            &child,
            Some(Secret::new(db.password())),
            SessionOptions::default(),
        )
        .await
        .unwrap();
        assert!(s.is_read_only());
        let r = run(&mut s, "CREATE TEMP TABLE x(a int)", Limits::default()).await;
        assert!(
            matches!(r[0], Err(ExecuteError::ReadOnlyViolation { .. })),
            "{:?}",
            r[0].as_ref().err()
        );
        let _ = s.close().await;
        admin
            .batch_execute(&format!("DROP DATABASE \"{other}\" WITH (FORCE)"))
            .await
            .unwrap();
    })
    .await;
}

#[tokio::test]
async fn a_sql_server_child_reads_as_the_reader_and_one_without_it_fails_alone() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_reader(
        &[],
        "read_only = true\nread_only_user = \"dbento_reader\"\ndatabases = \"all\"",
    )
    .await;
    let own = fx
        .ws
        .connection(&format!("ms/{}", fx.db.database()))
        .unwrap();
    let mut s = connect(&fx.ws, &own, Some(fx.secret()), SessionOptions::default())
        .await
        .unwrap();
    let who = run(
        &mut s,
        "SELECT USER_NAME() AS u, DB_NAME() AS d",
        Limits::default(),
    )
    .await
    .remove(0)
    .unwrap();
    assert_eq!(
        who.rows,
        vec![vec![text("dbento_reader"), text(fx.db.database())]]
    );
    let _ = s.close().await;
    let master = fx.ws.connection("ms/master").unwrap();
    let refused = connect(
        &fx.ws,
        &master,
        Some(fx.secret()),
        SessionOptions::default(),
    )
    .await;
    assert!(refused.is_err(), "master has no dbento_reader user");
    let again = connect(&fx.ws, &own, Some(fx.secret()), SessionOptions::default())
        .await
        .expect("the other child still opens");
    let _ = again.close().await;
}
