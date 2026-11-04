//! The SQL Server engine as the workspace, capability and dialect layers see it, before any server.
use dbento_core::{
    ConnectionSpec, Edit, EditValue, Engine, ExecuteError, ExplainError, ExplainMode, Flavor,
    FormatOptions, MemoryStore, OpenError, SecretSource, TlsMode, Workspace, default_schema,
    explain_request, explain_text, format_sql, key_filter, line_comment_starts, plan_edit,
    quote_ident, quote_literal, secret_hint, single_source, splits_token,
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

const MS: &str =
    "dbento = 1\n[connections.ms]\nengine = \"mssql\"\nhost = \"db.example.com\"\nuser = \"app\"\n";

#[test]
fn mssql_and_its_sqlserver_alias_name_one_engine() {
    let (_dir, ws) = ws_with(&format!(
        "{MS}[connections.alias]\nengine = \"sqlserver\"\nhost = \"h\"\nuser = \"u\"\ndatabase = \"shop\"\n"
    ));
    let ws = ws.unwrap();
    let c = ws.connection("ms").unwrap();
    assert_eq!((c.engine(), c.engine().name()), (Engine::Mssql, "mssql"));
    assert_eq!(
        (c.host(), c.port(), c.user(), c.database()),
        (Some("db.example.com"), Some(1433), Some("app"), None)
    );
    assert_eq!(c.display_target(), "app@db.example.com:1433");
    let alias = ws.connection("alias").unwrap();
    assert_eq!(alias.engine(), Engine::Mssql);
    assert_eq!(alias.display_target(), "u@h:1433/shop");
    assert_eq!(ws.connection_spec("alias").unwrap().engine, Engine::Mssql);
    let m = invalid("dbento = 1\n[connections.x]\nengine = \"oracle\"\nhost = \"h\"\n");
    assert!(
        m.contains("unknown engine `oracle`") && m.contains("mssql"),
        "{m}"
    );
}

#[test]
fn sql_server_capabilities_and_flavor() {
    let ms = Engine::Mssql.capabilities();
    assert!(ms.schemas && ms.server_cancel && ms.password && ms.tls);
    assert!(!ms.file && !ms.edit_rows && !ms.explain && !ms.table_definition);
    assert_ne!(Flavor::SqlServer, Flavor::Mysql);
}

#[test]
fn a_sql_server_keychain_connection_gets_the_secret_hint() {
    let (_dir, ws) = ws_with(MS);
    let ws = ws.unwrap();
    let hint = secret_hint(&ws, ws.connection("ms").unwrap(), &MemoryStore::new()).unwrap();
    assert!(
        hint.contains("dbento secret set ms") && hint.contains("DBENTO_SECRET_MS"),
        "{hint}"
    );
}

#[test]
fn names_and_literals_are_t_sql_and_nothing_guesses_a_source() {
    assert_eq!(quote_ident(Engine::Mssql, "users"), "[users]");
    assert_eq!(quote_ident(Engine::Mssql, "a]b"), "[a]]b]");
    assert_eq!(quote_literal(Engine::Mssql, "it's"), "N'it''s'");
    assert_eq!(quote_literal(Engine::Mssql, "a\\b é"), "N'a\\b é'");
    let bytes_key = [("k".to_owned(), EditValue::Bytes(vec![1, 2]))];
    assert_eq!(key_filter(Engine::Mssql, &bytes_key), None);
    assert_eq!(
        single_source(Engine::Mssql, "SELECT * FROM users", Some("dbo")),
        None
    );
    let (_dir, ws) = ws_with(MS);
    let ws = ws.unwrap();
    let schema = default_schema(Engine::Mssql, ws.connection("ms").unwrap());
    assert_eq!(schema.as_deref(), Some("dbo"));
}

#[test]
fn the_formatter_and_explain_leave_sql_server_alone() {
    let sql = "select [a b] from t /* x /* y */ z */ -- c";
    assert_eq!(
        format_sql(Engine::Mssql, sql, &FormatOptions::default()),
        sql
    );
    assert_eq!(line_comment_starts(Engine::Mssql, sql), None);
    assert!(splits_token(Engine::Mssql, sql, 0..3));
    let explained = explain_request(Engine::Mssql, "SELECT 1", ExplainMode::Plan);
    assert_eq!(explained.err(), Some(ExplainError::UnsupportedEngine));
    assert_eq!(
        explain_text(Engine::Mssql, "SELECT 1").err(),
        Some(ExplainError::UnsupportedEngine)
    );
}

#[test]
fn sql_server_keys_parse_with_their_defaults() {
    let (_dir, ws) = ws_with(
        "dbento = 1\n[connections.ms]\nengine = \"mssql\"\nhost = \"127.0.0.1\"\nuser = \"app\"\n\
         database = \"shop\"\ntls_ca = \"ca.pem\"\nread_only_user = \"dbento_reader\"\nsecret = { env = \"MS_PW\" }\n",
    );
    let ws = ws.unwrap();
    let c = ws.connection("ms").unwrap();
    assert_eq!((c.port(), c.database()), (Some(1433), Some("shop")));
    assert_eq!(
        c.tls(),
        Some(TlsMode::Verify),
        "verify even on a local host"
    );
    assert!(c.tls_ca().is_some_and(|p| p.ends_with("ca.pem")));
    assert_eq!(c.read_only_user(), Some("dbento_reader"));
    assert_eq!(c.secret_source(), &SecretSource::Env("MS_PW".into()));
    let (_d2, plain) = ws_with(MS);
    assert_eq!(
        plain.unwrap().connection("ms").unwrap().read_only_user(),
        None
    );
}

#[test]
fn sql_server_refuses_keys_it_cannot_honour_naming_each() {
    let base = "dbento = 1\n[connections.ms]\nengine = \"mssql\"\nhost = \"h\"\nuser = \"u\"\n";
    for (line, key) in [
        ("path = \"x.db\"", "path"),
        ("settings = { a = \"b\" }", "settings"),
        ("tls_cert = \"c.pem\"\ntls_key = \"k.pem\"", "tls_cert"),
        ("tls_key = \"k.pem\"", "tls_key"),
    ] {
        let m = invalid(&format!("{base}{line}\n"));
        assert!(
            m.contains(&format!("`{key}`")) && m.contains("mssql"),
            "{key}: {m}"
        );
    }
    match ws_with(&format!("{base}tls_key_passphrase = \"x\"\n")).1 {
        Err(OpenError::Parse { message, .. }) => {
            assert!(message.contains("tls_key_passphrase"), "{message}")
        }
        other => panic!("{:?}", other.err()),
    }
    let head = "dbento = 1\n[connections.ms]\nengine = \"mssql\"\n";
    assert!(invalid(&format!("{head}user = \"u\"\n")).contains("`host`"));
    assert!(invalid(&format!("{head}host = \"h\"\n")).contains("`user`"));
    assert!(invalid(&format!("{base}read_only_user = \"\"\n")).contains("`read_only_user`"));
    assert!(invalid(&format!("{base}read_only_user = \"  \"\n")).contains("`read_only_user`"));
    assert!(invalid(&format!("{base}tls = \"strict\"\n")).contains("tls `strict`"));
}

#[test]
fn read_only_user_is_a_sql_server_key_only() {
    for (engine, keys) in [
        ("postgres", "host = \"h\"\ndatabase = \"d\"\nuser = \"u\"\n"),
        ("mysql", "host = \"h\"\nuser = \"u\"\n"),
        ("sqlite", "path = \"a.db\"\n"),
    ] {
        let m = invalid(&format!(
            "dbento = 1\n[connections.c]\nengine = \"{engine}\"\n{keys}read_only_user = \"r\"\n"
        ));
        assert!(
            m.contains("`read_only_user`") && m.contains(engine),
            "{engine}: {m}"
        );
    }
}

#[test]
fn a_sql_server_entry_round_trips_through_a_save() {
    let (_dir, ws) = ws_with("dbento = 1\n");
    let ws = ws.unwrap();
    let mut spec = ConnectionSpec::new(Engine::Mssql);
    spec.host = Some("db.example.com".into());
    spec.port = Some(14330);
    spec.database = Some("shop".into());
    spec.user = Some("app".into());
    spec.tls = Some(TlsMode::Off);
    spec.tls_ca = Some("certs/ca.pem".into());
    spec.read_only_user = Some("dbento_reader".into());
    spec.secret = Some(SecretSource::Keychain);
    spec.read_only = Some(true);
    let edit = Edit::PutConnection {
        previous: None,
        name: "ms".into(),
        spec: spec.clone(),
    };
    let text = plan_edit(&ws, &edit).unwrap().new_text().to_owned();
    assert!(text.contains("engine = \"mssql\""), "{text}");
    assert!(
        text.contains("read_only_user = \"dbento_reader\""),
        "{text}"
    );
    let (_d2, reopened) = ws_with(&text);
    let reopened = reopened.unwrap();
    assert_eq!(reopened.connection_spec("ms"), Some(spec));
    let c = reopened.connection("ms").unwrap();
    assert_eq!(
        (c.read_only_user(), c.port(), c.tls()),
        (Some("dbento_reader"), Some(14330), Some(TlsMode::Off))
    );
}

#[test]
fn a_drain_cap_stop_on_sql_server_does_not_promise_a_rollback() {
    let m = ExecuteError::StoppedAtDrainCap.message_for(Engine::Mssql);
    assert!(!m.contains("any writes it made were rolled back"), "{m}");
    assert!(m.contains("already finished") && m.contains("keep"), "{m}");
    let e = ExecuteError::StoppedAtDrainCap;
    assert_eq!(e.message_for(Engine::Postgres), e.to_string());
}

#[test]
fn a_sql_server_stop_says_finished_statements_may_have_kept_their_writes() {
    for e in [ExecuteError::Cancelled, ExecuteError::Timeout] {
        let m = e.message_for(Engine::Mssql);
        assert!(m.starts_with(&e.to_string()), "{m}");
        assert!(
            m.contains("already finished") && m.contains("may have kept"),
            "{m}"
        );
        assert_eq!(e.message_for(Engine::Postgres), e.to_string());
    }
}
