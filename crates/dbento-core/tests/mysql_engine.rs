//! The MySQL engine as the workspace and capability layers see it, before any server.
use dbento_core::{
    Engine, ExplainError, ExplainMode, FormatOptions, IntrospectError, MemoryStore, OpenError,
    RelationRef, SecretSource, TlsMode, Workspace, default_schema, explain_request, explain_text,
    format_sql, line_comment_starts, quote_ident, secret_hint, single_source, splits_token,
};

fn ws_with(toml: &str) -> (tempfile::TempDir, Result<Workspace, OpenError>) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path());
    (dir, ws)
}

fn invalid(toml: &str) -> String {
    match ws_with(toml).1 {
        Err(OpenError::Invalid { message, .. }) => message,
        other => panic!("{:?}", other.err()),
    }
}

const MY: &str =
    "dbento = 1\n[connections.my]\nengine = \"mysql\"\nhost = \"db.example.com\"\nuser = \"app\"\n";

#[test]
fn a_mysql_connection_parses_with_its_defaults() {
    let (_dir, ws) = ws_with(MY);
    let ws = ws.unwrap();
    let c = ws.connection("my").unwrap();
    assert_eq!((c.engine(), c.engine().name()), (Engine::Mysql, "mysql"));
    assert_eq!(
        (c.host(), c.port(), c.user(), c.database()),
        (Some("db.example.com"), Some(3306), Some("app"), None)
    );
    assert_eq!(c.tls(), Some(TlsMode::Verify));
    assert_eq!(c.secret_source(), &SecretSource::Keychain);
    assert_eq!(c.display_target(), "app@db.example.com:3306");
}

#[test]
fn mariadb_is_an_alias_and_a_local_host_defaults_to_plaintext() {
    let (_dir, ws) = ws_with(
        "dbento = 1\n[connections.m]\nengine = \"mariadb\"\nhost = \"127.0.0.1\"\nport = 3307\nuser = \"u\"\ndatabase = \"shop\"\n",
    );
    let ws = ws.unwrap();
    let c = ws.connection("m").unwrap();
    assert_eq!((c.engine(), c.tls()), (Engine::Mysql, Some(TlsMode::Off)));
    assert_eq!(c.display_target(), "u@127.0.0.1:3307/shop");
    assert_eq!(ws.connection_spec("m").unwrap().engine, Engine::Mysql);
}

#[test]
fn mysql_refuses_other_engines_keys_and_needs_host_and_user() {
    let base = "dbento = 1\n[connections.my]\nengine = \"mysql\"\n";
    let m = invalid(&format!(
        "{base}host = \"h\"\nuser = \"u\"\npath = \"x.db\"\n"
    ));
    assert!(m.contains("`path`") && m.contains("mysql"), "{m}");
    let m = invalid(&format!(
        "{base}host = \"h\"\nuser = \"u\"\nsettings = {{ a = \"b\" }}\n"
    ));
    assert!(m.contains("`settings`") && m.contains("mysql"), "{m}");
    assert!(invalid(&format!("{base}user = \"u\"\n")).contains("`host`"));
    assert!(invalid(&format!("{base}host = \"h\"\n")).contains("`user`"));
    let m = invalid("dbento = 1\n[connections.x]\nengine = \"oracle\"\nhost = \"h\"\n");
    assert!(
        m.contains("unknown engine `oracle`") && m.contains("mysql"),
        "{m}"
    );
}

#[test]
fn capabilities_are_static_per_engine() {
    let my = Engine::Mysql.capabilities();
    assert!(my.schemas && my.password && my.tls);
    assert!(!my.file && my.server_cancel && !my.edit_rows && !my.explain && my.table_definition);
    let pg = Engine::Postgres.capabilities();
    assert!(pg.schemas && pg.server_cancel && pg.password && pg.tls && pg.edit_rows && pg.explain);
    assert!(!pg.file && !pg.table_definition);
    let lite = Engine::Sqlite.capabilities();
    assert!(
        lite.server_cancel && lite.file && lite.edit_rows && lite.explain && lite.table_definition
    );
    assert!(!lite.schemas && !lite.password && !lite.tls);
}

#[test]
fn a_mysql_keychain_connection_gets_the_secret_hint() {
    let (_dir, ws) = ws_with(MY);
    let ws = ws.unwrap();
    let hint = secret_hint(&ws, ws.connection("my").unwrap(), &MemoryStore::new()).unwrap();
    assert!(
        hint.contains("dbento secret set my") && hint.contains("DBENTO_SECRET_MY"),
        "{hint}"
    );
}

#[test]
fn mysql_names_are_backtick_quoted_and_sources_need_a_database() {
    assert_eq!(quote_ident(Engine::Mysql, "users"), "`users`");
    assert_eq!(quote_ident(Engine::Mysql, "a`b"), "`a``b`");
    assert_eq!(
        single_source(Engine::Mysql, "SELECT * FROM users", None),
        None
    );
    assert_eq!(
        single_source(Engine::Mysql, "SELECT * FROM users", Some("shop")),
        Some(RelationRef::new("shop", "users"))
    );
    let e = IntrospectError::Unsupported {
        what: "foreign keys",
    };
    assert_eq!(
        e.to_string(),
        "foreign keys is not available on this engine yet"
    );
}

#[test]
fn the_formatter_reads_mysql_and_explain_refuses_it() {
    let sql = "select 1 # c\nfrom t";
    assert_eq!(
        format_sql(Engine::Mysql, sql, &FormatOptions::default()),
        "SELECT 1 # c\nFROM t"
    );
    assert_eq!(line_comment_starts(Engine::Mysql, sql), Some(vec![9]));
    assert!(splits_token(Engine::Mysql, sql, 0..3));
    let explained = explain_request(Engine::Mysql, "SELECT 1", ExplainMode::Plan);
    assert_eq!(explained.err(), Some(ExplainError::UnsupportedEngine));
    assert_eq!(
        explain_text(Engine::Mysql, "SELECT 1").err(),
        Some(ExplainError::UnsupportedEngine)
    );
}

#[test]
fn the_default_schema_is_the_connections_database_on_mysql() {
    let (_dir, ws) = ws_with(&format!(
        "{MY}[connections.shop]\nengine = \"mysql\"\nhost = \"h\"\nuser = \"u\"\ndatabase = \"shop\"\n\
         [connections.pg]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\n\
         [connections.lite]\nengine = \"sqlite\"\npath = \"x.db\"\n"
    ));
    let ws = ws.unwrap();
    let at = |name: &str| {
        let c = ws.connection(name).unwrap();
        default_schema(c.engine(), c)
    };
    assert_eq!(at("my"), None);
    assert_eq!(at("shop").as_deref(), Some("shop"));
    assert_eq!(at("pg").as_deref(), Some("public"));
    assert_eq!(at("lite").as_deref(), Some("main"));
}
