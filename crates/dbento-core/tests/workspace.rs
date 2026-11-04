// crates/dbento-core/tests/workspace.rs
use dbento_core::{Color, Engine, OpenError, SecretSource, TlsMode, Workspace};
use std::fs;
use std::time::Duration;

fn ws_with(toml: &str) -> (tempfile::TempDir, Result<Workspace, OpenError>) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path());
    (dir, ws)
}

fn open_err(toml: &str) -> OpenError {
    ws_with(toml).1.expect_err("should be rejected")
}

const PG: &str = "dbento = 1\n[connections.p]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\n";

const FULL: &str = r#"
dbento = 1

[connections.prod]
engine = "postgres"
host = "db.example.com"
database = "app"
user = "app_ro"
secret = { env = "PROD_PW" }
tls_ca = "certs/rds.pem"
read_only = true
color = "red"
statement_timeout = "30s"
settings = { search_path = "app, public", TimeZone = "UTC", "app.tenant" = "7" }

[connections.local-pg]
engine = "postgres"
host = "127.0.0.1"
port = 5433
database = "dev"
user = "me"
secret = "none"

[connections.sock]
engine = "postgres"
host = "/run/postgresql"
database = "dev"
user = "me"

[connections.scratch]
engine = "sqlite"
path = "data/app.db"
"#;

#[test]
fn a_full_workspace_opens_with_every_field() {
    let (dir, ws) = ws_with(FULL);
    let ws = ws.unwrap();
    assert_eq!(ws.dir(), fs::canonicalize(dir.path()).unwrap());
    let names: Vec<&str> = ws.connections().iter().map(|c| c.name()).collect();
    assert_eq!(names, ["local-pg", "prod", "scratch", "sock"]);
    let prod = ws.connection("prod").unwrap();
    assert_eq!(prod.engine(), Engine::Postgres);
    assert!(prod.read_only());
    assert_eq!(prod.color(), Some(Color::Red));
    assert_eq!(prod.statement_timeout(), Some(Duration::from_secs(30)));
    assert_eq!(
        prod.secret_source(),
        &SecretSource::Env("PROD_PW".to_owned())
    );
    assert_eq!(prod.display_target(), "app_ro@db.example.com:5432/app");
    assert_eq!(
        (prod.host(), prod.port(), prod.database(), prod.user()),
        (
            Some("db.example.com"),
            Some(5432),
            Some("app"),
            Some("app_ro")
        )
    );
    assert_eq!(
        prod.tls_ca(),
        Some(ws.dir().join("certs/rds.pem").as_path())
    );
    assert_eq!(
        prod.settings().get("search_path").map(String::as_str),
        Some("app, public")
    );
    assert_eq!(
        prod.settings().get("app.tenant").map(String::as_str),
        Some("7")
    );
    let local = ws.connection("local-pg").unwrap();
    assert!(!local.read_only());
    assert_eq!(
        (local.color(), local.statement_timeout(), local.tls_ca()),
        (None, None, None)
    );
    assert_eq!(local.secret_source(), &SecretSource::None);
    assert_eq!(local.display_target(), "me@127.0.0.1:5433/dev");
    assert!(local.settings().is_empty());
    assert_eq!(
        ws.connection("sock").unwrap().secret_source(),
        &SecretSource::Keychain
    );
    let scratch = ws.connection("scratch").unwrap();
    assert_eq!(scratch.engine(), Engine::Sqlite);
    assert_eq!(scratch.secret_source(), &SecretSource::None);
    assert_eq!(
        scratch.sqlite_path(),
        Some(ws.dir().join("data/app.db").as_path())
    );
    assert_eq!(scratch.display_target(), "data/app.db");
    assert_eq!((scratch.host(), scratch.port()), (None, None));
    assert!(ws.connection("missing").is_none());
}

#[test]
fn tls_defaults_to_off_only_for_loopback_and_sockets() {
    let (_d, ws) = ws_with(FULL);
    let ws = ws.unwrap();
    assert_eq!(ws.connection("prod").unwrap().tls(), Some(TlsMode::Verify));
    assert_eq!(ws.connection("local-pg").unwrap().tls(), Some(TlsMode::Off));
    assert_eq!(ws.connection("sock").unwrap().tls(), Some(TlsMode::Off));
    assert_eq!(ws.connection("scratch").unwrap().tls(), None);
    for (host, extra, want) in [
        ("localhost", "", TlsMode::Off),
        ("::1", "", TlsMode::Off),
        ("localhost", "tls = \"verify\"\n", TlsMode::Verify),
    ] {
        let toml = format!(
            "dbento = 1\n[connections.l]\nengine = \"postgres\"\nhost = \"{host}\"\ndatabase = \"d\"\nuser = \"u\"\n{extra}"
        );
        let (_d, ws) = ws_with(&toml);
        assert_eq!(
            ws.unwrap().connection("l").unwrap().tls(),
            Some(want),
            "{host} {extra}"
        );
    }
}

#[test]
fn a_password_key_is_rejected_with_the_secret_set_hint() {
    let err = open_err(&format!("{PG}password = \"hunter2\"\n"));
    assert!(
        matches!(err, OpenError::Password { ref key } if key == "connections.p.password"),
        "{err:?}"
    );
    let text = err.to_string();
    assert!(text.contains("dbento secret set"), "{text}");
    assert!(!text.contains("hunter2"), "{text}");
    let err = open_err("dbento = 1\npassword = \"x\"\n");
    assert!(matches!(err, OpenError::Password { .. }), "{err:?}");
    let err = open_err(&format!("{PG}settings = {{ password = \"x\" }}\n"));
    assert!(matches!(err, OpenError::Password { .. }), "{err:?}");
}

#[test]
fn unknown_keys_and_keys_of_the_other_engine_are_rejected() {
    let err = open_err("dbento = 1\n[connections.s]\nengine = \"sqlite\"\npaht = \"x.db\"\n");
    assert!(matches!(err, OpenError::Parse { .. }), "{err:?}");
    assert!(err.to_string().contains("paht"), "{err}");
    let err = open_err(
        "dbento = 1\n[connections.s]\nengine = \"sqlite\"\npath = \"x.db\"\nhost = \"h\"\n",
    );
    assert!(
        matches!(err, OpenError::Invalid { ref connection, .. } if connection == "s"),
        "{err:?}"
    );
}

#[test]
fn bad_connection_names_are_rejected() {
    for name in ["\"a b\"", "\"é\"", "\"a.b\"", "\"\""] {
        let err = open_err(&format!(
            "dbento = 1\n[connections.{name}]\nengine = \"sqlite\"\npath = \"x.db\"\n"
        ));
        assert!(
            matches!(err, OpenError::InvalidName { .. }),
            "{name}: {err:?}"
        );
    }
}

#[test]
fn the_format_version_must_be_known() {
    assert!(matches!(
        open_err("dbento = 2\n"),
        OpenError::UnsupportedVersion { found: 2 }
    ));
    assert!(matches!(
        open_err("[connections]\n"),
        OpenError::Parse { .. }
    ));
}

#[test]
fn settings_names_must_be_identifiers_and_not_reserved() {
    let keys = [
        "\"bad name\"",
        "\"1abc\"",
        "\"a-b\"",
        "statement_timeout",
        "Client_Encoding",
        "bytea_output",
        "default_transaction_read_only",
    ];
    for key in keys {
        let err = open_err(&format!("{PG}settings = {{ {key} = \"x\" }}\n"));
        assert!(matches!(err, OpenError::Invalid { .. }), "{key}: {err:?}");
    }
}

#[test]
fn malformed_values_are_rejected() {
    let extras = [
        "statement_timeout = \"soon\"",
        "color = \"pink\"",
        "secret = \"vault\"",
        "secret = { file = \"x\" }",
        "secret = { env = \"X\", extra = 1 }",
        "port = \"5432\"",
    ];
    for extra in extras {
        let err = open_err(&format!("{PG}{extra}\n"));
        assert!(
            matches!(err, OpenError::Parse { .. } | OpenError::Invalid { .. }),
            "{extra}: {err:?}"
        );
    }
    for toml in [
        "dbento = 1\n[connections.s]\nengine = \"mysql\"\n",
        "dbento = 1\n[connections.s]\nengine = \"sqlite\"\n",
        "dbento = 1\n[connections.p]\nengine = \"postgres\"\nhost = \"h\"\nuser = \"u\"\n",
    ] {
        let err = open_err(toml);
        assert!(
            matches!(err, OpenError::Parse { .. } | OpenError::Invalid { .. }),
            "{toml}: {err:?}"
        );
    }
}

#[test]
fn an_engine_is_named_as_dbento_toml_names_it() {
    let ws = ws_with(FULL).1.unwrap();
    assert_eq!(ws.connection("prod").unwrap().engine().name(), "postgres");
    assert_eq!(ws.connection("scratch").unwrap().engine().name(), "sqlite");
}
