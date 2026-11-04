//! `Engine::Duckdb`: workspace rules, the connection form, capabilities and dialect arms.
use dbento_core::{
    Engine, FieldKind, FormatOptions, OpenError, Workspace, connection_form, default_schema,
    format_sql, line_comment_starts, quote_ident, quote_literal, statement_ranges,
};

fn ws_with(conn: &str) -> (tempfile::TempDir, Result<Workspace, OpenError>) {
    let dir = tempfile::tempdir().unwrap();
    let toml = format!("dbento = 1\n[connections.duck]\n{conn}");
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path());
    (dir, ws)
}

fn invalid(conn: &str) -> String {
    match ws_with(conn).1 {
        Err(OpenError::Invalid { message, .. }) => message,
        other => panic!("{:?}", other.err()),
    }
}

const DUCK: &str = "engine = \"duckdb\"\npath = \"data/a.duckdb\"\n";

#[test]
fn a_duckdb_connection_is_a_file_resolved_against_the_workspace() {
    let (_dir, ws) = ws_with(&format!("{DUCK}read_only = true\ncolor = \"red\"\n"));
    let ws = ws.unwrap();
    let c = ws.connection("duck").unwrap();
    assert_eq!(c.engine(), Engine::Duckdb);
    assert_eq!(Engine::Duckdb.name(), "duckdb");
    assert!(c.read_only());
    assert_eq!(c.display_target(), "data/a.duckdb");
    assert_eq!(
        c.file_path(),
        Some(ws.dir().join("data/a.duckdb").as_path())
    );
    assert_eq!(c.sqlite_path(), None);
    assert_eq!(c.host(), None);
}

#[test]
fn sqlite_keeps_its_path_under_both_accessors() {
    let (_dir, ws) = ws_with("engine = \"sqlite\"\npath = \"a.db\"\n");
    let ws = ws.unwrap();
    let c = ws.connection("duck").unwrap();
    assert_eq!(c.sqlite_path(), Some(ws.dir().join("a.db").as_path()));
    assert_eq!(c.file_path(), c.sqlite_path());
}

#[test]
fn duckdb_refuses_server_keys_and_settings_and_needs_a_path() {
    for extra in [
        "host = \"h\"",
        "port = 1",
        "database = \"d\"",
        "user = \"u\"",
        "secret = \"none\"",
        "tls = \"off\"",
        "settings = { a = \"b\" }",
    ] {
        let key = extra.split(' ').next().unwrap();
        let m = invalid(&format!("{DUCK}{extra}\n"));
        assert!(
            m.contains(&format!("`{key}`")) && m.contains("not valid for duckdb"),
            "{m}"
        );
    }
    assert!(invalid("engine = \"duckdb\"\n").contains("duckdb needs `path`"));
}

#[test]
fn engine_names_are_exact_and_the_list_names_duckdb() {
    let m = invalid("engine = \"DuckDB\"\npath = \"a.duckdb\"\n");
    assert!(
        m.contains("unknown engine `DuckDB`") && m.contains("duckdb"),
        "{m}"
    );
}

#[test]
fn same_session_compares_the_resolved_file() {
    let (_dir, ws) = ws_with(&format!(
        "{DUCK}color = \"red\"\n[connections.b]\n{DUCK}\n[connections.c]\nengine = \"duckdb\"\npath = \"data/b.duckdb\"\n"
    ));
    let ws = ws.unwrap();
    let (a, b, c) = (
        ws.connection("duck").unwrap(),
        ws.connection("b").unwrap(),
        ws.connection("c").unwrap(),
    );
    assert!(a.same_session(b));
    assert!(!a.same_session(c));
}

#[test]
fn capabilities_say_what_works() {
    let d = Engine::Duckdb.capabilities();
    assert!(d.schemas && d.file && d.server_cancel && d.table_definition);
    assert!(!d.explain && !d.edit_rows && !d.password && !d.tls);
}

#[test]
fn the_form_is_one_required_path_with_the_read_only_cost() {
    let form = connection_form(Engine::Duckdb);
    assert_eq!(form.sections.len(), 1);
    assert_eq!(form.sections[0].title, "Connection");
    let fields = &form.sections[0].fields;
    assert_eq!(fields.len(), 1);
    assert_eq!((fields[0].key, fields[0].required), ("path", true));
    assert_eq!(fields[0].kind, FieldKind::Path);
    let help = form.read_only_help.unwrap();
    assert!(
        help.contains("read_csv") && help.contains("CLI and MCP"),
        "{help}"
    );
}

#[test]
fn duckdb_text_uses_the_postgres_dialect_rules() {
    let sql = "SELECT $$a;b$$, E'x\\';y'; /* a /* b; */ c */ SELECT 2";
    assert_eq!(
        statement_ranges(Engine::Duckdb, sql),
        statement_ranges(Engine::Postgres, sql)
    );
    assert_eq!(quote_ident(Engine::Duckdb, "users"), "users");
    assert_eq!(quote_ident(Engine::Duckdb, "Users"), "\"Users\"");
    let (_dir, ws) = ws_with(DUCK);
    let ws = ws.unwrap();
    let c = ws.connection("duck").unwrap();
    assert_eq!(default_schema(Engine::Duckdb, c), Some("main".to_owned()));
}

#[cfg(not(feature = "duckdb"))]
#[tokio::test]
async fn without_the_feature_a_duckdb_connection_is_unsupported() {
    use dbento_core::{ConnectError, SessionOptions, connect};
    let (_dir, ws) = ws_with(DUCK);
    let ws = ws.unwrap();
    let c = ws.connection("duck").unwrap();
    match connect(&ws, c, None, SessionOptions::default()).await {
        Err(ConnectError::Unsupported { reason }) => {
            assert_eq!(reason, "DuckDB is not available in this build");
        }
        other => panic!("{:?}", other.err()),
    }
}

#[test]
fn duckdb_formats_as_postgres_does() {
    let o = FormatOptions::default();
    for sql in [
        "select $$a;b$$ as x from t where y = e'it\\'s'",
        "select 1 /* a /* b */ c */ , 2 -- end",
    ] {
        assert_eq!(
            format_sql(Engine::Duckdb, sql, &o),
            format_sql(Engine::Postgres, sql, &o),
            "{sql}"
        );
        assert_eq!(
            line_comment_starts(Engine::Duckdb, sql),
            line_comment_starts(Engine::Postgres, sql),
            "{sql}"
        );
    }
}

#[cfg(feature = "duckdb")]
#[test]
fn quoted_names_and_literals_round_trip_through_duckdb() {
    let cfg = duckdb::Config::default()
        .enable_autoload_extension(false)
        .unwrap();
    let c = duckdb::Connection::open_in_memory_with_flags(cfg).unwrap();
    let mut st = c
        .prepare("SELECT keyword_name FROM duckdb_keywords()")
        .unwrap();
    let mut names: Vec<String> = st
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    names.extend(["users", "Users", "a b", "x\"y", "1a", "_ok", "é", "UPPER"].map(str::to_owned));
    for name in &names {
        let q = quote_ident(Engine::Duckdb, name);
        // A column reference needs a ColId; an AS label alone would accept any keyword.
        let mut st = c
            .prepare(&format!("SELECT {q} FROM (SELECT 1 AS {q}) AS t"))
            .unwrap_or_else(|e| panic!("{name} as {q}: {e}"));
        drop(st.stream_arrow([]).unwrap());
        assert_eq!(st.schema().field(0).name(), name, "{q}");
    }
    for text in ["it's", "a\\b", "", "é", "line\nbreak", "''"] {
        let lit = quote_literal(Engine::Duckdb, text);
        let back: String = c
            .query_row(&format!("SELECT {lit}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(back, text, "{lit}");
    }
}
