//! The Redis engine as the workspace, form and dialect layers see it, before any server.
use dbento_core::{
    ConnectionSpec, Edit, Engine, ExplainError, ExplainMode, FormatOptions, OpenError,
    QueryLanguage, SecretSource, TlsMode, Workspace, default_schema, explain_request, explain_text,
    format_sql, line_comment_starts, plan_edit, quote_ident, quote_literal, read_only_residual,
    single_source, statement_count, statement_ranges,
};

fn ws_with(toml: &str) -> (tempfile::TempDir, Result<Workspace, OpenError>) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path());
    (dir, ws)
}

fn open(toml: &str) -> (tempfile::TempDir, Workspace) {
    let (d, ws) = ws_with(toml);
    (d, ws.unwrap())
}

fn invalid(toml: &str) -> String {
    match ws_with(toml).1 {
        Err(OpenError::Invalid { message, .. }) => message,
        other => panic!("{:?}", other.err()),
    }
}

const RD: &str = "dbento = 1\n[connections.cache]\nengine = \"redis\"\nhost = \"cache.internal\"\n\
                  port = 6379\ndatabase = \"0\"\nuser = \"dbento_ro\"\ntls = \"verify\"\n\
                  read_only = true\n";
const HEAD: &str = "dbento = 1\n[connections.cache]\nengine = \"redis\"\n";

#[test]
fn a_redis_entry_opens_with_its_keys() {
    let (_d, ws) = open(RD);
    let c = ws.declared("cache").unwrap();
    assert_eq!((c.engine(), c.engine().name()), (Engine::Redis, "redis"));
    assert_eq!(
        (c.host(), c.port(), c.database(), c.user()),
        (
            Some("cache.internal"),
            Some(6379),
            Some("0"),
            Some("dbento_ro")
        )
    );
    assert_eq!((c.tls(), c.read_only()), (Some(TlsMode::Verify), true));
    assert_eq!(c.secret_source(), &SecretSource::Keychain);
    assert_eq!(c.display_target(), "dbento_ro@cache.internal:6379/0");
    assert_eq!(default_schema(Engine::Redis, c).as_deref(), Some("db0"));
}

#[test]
fn a_bare_valkey_entry_takes_every_default() {
    let (_d, ws) = open("dbento = 1\n[connections.cache]\nengine = \"valkey\"\n");
    let c = ws.declared("cache").unwrap();
    assert_eq!(c.engine(), Engine::Redis);
    assert_eq!(
        (c.host(), c.port(), c.database(), c.user()),
        (Some("127.0.0.1"), Some(6379), Some("0"), None)
    );
    assert_eq!(c.tls(), Some(TlsMode::Off));
    assert_eq!(c.secret_source(), &SecretSource::Keychain);
    assert_eq!(c.display_target(), "127.0.0.1:6379/0");
    let (_d, ws) = open(&format!("{HEAD}user = \"  \"\nsecret = \"none\"\n"));
    let c = ws.declared("cache").unwrap();
    assert_eq!((c.user(), c.secret_source()), (None, &SecretSource::None));
}

#[test]
fn the_db_index_is_a_non_negative_integer_in_canonical_form() {
    for (word, want) in [("3", "3"), ("007", "7"), ("4294967295", "4294967295")] {
        let (_d, ws) = open(&format!("{HEAD}database = \"{word}\"\n"));
        let c = ws.declared("cache").unwrap();
        assert_eq!(c.database(), Some(want), "{word}");
        assert_eq!(
            default_schema(Engine::Redis, c),
            Some(format!("db{want}")),
            "{word}"
        );
    }
    for word in ["abc", "-1", "1.5", "", " ", "4294967296"] {
        let m = invalid(&format!("{HEAD}database = \"{word}\"\n"));
        assert!(
            m.contains("redis `database` must be a DB index"),
            "{word:?}: {m}"
        );
    }
    assert!(matches!(
        ws_with(&format!("{HEAD}database = 3\n")).1,
        Err(OpenError::Parse { .. })
    ));
}

#[test]
fn keys_of_other_engines_are_refused_with_a_reason() {
    for (line, want) in [
        ("path = \"x\"", "`path` is not valid for redis"),
        (
            "settings = { a = \"b\" }",
            "`settings` is not valid for redis",
        ),
        (
            "uri = \"redis://h\"",
            "`uri` is a mongodb key, not valid for redis",
        ),
        (
            "namespace = \"a\"",
            "`namespace` is a surrealdb key, not valid for redis",
        ),
        (
            "auth_level = \"root\"",
            "`auth_level` is a surrealdb key, not valid for redis",
        ),
        (
            "read_only_user = \"r\"",
            "`read_only_user` is a mssql key, not valid for redis",
        ),
    ] {
        let m = invalid(&format!("{HEAD}{line}\n"));
        assert!(m.contains(want), "{line}: {m}");
    }
    let m = invalid("dbento = 1\n[connections.x]\nengine = \"oracle\"\n");
    assert!(m.contains("redis"), "{m}");
}

#[test]
fn tls_follows_the_postgres_rules() {
    let tls = |host: &str, line: &str| {
        open(&format!("{HEAD}host = \"{host}\"\n{line}"))
            .1
            .declared("cache")
            .unwrap()
            .tls()
    };
    assert_eq!(tls("localhost", ""), Some(TlsMode::Off));
    assert_eq!(tls("cache.internal", ""), Some(TlsMode::Verify));
    assert_eq!(tls("cache.internal", "tls = \"off\"\n"), Some(TlsMode::Off));
    let pair = "tls = \"verify\"\ntls_ca = \"ca.pem\"\ntls_cert = \"c.pem\"\ntls_key = \"k.pem\"\n";
    let (_d, ws) = open(&format!("{HEAD}{pair}"));
    assert!(
        ws.declared("cache")
            .unwrap()
            .tls_ca()
            .unwrap()
            .ends_with("ca.pem")
    );
    let m = invalid(&format!("{HEAD}tls = \"verify\"\ntls_cert = \"c.pem\"\n"));
    assert!(m.contains("`tls_cert` needs `tls_key`"), "{m}");
    let m = invalid(&format!("{HEAD}tls = \"require\"\n"));
    assert!(m.contains("must be \"verify\" or \"off\""), "{m}");
}

#[test]
fn a_redis_entry_round_trips_through_a_save() {
    let (_dir, ws) = open("dbento = 1\n");
    let mut spec = ConnectionSpec::new(Engine::Redis);
    spec.host = Some("cache.internal".into());
    spec.port = Some(6380);
    spec.database = Some("2".into());
    spec.user = Some("dbento_ro".into());
    spec.tls = Some(TlsMode::Verify);
    spec.read_only = Some(true);
    let edit = Edit::PutConnection {
        previous: None,
        name: "cache".into(),
        spec: spec.clone(),
    };
    let text = plan_edit(&ws, &edit).unwrap().new_text().to_owned();
    assert!(text.contains("engine = \"redis\""), "{text}");
    assert!(text.contains("database = \"2\""), "{text}");
    assert_eq!(open(&text).1.connection_spec("cache"), Some(spec));
}

#[test]
fn a_changed_db_index_is_a_different_session() {
    let (_a, a) = open(RD);
    let (_b, b) = open(&RD.replace("database = \"0\"", "database = \"1\""));
    let (a, b) = (a.declared("cache").unwrap(), b.declared("cache").unwrap());
    assert!(a.same_session(a) && !a.same_session(b));
}

#[test]
fn statement_ranges_are_the_redis_lines() {
    let text = "GET a\n# c\nSET b \"x y\"\n";
    assert_eq!(statement_ranges(Engine::Redis, text), [0..5, 10..21]);
    assert_eq!(statement_count(Engine::Redis, text), 2);
}

#[cfg(feature = "redis")]
#[tokio::test]
async fn a_read_only_redis_connection_dials_its_host() {
    use dbento_core::{ConnectError, SessionOptions, connect};

    // RFC 6761 reserves `.invalid` to never resolve, so the connect fails at the network.
    let (_d, ws) = open(&RD.replace("cache.internal", "cache.invalid"));
    match connect(
        &ws,
        ws.declared("cache").unwrap(),
        None,
        SessionOptions::default(),
    )
    .await
    {
        Err(ConnectError::Open { message, .. }) => {
            assert!(message.contains("cache.invalid:6379"), "{message}")
        }
        Err(other) => panic!("{other}"),
        Ok(_) => panic!("a Redis session opened"),
    }
}

#[test]
fn capabilities_and_the_sql_helpers_fit_a_command_language() {
    let r = Engine::Redis.capabilities();
    assert!(r.schemas && r.password && r.tls && r.key_browser);
    assert!(!r.server_cancel && !r.file && !r.edit_rows && !r.explain && !r.table_definition);
    assert_eq!(r.language, QueryLanguage::RedisCommands);
    let q = "HSET \"a b\" f 'it''s'";
    assert_eq!(
        (
            quote_ident(Engine::Redis, "a b"),
            quote_literal(Engine::Redis, "it's")
        ),
        ("a b".to_owned(), "it's".to_owned())
    );
    assert_eq!(format_sql(Engine::Redis, q, &FormatOptions::default()), q);
    assert_eq!(line_comment_starts(Engine::Redis, q), None);
    assert_eq!(single_source(Engine::Redis, q, Some("db0")), None);
    let unsupported = Some(ExplainError::UnsupportedEngine);
    assert_eq!(
        explain_request(Engine::Redis, q, ExplainMode::Plan).err(),
        unsupported
    );
    assert_eq!(explain_text(Engine::Redis, q).err(), unsupported);
}

#[test]
fn the_residual_names_the_acl_at_connect() {
    let r = read_only_residual(Engine::Redis).unwrap();
    assert!(r.contains("ACL SETUSER") && r.contains("connect"), "{r}");
}
