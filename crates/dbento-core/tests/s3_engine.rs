//! The S3 engine as the workspace and dialect layers see it, before any server.
use dbento_core::{
    ConnectError, ConnectionSpec, Edit, Engine, ExplainError, ExplainMode, FormatOptions,
    OpenError, QueryLanguage, ReadOnlyEnforcement, SecretSource, SessionOptions, Workspace,
    connect, default_schema, explain_request, explain_text, format_sql, line_comment_starts,
    plan_edit, quote_ident, quote_literal, read_only_residual, single_source, statement_count,
    statement_ranges,
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

const HEAD: &str = "dbento = 1\n[connections.assets]\nengine = \"s3\"\n";
const S3: &str = "dbento = 1\n[connections.assets]\nengine = \"s3\"\nregion = \"auto\"\n\
                  endpoint = \"http://127.0.0.1:5000\"\naccess_key_id = \"AKIDTEST\"\n\
                  secret = \"keychain\"\nread_only = true\n";

#[test]
fn an_s3_entry_opens_with_its_keys() {
    let (_d, ws) = open(S3);
    let c = ws.declared("assets").unwrap();
    assert_eq!((c.engine(), c.engine().name()), (Engine::S3, "s3"));
    assert_eq!(
        (c.region(), c.endpoint(), c.access_key_id()),
        (
            Some("auto"),
            Some("http://127.0.0.1:5000"),
            Some("AKIDTEST")
        )
    );
    assert!(c.read_only() && !c.consistent_read());
    assert_eq!(c.secret_source(), &SecretSource::Keychain);
    assert_eq!(
        (c.host(), c.port(), c.database(), c.user(), c.tls()),
        (None, None, None, None, None)
    );
    assert_eq!(c.display_target(), "auto at http://127.0.0.1:5000");
    assert_eq!(default_schema(Engine::S3, c), None);
    assert_eq!(c.read_only_enforcement(), ReadOnlyEnforcement::BestEffort);
}

#[test]
fn a_bare_entry_takes_everything_from_the_environment() {
    let (_d, ws) = open(HEAD);
    let c = ws.declared("assets").unwrap();
    assert_eq!(
        (c.region(), c.endpoint(), c.access_key_id()),
        (None, None, None)
    );
    assert_eq!(c.secret_source(), &SecretSource::None);
    assert_eq!(c.display_target(), "(region from environment)");
}

#[test]
fn an_access_key_takes_its_secret_from_the_keychain_or_a_variable_but_never_none() {
    let key = format!("{HEAD}access_key_id = \"AKIDTEST\"\n");
    let (_d, ws) = open(&format!("{key}secret = {{ env = \"ASSETS_SECRET\" }}\n"));
    assert_eq!(
        ws.declared("assets").unwrap().secret_source(),
        &SecretSource::Env("ASSETS_SECRET".into())
    );
    let m = invalid(&format!("{key}secret = \"none\"\n"));
    assert!(
        m.contains("not valid for s3") && m.contains("unsigned"),
        "{m}"
    );
    let m = invalid(&format!("{HEAD}secret = \"keychain\"\n"));
    assert!(m.contains("`secret` needs `access_key_id`"), "{m}");
}

#[test]
fn keys_of_other_engines_are_refused_with_a_reason() {
    for (line, want) in [
        ("host = \"h\"", "`host` is not valid for s3; use `endpoint`"),
        ("port = 9000", "`port` is not valid for s3; use `endpoint`"),
        ("database = \"d\"", "`database` is not valid for s3"),
        (
            "user = \"u\"",
            "`user` is not valid for s3; use `access_key_id`",
        ),
        ("path = \"x\"", "`path` is not valid for s3"),
        ("settings = { a = \"b\" }", "`settings` is not valid for s3"),
        ("tls = \"verify\"", "`tls` is not valid for s3"),
        ("tls_ca = \"ca.pem\"", "`tls_ca` is not valid for s3"),
        ("tls_cert = \"c.pem\"", "`tls_cert` is not valid for s3"),
        ("tls_key = \"k.pem\"", "`tls_key` is not valid for s3"),
        (
            "consistent_read = true",
            "`consistent_read` is a dynamodb key, not valid for s3",
        ),
        (
            "databases = \"all\"",
            "`databases` is a postgres and mssql key, not valid for s3",
        ),
        ("uri = \"x\"", "`uri` is a mongodb key, not valid for s3"),
        (
            "namespace = \"a\"",
            "`namespace` is a surrealdb key, not valid for s3",
        ),
        (
            "read_only_user = \"r\"",
            "`read_only_user` is a mssql key, not valid for s3",
        ),
    ] {
        let m = invalid(&format!("{HEAD}{line}\n"));
        assert!(m.contains(want), "{line}: {m}");
    }
    let m = invalid("dbento = 1\n[connections.x]\nengine = \"oracle\"\n");
    assert_eq!(
        m,
        "unknown engine `oracle`; use postgres, mysql, mssql, sqlite, duckdb, mongodb, surrealdb, redis, dynamodb or s3"
    );
}

#[test]
fn an_ssh_tunnel_is_refused() {
    // An unknown field before the SSH work merges, its per-engine refusal after.
    let text = format!("{HEAD}\n[connections.assets.ssh]\nhost = \"bastion\"\n");
    match ws_with(&text).1 {
        Err(e) => assert!(e.to_string().contains("ssh"), "{e}"),
        Ok(_) => panic!("an s3 connection took an ssh tunnel"),
    }
}

#[test]
fn the_shared_aws_keys_are_refused_on_engines_without_them() {
    for base in [
        "engine = \"sqlite\"\npath = \"a.db\"\n",
        "engine = \"redis\"\n",
        "engine = \"mongodb\"\nuri = \"mongodb://localhost/x\"\n",
        "engine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\n",
    ] {
        for line in [
            "region = \"auto\"",
            "endpoint = \"https://h\"",
            "access_key_id = \"A\"",
        ] {
            let m = invalid(&format!("dbento = 1\n[connections.c]\n{base}{line}\n"));
            assert!(
                m.contains("is a dynamodb and s3 key, not valid for"),
                "{base}{line}: {m}"
            );
        }
    }
}

#[test]
fn a_changed_region_endpoint_or_key_is_a_different_session() {
    let (_a, a) = open(S3);
    let a = a.declared("assets").unwrap();
    for (from, to) in [
        ("\"auto\"", "\"eu-west-1\""),
        ("127.0.0.1:5000", "127.0.0.1:5001"),
        ("AKIDTEST", "AKIDOTHER"),
        ("engine = \"s3\"", "engine = \"dynamodb\""),
    ] {
        let (_b, b) = open(&S3.replace(from, to));
        assert!(!a.same_session(b.declared("assets").unwrap()), "{from}");
    }
}

#[test]
fn a_region_or_access_key_must_not_be_blank_and_an_endpoint_follows_the_aws_rule() {
    for blank in ["", "  "] {
        let m = invalid(&format!("{HEAD}region = \"{blank}\"\n"));
        assert!(m.contains("`region` must not be empty"), "{m}");
        let m = invalid(&format!("{HEAD}access_key_id = \"{blank}\"\n"));
        assert!(m.contains("`access_key_id` must not be empty"), "{m}");
    }
    for ok in [
        "https://acct.r2.cloudflarestorage.com",
        "http://127.0.0.1:9000",
    ] {
        let (_d, ws) = open(&format!("{HEAD}endpoint = \"{ok}\"\n"));
        assert_eq!(ws.declared("assets").unwrap().endpoint(), Some(ok));
    }
    let m = invalid(&format!(
        "{HEAD}endpoint = \"http://minio.internal:9000\"\n"
    ));
    assert!(m.contains("use https"), "{m}");
    match ws_with(&format!("{HEAD}endpoint = \"https://u:hunter2@h\"\n")).1 {
        Err(OpenError::Secret { key, message, .. }) => {
            assert_eq!(key, "endpoint");
            assert!(!message.contains("hunter2"), "{message}");
        }
        other => panic!("{:?}", other.err()),
    }
}

#[test]
fn statement_ranges_are_the_s3_commands() {
    let text = "# list\nls 's3://b/a;b'\ncat 's3://b/k'";
    assert_eq!(statement_ranges(Engine::S3, text), [7..22, 23..text.len()]);
    assert_eq!(statement_count(Engine::S3, text), 2);
}

#[test]
fn capabilities_and_the_sql_helpers_fit_s3_commands() {
    let s = Engine::S3.capabilities();
    assert!(s.password, "{s:?}");
    assert!(
        !s.schemas
            && !s.tls
            && !s.server_cancel
            && !s.file
            && !s.edit_rows
            && !s.explain
            && !s.table_definition
            && !s.key_browser,
        "{s:?}"
    );
    assert_eq!(s.language, QueryLanguage::S3Cmd);
    let key = "s3://b/it's";
    assert_eq!(quote_ident(Engine::S3, key), "'s3://b/it''s'");
    assert_eq!(quote_literal(Engine::S3, key), "'s3://b/it''s'");
    let q = "ls  's3://b/'  # c";
    assert_eq!(format_sql(Engine::S3, q, &FormatOptions::default()), q);
    assert_eq!(line_comment_starts(Engine::S3, q), None);
    assert_eq!(single_source(Engine::S3, q, None), None);
    let unsupported = Some(ExplainError::UnsupportedEngine);
    assert_eq!(
        explain_request(Engine::S3, q, ExplainMode::Plan).err(),
        unsupported
    );
    assert_eq!(explain_text(Engine::S3, q).err(), unsupported);
}

#[test]
fn the_residual_names_the_reader_iam_actions() {
    let r = read_only_residual(Engine::S3).unwrap();
    for want in [
        "IAM",
        "s3:ListAllMyBuckets",
        "s3:ListBucket",
        "s3:GetObject",
    ] {
        assert!(r.contains(want), "{want}: {r}");
    }
}

#[tokio::test]
async fn connect_refuses_s3_until_the_driver_exists() {
    let (_d, ws) = open(S3);
    match connect(
        &ws,
        ws.declared("assets").unwrap(),
        Some(dbento_core::Secret::new("s")),
        SessionOptions::default(),
    )
    .await
    {
        Err(ConnectError::Unsupported { reason }) => assert!(reason.contains("S3"), "{reason}"),
        Err(other) => panic!("{other}"),
        Ok(_) => panic!("an S3 session opened"),
    }
}

#[test]
fn an_s3_entry_round_trips_through_a_save() {
    let (_dir, ws) = open("dbento = 1\n");
    let mut spec = ConnectionSpec::new(Engine::S3);
    spec.region = Some("auto".into());
    spec.endpoint = Some("https://acct.r2.cloudflarestorage.com".into());
    spec.access_key_id = Some("AKIDTEST".into());
    spec.secret = Some(SecretSource::Env("ASSETS_SECRET".into()));
    spec.read_only = Some(true);
    let edit = Edit::PutConnection {
        previous: None,
        name: "assets".into(),
        spec: spec.clone(),
    };
    let text = plan_edit(&ws, &edit).unwrap().new_text().to_owned();
    assert!(text.contains("engine = \"s3\""), "{text}");
    assert_eq!(open(&text).1.connection_spec("assets"), Some(spec));
}
