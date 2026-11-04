//! The SurrealDB engine as the capability and dialect layers see it, before any server.
use dbento_core::{
    ConnectError, Engine, ExplainError, ExplainMode, FormatOptions, OpenError, QueryLanguage,
    SessionOptions, Workspace, connect, default_schema, explain_request, explain_text, format_sql,
    line_comment_starts, quote_ident, quote_literal, single_source, statement_count,
    statement_ranges,
};

fn ws_with(toml: &str) -> (tempfile::TempDir, Result<Workspace, OpenError>) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path());
    (dir, ws)
}

const SR: &str = "dbento = 1\n[connections.graph]\nengine = \"surrealdb\"\nhost = \"h\"\n\
                  namespace = \"app\"\ndatabase = \"main\"\n";

#[test]
fn the_default_schema_is_the_connections_database() {
    let (_d, ws) = ws_with(SR);
    let ws = ws.unwrap();
    let c = ws.connection("graph").unwrap();
    assert_eq!(
        default_schema(Engine::Surrealdb, c).as_deref(),
        Some("main")
    );
}

#[test]
fn capabilities_and_the_sql_helpers_fit_surrealql() {
    let c = Engine::Surrealdb.capabilities();
    assert!(c.schemas && c.server_cancel && c.password && c.tls);
    assert!(!c.file && !c.edit_rows && !c.explain && !c.table_definition);
    assert_eq!(c.language, QueryLanguage::SurrealQl);
    let e = Engine::Surrealdb;
    assert_eq!(quote_ident(e, "a`b c"), "`a\\`b c`");
    assert_eq!(quote_ident(e, "plain"), "`plain`");
    assert_eq!(quote_literal(e, "it's \\"), "'it\\'s \\\\'");
    let q = "SELECT  *  FROM t -- c";
    assert_eq!(format_sql(e, q, &FormatOptions::default()), q);
    assert_eq!(line_comment_starts(e, q), None);
    assert_eq!(single_source(e, q, Some("main")), None);
    let unsupported = Some(ExplainError::UnsupportedEngine);
    assert_eq!(explain_request(e, q, ExplainMode::Plan).err(), unsupported);
    assert_eq!(explain_text(e, q).err(), unsupported);
}

#[test]
fn statement_ranges_returns_units_and_statement_count_counts_statements() {
    let e = Engine::Surrealdb;
    let src = "USE DB b; RETURN 1; BEGIN; CREATE a:1; COMMIT; LET $x = 1; RETURN $x";
    let ranges: Vec<&str> = statement_ranges(e, src)
        .into_iter()
        .map(|r| &src[r])
        .collect();
    assert_eq!(
        ranges,
        [
            "USE DB b;",
            "RETURN 1;",
            "BEGIN; CREATE a:1; COMMIT;",
            "LET $x = 1; RETURN $x"
        ]
    );
    assert_eq!(statement_count(e, src), 7);
    assert_eq!(statement_count(e, "LET $a = 1; RETURN $a"), 2);
    assert_eq!(statement_ranges(e, "LET $a = 1; RETURN $a").len(), 1);
    // Every other engine counts its ranges.
    for (engine, sql, n) in [
        (Engine::Postgres, "SELECT 1; SELECT 2", 2),
        (Engine::Sqlite, "SELECT 1; SELECT 2; SELECT 3", 3),
        (Engine::Mysql, "SELECT 1", 1),
        (Engine::Mssql, "SELECT 1; SELECT 2\nGO\nSELECT 3", 2),
        (Engine::Duckdb, "SELECT 1; SELECT 2", 2),
        (Engine::Mongodb, "db.t.find(); db.u.find()", 2),
    ] {
        assert_eq!(statement_count(engine, sql), n, "{engine:?}");
        assert_eq!(statement_ranges(engine, sql).len(), n, "{engine:?}");
    }
}

async fn connect_error(toml: &str) -> String {
    let (_d, ws) = ws_with(toml);
    let ws = ws.unwrap();
    match connect(
        &ws,
        ws.connection("graph").unwrap(),
        None,
        SessionOptions::default(),
    )
    .await
    {
        Err(ConnectError::Unsupported { reason, .. }) => reason,
        Err(other) => panic!("{other}"),
        Ok(_) => panic!("a SurrealDB session opened"),
    }
}

#[cfg(feature = "surrealdb")]
#[tokio::test]
async fn connect_waits_for_the_driver() {
    assert_eq!(
        connect_error(SR).await,
        "SurrealDB sessions are not available yet"
    );
}

#[cfg(not(feature = "surrealdb"))]
#[tokio::test]
async fn without_the_feature_a_surrealdb_connection_parses_but_does_not_connect() {
    assert_eq!(
        connect_error(SR).await,
        "SurrealDB is not available in this build"
    );
}
