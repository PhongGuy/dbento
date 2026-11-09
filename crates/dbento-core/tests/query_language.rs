//! The non-SQL seam: query languages, document-shaped unguarded kinds, syntax errors, read-only
//! enforcement and saved shell queries.
use crate::support::{run, sqlite_fixture};
use dbento_core::{
    Engine, ExecuteError, Flavor, Limits, QueryLanguage, ReadOnlyEnforcement, SessionOptions,
    Unguarded, Workspace,
};

#[test]
fn every_sql_engine_reports_the_sql_language() {
    for e in [
        Engine::Postgres,
        Engine::Sqlite,
        Engine::Mysql,
        Engine::Mssql,
        Engine::Duckdb,
    ] {
        assert_eq!(e.capabilities().language, QueryLanguage::Sql, "{e:?}");
    }
    let sql = QueryLanguage::Sql;
    let shell = QueryLanguage::MongoShell;
    assert_eq!((sql.name(), sql.file_extension()), ("SQL", "sql"));
    assert_eq!(
        (shell.name(), shell.file_extension()),
        ("MongoDB shell", "js")
    );
}

#[test]
fn the_document_kinds_have_shell_labels_and_drop_is_reused() {
    assert_eq!(
        Unguarded::UpdateAllDocuments.label(),
        "updateMany with an empty filter"
    );
    assert_eq!(
        Unguarded::DeleteAllDocuments.label(),
        "deleteMany with an empty filter"
    );
    assert_eq!(Unguarded::Drop.label(), "DROP");
}

#[test]
fn a_syntax_error_reports_its_offset_and_mongodb_is_a_flavor() {
    let e = ExecuteError::Syntax {
        message: "expected `,` or `}`".into(),
        offset: 7,
    };
    assert_eq!(e.to_string(), "syntax error at byte 7: expected `,` or `}`");
    assert!(!e.closes_session());
    assert_ne!(Flavor::MongoDb, Flavor::SqlServer);
}

#[tokio::test]
async fn a_read_only_session_reports_database_enforcement_and_a_writable_one_none() {
    let fx = sqlite_fixture("CREATE TABLE t(x)", "");
    let writable = fx.session(SessionOptions::default()).await;
    assert_eq!(writable.read_only_enforcement(), None);
    let mut forced = SessionOptions::default();
    forced.force_read_only = true;
    assert_eq!(
        fx.session(forced).await.read_only_enforcement(),
        Some(ReadOnlyEnforcement::Database)
    );
    let ro = sqlite_fixture("CREATE TABLE t(x)", "read_only = true\n");
    assert_eq!(
        ro.session(SessionOptions::default())
            .await
            .read_only_enforcement(),
        Some(ReadOnlyEnforcement::Database)
    );
}

#[test]
fn js_files_under_queries_are_saved_queries_in_the_shell_language() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"a.db\"\n",
    )
    .unwrap();
    let q = dir.path().join("queries");
    std::fs::create_dir_all(q.join("lite")).unwrap();
    for f in ["lite/a.sql", "lite/b.js", "c.JS", "d.txt", "e.json"] {
        std::fs::write(q.join(f), "x").unwrap();
    }
    let ws = Workspace::open(dir.path()).unwrap();
    let listed: Vec<(String, QueryLanguage, Option<String>)> = ws
        .saved_queries()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s.path().to_string_lossy().replace('\\', "/"),
                s.language(),
                s.connection().map(str::to_owned),
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            ("queries/c.JS".to_owned(), QueryLanguage::MongoShell, None),
            (
                "queries/lite/a.sql".to_owned(),
                QueryLanguage::Sql,
                Some("lite".to_owned())
            ),
            (
                "queries/lite/b.js".to_owned(),
                QueryLanguage::MongoShell,
                Some("lite".to_owned())
            ),
        ]
    );
}

#[test]
fn surrealql_is_a_query_language_and_surrealdb_a_flavor() {
    let q = QueryLanguage::SurrealQl;
    assert_eq!((q.name(), q.file_extension()), ("SurrealQL", "surql"));
    assert_ne!(Flavor::SurrealDb, Flavor::MongoDb);
}

#[test]
fn an_expired_result_says_to_run_it_again_and_leaves_the_session_open() {
    let e = ExecuteError::ResultExpired;
    assert_eq!(e.to_string(), "the result expired; run the query again");
    assert!(!e.closes_session());
    assert_eq!(e.message_for(Engine::Sqlite), e.to_string());
}

#[tokio::test]
async fn an_outcome_is_not_rolled_back_on_an_existing_engine() {
    let fx = sqlite_fixture("CREATE TABLE t(x)", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let results = run(
        &mut s,
        "INSERT INTO t VALUES (1); SELECT x FROM t",
        Limits::default(),
    )
    .await;
    assert_eq!(results.len(), 2);
    for r in results {
        assert!(!r.unwrap().outcome.rolled_back);
    }
}

#[tokio::test]
async fn is_read_only_follows_the_level_the_driver_reports() {
    let fx = sqlite_fixture("CREATE TABLE t(x)", "");
    let writable = fx.session(SessionOptions::default()).await;
    assert_eq!(
        (writable.is_read_only(), writable.read_only_enforcement()),
        (false, None)
    );
    let mut forced = SessionOptions::default();
    forced.force_read_only = true;
    let ro = fx.session(forced).await;
    assert_eq!(
        (ro.is_read_only(), ro.read_only_enforcement()),
        (true, Some(ReadOnlyEnforcement::Database))
    );
}

#[test]
fn surql_files_under_queries_are_saved_queries_in_surrealql() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"a.db\"\n",
    )
    .unwrap();
    let q = dir.path().join("queries");
    std::fs::create_dir_all(q.join("lite")).unwrap();
    for f in [
        "lite/a.surql",
        "b.SURQL",
        "lite/c.sql",
        "lite/d.js",
        "e.surreal",
    ] {
        std::fs::write(q.join(f), "x").unwrap();
    }
    let ws = Workspace::open(dir.path()).unwrap();
    let listed: Vec<(String, QueryLanguage)> = ws
        .saved_queries()
        .unwrap()
        .iter()
        .map(|s| (s.path().to_string_lossy().replace('\\', "/"), s.language()))
        .collect();
    assert_eq!(
        listed,
        [
            ("queries/b.SURQL".to_owned(), QueryLanguage::SurrealQl),
            ("queries/lite/a.surql".to_owned(), QueryLanguage::SurrealQl),
            ("queries/lite/c.sql".to_owned(), QueryLanguage::Sql),
            ("queries/lite/d.js".to_owned(), QueryLanguage::MongoShell),
        ]
    );
}

#[test]
fn flushing_every_key_is_an_unguarded_kind() {
    assert_eq!(Unguarded::FlushKeys.label(), "FLUSHDB or FLUSHALL");
}

#[test]
fn redis_commands_is_a_query_language_and_redis_and_valkey_are_flavors() {
    let q = QueryLanguage::RedisCommands;
    assert_eq!((q.name(), q.file_extension()), ("Redis commands", "redis"));
    assert_ne!(Flavor::Redis, Flavor::Valkey);
}

#[test]
fn no_existing_engine_has_the_key_browser() {
    for e in [
        Engine::Postgres,
        Engine::Sqlite,
        Engine::Mysql,
        Engine::Mssql,
        Engine::Duckdb,
        Engine::Mongodb,
        Engine::Surrealdb,
    ] {
        assert!(!e.capabilities().key_browser, "{e:?}");
    }
}

#[test]
fn redis_files_under_queries_are_saved_queries_in_redis_commands() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"a.db\"\n",
    )
    .unwrap();
    let q = dir.path().join("queries");
    std::fs::create_dir_all(q.join("lite")).unwrap();
    for f in [
        "lite/a.redis",
        "b.REDIS",
        "lite/c.sql",
        "lite/d.surql",
        "e.rds",
    ] {
        std::fs::write(q.join(f), "x").unwrap();
    }
    let ws = Workspace::open(dir.path()).unwrap();
    let listed: Vec<(String, QueryLanguage)> = ws
        .saved_queries()
        .unwrap()
        .iter()
        .map(|s| (s.path().to_string_lossy().replace('\\', "/"), s.language()))
        .collect();
    assert_eq!(
        listed,
        [
            ("queries/b.REDIS".to_owned(), QueryLanguage::RedisCommands),
            (
                "queries/lite/a.redis".to_owned(),
                QueryLanguage::RedisCommands
            ),
            ("queries/lite/c.sql".to_owned(), QueryLanguage::Sql),
            ("queries/lite/d.surql".to_owned(), QueryLanguage::SurrealQl),
        ]
    );
}
