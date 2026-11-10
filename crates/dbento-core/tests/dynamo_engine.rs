//! The DynamoDB engine as the workspace and dialect layers see it, before any server.
use dbento_core::{
    ConnectError, Engine, ExplainError, ExplainMode, FormatOptions, OpenError, QueryLanguage,
    ReadOnlyEnforcement, SecretSource, SessionOptions, Workspace, connect, default_schema,
    explain_request, explain_text, format_sql, line_comment_starts, quote_ident, quote_literal,
    read_only_residual, single_source, statement_count, statement_ranges,
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

const HEAD: &str = "dbento = 1\n[connections.orders]\nengine = \"dynamodb\"\n";
const DY: &str = "dbento = 1\n[connections.orders]\nengine = \"dynamodb\"\nregion = \"eu-west-1\"\n\
                  endpoint = \"http://127.0.0.1:5000\"\naccess_key_id = \"AKIDTEST\"\n\
                  secret = \"keychain\"\nconsistent_read = true\nread_only = true\n";

#[test]
fn a_dynamodb_entry_opens_with_its_keys() {
    let (_d, ws) = open(DY);
    let c = ws.connection("orders").unwrap();
    assert_eq!(
        (c.engine(), c.engine().name()),
        (Engine::Dynamodb, "dynamodb")
    );
    assert_eq!(
        (c.region(), c.endpoint(), c.access_key_id()),
        (
            Some("eu-west-1"),
            Some("http://127.0.0.1:5000"),
            Some("AKIDTEST")
        )
    );
    assert!(c.consistent_read() && c.read_only());
    assert_eq!(c.secret_source(), &SecretSource::Keychain);
    assert_eq!(
        (c.host(), c.port(), c.database(), c.user(), c.tls()),
        (None, None, None, None, None)
    );
    assert_eq!(c.display_target(), "eu-west-1 at http://127.0.0.1:5000");
    assert_eq!(
        default_schema(Engine::Dynamodb, c).as_deref(),
        Some("eu-west-1")
    );
    assert_eq!(c.read_only_enforcement(), ReadOnlyEnforcement::BestEffort);
}

#[test]
fn a_bare_entry_takes_everything_from_the_environment() {
    let (_d, ws) = open("dbento = 1\n[connections.orders]\nengine = \"dynamo\"\n");
    let c = ws.connection("orders").unwrap();
    assert_eq!(c.engine(), Engine::Dynamodb);
    assert_eq!(
        (
            c.region(),
            c.endpoint(),
            c.access_key_id(),
            c.consistent_read()
        ),
        (None, None, None, false)
    );
    assert_eq!(c.secret_source(), &SecretSource::None);
    assert_eq!(c.display_target(), "(region from environment)");
    assert_eq!(default_schema(Engine::Dynamodb, c), None);
    let (_d, ws) = open(&format!("{HEAD}region = \"us-east-1\"\n"));
    assert_eq!(
        ws.connection("orders").unwrap().display_target(),
        "us-east-1"
    );
}

#[test]
fn an_access_key_takes_its_secret_from_the_keychain_or_a_variable_but_never_none() {
    let key = format!("{HEAD}access_key_id = \"AKIDTEST\"\n");
    let (_d, ws) = open(&key);
    assert_eq!(
        ws.connection("orders").unwrap().secret_source(),
        &SecretSource::Keychain
    );
    let (_d, ws) = open(&format!("{key}secret = {{ env = \"ORDERS_SECRET\" }}\n"));
    assert_eq!(
        ws.connection("orders").unwrap().secret_source(),
        &SecretSource::Env("ORDERS_SECRET".into())
    );
    let m = invalid(&format!("{key}secret = \"none\"\n"));
    assert!(m.contains("unsigned"), "{m}");
    let m = invalid(&format!("{HEAD}secret = \"keychain\"\n"));
    assert!(m.contains("`secret` needs `access_key_id`"), "{m}");
}

#[test]
fn keys_of_other_engines_are_refused_with_a_reason() {
    for (line, want) in [
        (
            "host = \"h\"",
            "`host` is not valid for dynamodb; use `endpoint`",
        ),
        (
            "port = 8000",
            "`port` is not valid for dynamodb; use `endpoint`",
        ),
        ("database = \"d\"", "`database` is not valid for dynamodb"),
        (
            "user = \"u\"",
            "`user` is not valid for dynamodb; use `access_key_id`",
        ),
        ("path = \"x\"", "`path` is not valid for dynamodb"),
        (
            "settings = { a = \"b\" }",
            "`settings` is not valid for dynamodb",
        ),
        ("tls = \"verify\"", "`tls` is not valid for dynamodb"),
        ("tls_ca = \"ca.pem\"", "`tls_ca` is not valid for dynamodb"),
        (
            "tls_cert = \"c.pem\"",
            "`tls_cert` is not valid for dynamodb",
        ),
        ("tls_key = \"k.pem\"", "`tls_key` is not valid for dynamodb"),
        (
            "uri = \"x\"",
            "`uri` is a mongodb key, not valid for dynamodb",
        ),
        (
            "namespace = \"a\"",
            "`namespace` is a surrealdb key, not valid for dynamodb",
        ),
        (
            "auth_level = \"root\"",
            "`auth_level` is a surrealdb key, not valid for dynamodb",
        ),
        (
            "read_only_user = \"r\"",
            "`read_only_user` is a mssql key, not valid for dynamodb",
        ),
    ] {
        let m = invalid(&format!("{HEAD}{line}\n"));
        assert!(m.contains(want), "{line}: {m}");
    }
    let m = invalid("dbento = 1\n[connections.x]\nengine = \"oracle\"\n");
    assert!(m.contains("dynamodb"), "{m}");
}

#[test]
fn the_dynamodb_keys_are_refused_on_every_other_engine() {
    for base in [
        "engine = \"sqlite\"\npath = \"a.db\"\n",
        "engine = \"redis\"\n",
        "engine = \"mongodb\"\nuri = \"mongodb://localhost/x\"\n",
        "engine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\n",
    ] {
        for line in [
            "region = \"eu-west-1\"",
            "endpoint = \"https://h\"",
            "access_key_id = \"A\"",
            "consistent_read = true",
        ] {
            let m = invalid(&format!("dbento = 1\n[connections.c]\n{base}{line}\n"));
            assert!(
                m.contains("is a dynamodb key, not valid for"),
                "{base}{line}: {m}"
            );
        }
    }
}

#[test]
fn a_changed_region_endpoint_key_or_consistency_is_a_different_session() {
    let (_a, a) = open(DY);
    let a = a.connection("orders").unwrap();
    assert!(a.same_session(a));
    for (from, to) in [
        ("eu-west-1", "us-east-1"),
        ("127.0.0.1:5000", "127.0.0.1:5001"),
        ("AKIDTEST", "AKIDOTHER"),
        ("consistent_read = true", "consistent_read = false"),
    ] {
        let (_b, b) = open(&DY.replace(from, to));
        assert!(!a.same_session(b.connection("orders").unwrap()), "{from}");
    }
}

#[test]
fn statement_ranges_are_the_partiql_statements() {
    let text = "-- c\nSELECT * FROM \"t\";\nUPDATE t SET a = 'x;y' WHERE pk = '1'";
    assert_eq!(
        statement_ranges(Engine::Dynamodb, text),
        [5..22, 24..text.len()]
    );
    assert_eq!(statement_count(Engine::Dynamodb, text), 2);
}

#[test]
fn capabilities_and_the_sql_helpers_fit_partiql() {
    let d = Engine::Dynamodb.capabilities();
    assert!(d.password, "{d:?}");
    assert!(
        !d.schemas
            && !d.tls
            && !d.server_cancel
            && !d.file
            && !d.edit_rows
            && !d.explain
            && !d.table_definition
            && !d.key_browser,
        "{d:?}"
    );
    assert_eq!(d.language, QueryLanguage::PartiQl);
    assert_eq!(quote_ident(Engine::Dynamodb, "orders"), "\"orders\"");
    assert_eq!(
        (
            quote_ident(Engine::Dynamodb, "a\"b"),
            quote_literal(Engine::Dynamodb, "it's")
        ),
        ("\"a\"\"b\"".to_owned(), "'it''s'".to_owned())
    );
    let q = "SELECT  *  FROM \"orders\" -- c";
    assert_eq!(
        format_sql(Engine::Dynamodb, q, &FormatOptions::default()),
        q
    );
    assert_eq!(line_comment_starts(Engine::Dynamodb, q), None);
    assert_eq!(single_source(Engine::Dynamodb, q, Some("eu-west-1")), None);
    let unsupported = Some(ExplainError::UnsupportedEngine);
    assert_eq!(
        explain_request(Engine::Dynamodb, q, ExplainMode::Plan).err(),
        unsupported
    );
    assert_eq!(explain_text(Engine::Dynamodb, q).err(), unsupported);
}

#[test]
fn the_residual_says_iam_is_out_of_sight() {
    let r = read_only_residual(Engine::Dynamodb).unwrap();
    assert!(
        r.contains("IAM") && r.contains("dynamodb:PartiQLSelect"),
        "{r}"
    );
}

#[tokio::test]
async fn connect_refuses_dynamodb_until_the_driver_exists() {
    let (_d, ws) = open(DY);
    match connect(
        &ws,
        ws.connection("orders").unwrap(),
        None,
        SessionOptions::default(),
    )
    .await
    {
        Err(ConnectError::Unsupported { reason }) => {
            assert!(reason.contains("DynamoDB"), "{reason}")
        }
        Err(other) => panic!("{other}"),
        Ok(_) => panic!("a DynamoDB session opened"),
    }
}
