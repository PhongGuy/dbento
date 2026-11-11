//! The MongoDB engine as the workspace, capability and dialect layers see it, before any server.
#[cfg(not(feature = "mongodb"))]
use dbento_core::{ConnectError, SessionOptions, connect};
use dbento_core::{
    ConnectionSpec, Edit, Engine, ExplainError, ExplainMode, FormatOptions, OpenError,
    QueryLanguage, SecretSource, Workspace, default_schema, explain_request, explain_text,
    format_sql, line_comment_starts, plan_edit, quote_ident, quote_literal, single_source,
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

const MG: &str = "dbento = 1\n[connections.shop]\nengine = \"mongodb\"\n\
                  uri = \"mongodb://app@db.internal:27017/shop?authSource=admin\"\n";
const HEAD: &str = "dbento = 1\n[connections.c]\nengine = \"mongodb\"\n";

#[cfg(not(feature = "mongodb"))]
async fn connect_error(toml: &str) -> String {
    let (_d, ws) = ws_with(toml);
    let ws = ws.unwrap();
    match connect(
        &ws,
        ws.declared("shop").unwrap(),
        None,
        SessionOptions::default(),
    )
    .await
    {
        Err(ConnectError::Unsupported { reason, .. }) => reason,
        Err(other) => panic!("{other}"),
        Ok(_) => panic!("a MongoDB session opened"),
    }
}

#[test]
fn a_mongodb_entry_opens_beside_a_sql_one_and_a_password_is_refused() {
    let (_d, ws) = ws_with(&format!(
        "{MG}[connections.alias]\nengine = \"mongo\"\nuri = \"mongodb://localhost\"\n\
         [connections.lite]\nengine = \"sqlite\"\npath = \"a.db\"\n"
    ));
    let ws = ws.unwrap();
    assert_eq!(ws.declared("lite").unwrap().engine(), Engine::Sqlite);
    let (c, a) = (ws.declared("shop").unwrap(), ws.declared("alias").unwrap());
    let uri = "mongodb://app@db.internal:27017/shop?authSource=admin";
    assert_eq!(
        (c.engine(), c.engine().name(), c.uri()),
        (Engine::Mongodb, "mongodb", Some(uri))
    );
    assert_eq!(
        (c.user(), c.database(), c.host(), c.port()),
        (Some("app"), Some("shop"), None, None)
    );
    assert_eq!(
        (c.display_target(), c.secret_source()),
        (uri.to_owned(), &SecretSource::Keychain)
    );
    assert_eq!(
        (a.engine(), a.user(), a.database()),
        (Engine::Mongodb, None, Some("test"))
    );
    assert_eq!(a.secret_source(), &SecretSource::None);
    assert_eq!(default_schema(Engine::Mongodb, c).as_deref(), Some("shop"));
    let m = invalid(&format!("{HEAD}uri = \"mongodb://app:hunter2@h/shop\"\n"));
    assert!(
        m.contains("remove the password from `uri`") && !m.contains("hunter2"),
        "{m}"
    );
    let m = invalid("dbento = 1\n[connections.x]\nengine = \"oracle\"\n");
    assert!(
        m.contains("unknown engine `oracle`") && m.contains("mongodb"),
        "{m}"
    );
}

#[test]
fn the_uri_is_required_mongodb_only_and_server_keys_name_the_uri_option() {
    assert!(invalid(HEAD).contains("mongodb needs `uri`"));
    let base = format!("{HEAD}uri = \"mongodb://h/x\"\n");
    for (line, key, hint) in [
        ("host = \"h\"", "host", "put it in `uri`"),
        ("port = 27017", "port", "host:port"),
        ("database = \"d\"", "database", "/<database>"),
        ("user = \"u\"", "user", "user@"),
        ("path = \"x\"", "path", "use `uri`"),
        ("settings = { a = \"b\" }", "settings", "after ?"),
        ("tls = \"verify\"", "tls", "tls=true"),
        ("tls_ca = \"ca.pem\"", "tls_ca", "tlsCAFile"),
        ("tls_cert = \"c.pem\"", "tls_cert", "tlsCertificateKeyFile"),
        ("tls_key = \"k.pem\"", "tls_key", "tlsCertificateKeyFile"),
        ("read_only_user = \"r\"", "read_only_user", "mongodb"),
    ] {
        let m = invalid(&format!("{base}{line}\n"));
        assert!(
            m.contains(&format!("`{key}`")) && m.contains(hint),
            "{key}: {m}"
        );
    }
    for (engine, keys) in [
        ("postgres", "host = \"h\"\ndatabase = \"d\"\nuser = \"u\"\n"),
        ("mysql", "host = \"h\"\nuser = \"u\"\n"),
        ("mssql", "host = \"h\"\nuser = \"u\"\n"),
        ("sqlite", "path = \"a.db\"\n"),
        ("duckdb", "path = \"a.duckdb\"\n"),
    ] {
        let m = invalid(&format!(
            "dbento = 1\n[connections.c]\nengine = \"{engine}\"\n{keys}uri = \"mongodb://h\"\n"
        ));
        assert!(
            m.contains("`uri` is a mongodb key, not valid for") && m.contains(engine),
            "{engine}: {m}"
        );
    }
}

#[test]
fn a_secret_needs_a_user_in_the_uri() {
    let anon = format!("{HEAD}uri = \"mongodb://localhost/x\"\n");
    for (toml, name, want) in [
        (
            format!("{MG}secret = \"none\"\n"),
            "shop",
            SecretSource::None,
        ),
        (
            format!("{MG}secret = {{ env = \"MG_PW\" }}\n"),
            "shop",
            SecretSource::Env("MG_PW".into()),
        ),
        (
            format!("{anon}secret = \"none\"\n"),
            "c",
            SecretSource::None,
        ),
    ] {
        let (_d, ws) = ws_with(&toml);
        assert_eq!(
            ws.unwrap().declared(name).unwrap().secret_source(),
            &want,
            "{toml}"
        );
    }
    for line in ["secret = \"keychain\"\n", "secret = { env = \"X\" }\n"] {
        assert!(
            invalid(&format!("{anon}{line}")).contains("`secret` needs a user in `uri`"),
            "{line}"
        );
    }
}

#[test]
fn capabilities_and_the_sql_helpers_fit_a_shell_language() {
    let m = Engine::Mongodb.capabilities();
    assert!(m.schemas && m.password && m.tls);
    assert!(!m.server_cancel && !m.file && !m.edit_rows && !m.explain && !m.table_definition);
    assert_eq!(m.language, QueryLanguage::MongoShell);
    let q = "db.t.find({a: 'it''s'}) // c";
    assert_eq!(
        (
            quote_ident(Engine::Mongodb, "a b"),
            quote_literal(Engine::Mongodb, "it's")
        ),
        ("a b".to_owned(), "it's".to_owned())
    );
    assert_eq!(format_sql(Engine::Mongodb, q, &FormatOptions::default()), q);
    assert_eq!(line_comment_starts(Engine::Mongodb, q), None);
    assert_eq!(single_source(Engine::Mongodb, q, Some("shop")), None);
    let unsupported = Some(ExplainError::UnsupportedEngine);
    assert_eq!(
        explain_request(Engine::Mongodb, q, ExplainMode::Plan).err(),
        unsupported
    );
    assert_eq!(explain_text(Engine::Mongodb, q).err(), unsupported);
}

#[cfg(not(feature = "mongodb"))]
#[tokio::test]
async fn without_the_feature_a_mongodb_connection_parses_but_does_not_connect() {
    assert_eq!(
        connect_error(MG).await,
        "MongoDB is not available in this build"
    );
}

#[test]
fn a_mongodb_entry_round_trips_through_a_save() {
    let (_dir, ws) = ws_with("dbento = 1\n");
    let mut spec = ConnectionSpec::new(Engine::Mongodb);
    spec.uri = Some("mongodb://app@h/shop".into());
    spec.secret = Some(SecretSource::Keychain);
    spec.read_only = Some(true);
    let edit = Edit::PutConnection {
        previous: None,
        name: "shop".into(),
        spec: spec.clone(),
    };
    let text = plan_edit(&ws.unwrap(), &edit)
        .unwrap()
        .new_text()
        .to_owned();
    assert!(text.contains("uri = \"mongodb://app@h/shop\""), "{text}");
    assert_eq!(
        ws_with(&text).1.unwrap().connection_spec("shop"),
        Some(spec)
    );
}

#[test]
fn a_changed_uri_is_a_different_session() {
    let other = MG.replace("authSource=admin", "authSource=shop");
    let (_a, a) = ws_with(MG);
    let (_b, b) = ws_with(&other);
    let (a, b) = (a.unwrap(), b.unwrap());
    let (a, b) = (a.declared("shop").unwrap(), b.declared("shop").unwrap());
    assert!(!a.same_session(b));
    assert!(a.same_session(a));
}

#[test]
fn a_file_that_does_not_parse_never_prints_its_own_text() {
    let uri = "mongodb://app:hunter2@h/shop";
    for (line, at) in [
        (format!("url = \"{uri}\"\n"), "line 4"),
        (format!("uri = \"{uri}\n"), "line 4"),
        (format!("{uri}\n"), "line 4"),
        (format!("port = \"{uri}\"\n"), "line 4"),
        (format!("uri = [\"{uri}\"]\n"), "line 4"),
        (format!("settings = \"{uri}\"\n"), "line 4"),
    ] {
        match ws_with(&format!("{HEAD}{line}")).1 {
            Err(e @ OpenError::Parse { .. }) => {
                let m = e.to_string();
                assert!(!m.contains("hunter2") && m.contains(at), "{line}: {m}");
            }
            other => panic!("{line}: {:?}", other.err()),
        }
    }
}

#[test]
fn a_bad_secret_word_is_never_printed() {
    let server = "dbento = 1\n[connections.c]\nengine = \"postgres\"\nhost = \"h\"\n\
                  database = \"d\"\nuser = \"u\"\n";
    for head in [server, MG] {
        let m = invalid(&format!("{head}secret = \"hunter2\"\n"));
        assert!(
            m.contains("secret must be") && !m.contains("hunter2"),
            "{m}"
        );
    }
}
