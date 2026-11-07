//! The connection form descriptor agrees with the dbento.toml parser for every engine.
use dbento_core::{Engine, FieldKind, FormField, Workspace, connection_form};

const ENGINES: [Engine; 8] = [
    Engine::Postgres,
    Engine::Sqlite,
    Engine::Mysql,
    Engine::Mssql,
    Engine::Duckdb,
    Engine::Mongodb,
    Engine::Surrealdb,
    Engine::Redis,
];
/// Engine-specific keys the parser knows; the TLS kind's file keys and `settings` are not
/// form fields of their own.
const SERVER_KEYS: [&str; 11] = [
    "path",
    "host",
    "port",
    "database",
    "user",
    "secret",
    "tls",
    "read_only_user",
    "uri",
    "namespace",
    "auth_level",
];

fn fields(engine: Engine) -> Vec<FormField> {
    connection_form(engine)
        .sections
        .into_iter()
        .flat_map(|s| s.fields)
        .collect()
}

fn line(key: &str, kind: Option<FieldKind>) -> String {
    let value = match (key, kind) {
        (_, Some(FieldKind::Port { default })) => default.to_string(),
        ("port", _) => "1".to_owned(),
        ("secret", _) => "\"none\"".to_owned(),
        ("tls", _) => "\"off\"".to_owned(),
        ("uri", _) => "\"mongodb://localhost/x\"".to_owned(),
        ("auth_level", _) => "\"root\"".to_owned(),
        ("database", _) => "\"0\"".to_owned(),
        _ => "\"x.db\"".to_owned(),
    };
    format!("{key} = {value}")
}

fn lines(fields: &[FormField], only_required: bool) -> Vec<String> {
    fields
        .iter()
        .filter(|f| f.required || !only_required)
        .map(|f| line(f.key, Some(f.kind)))
        .collect()
}

fn parses(engine: Engine, lines: &[String]) -> Result<Workspace, String> {
    let dir = tempfile::tempdir().unwrap();
    let toml = format!(
        "dbento = 1\n[connections.c]\nengine = \"{}\"\n{}\n",
        engine.name(),
        lines.join("\n")
    );
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    Workspace::open(dir.path()).map_err(|e| e.to_string())
}

#[test]
fn every_descriptor_key_parses_and_every_required_key_is_required() {
    for engine in ENGINES {
        let fields = fields(engine);
        let all = lines(&fields, false);
        assert!(
            parses(engine, &all).is_ok(),
            "{engine:?}: {:?}",
            parses(engine, &all).err()
        );
        let required = lines(&fields, true);
        assert!(
            parses(engine, &required).is_ok(),
            "{engine:?} with only required keys"
        );
        for (i, f) in fields.iter().enumerate().filter(|(_, f)| f.required) {
            let mut without = all.clone();
            without.remove(i);
            assert!(
                parses(engine, &without).is_err(),
                "{engine:?} parses without required `{}`",
                f.key
            );
        }
    }
}

#[test]
fn the_parser_refuses_every_server_key_the_descriptor_leaves_out() {
    for engine in ENGINES {
        let fields = fields(engine);
        for key in SERVER_KEYS {
            if fields.iter().any(|f| f.key == key) {
                continue;
            }
            let mut with = lines(&fields, true);
            with.push(line(key, None));
            assert!(parses(engine, &with).is_err(), "{engine:?} accepts `{key}`");
        }
    }
}

#[test]
fn a_port_left_out_is_the_descriptor_default() {
    for (engine, want) in [
        (Engine::Postgres, 5432),
        (Engine::Mysql, 3306),
        (Engine::Mssql, 1433),
        (Engine::Surrealdb, 8000),
        (Engine::Redis, 6379),
    ] {
        let fields = fields(engine);
        let default = fields.iter().find_map(|f| match f.kind {
            FieldKind::Port { default } => Some(default),
            _ => None,
        });
        assert_eq!(default, Some(want), "{engine:?}");
        let ws = parses(engine, &lines(&fields, true)).unwrap();
        assert_eq!(ws.connection("c").unwrap().port(), Some(want), "{engine:?}");
    }
}

#[test]
fn the_descriptors_pin_their_fields_sections_and_help() {
    let keys = |e: Engine| -> Vec<(&'static str, bool)> {
        fields(e).into_iter().map(|f| (f.key, f.required)).collect()
    };
    let server = [
        ("host", true),
        ("port", false),
        ("database", true),
        ("user", true),
        ("secret", false),
        ("tls", false),
    ];
    assert_eq!(keys(Engine::Postgres), server);
    let mut my = server;
    my[2].1 = false;
    assert_eq!(keys(Engine::Mysql), my);
    assert_eq!(keys(Engine::Sqlite), [("path", true)]);
    let titles = |e: Engine| -> Vec<&'static str> {
        connection_form(e)
            .sections
            .iter()
            .map(|s| s.title)
            .collect()
    };
    assert_eq!(
        titles(Engine::Postgres),
        ["Connection", "Authentication", "Options"]
    );
    assert_eq!(
        titles(Engine::Mysql),
        ["Connection", "Authentication", "Options"]
    );
    assert_eq!(titles(Engine::Sqlite), ["Connection"]);
    assert!(
        connection_form(Engine::Mysql)
            .read_only_help
            .is_some_and(|h| h.contains("only SELECT"))
    );
    assert_eq!(connection_form(Engine::Postgres).read_only_help, None);
    assert_eq!(connection_form(Engine::Sqlite).read_only_help, None);
    let database = fields(Engine::Mysql)
        .into_iter()
        .find(|f| f.key == "database")
        .unwrap();
    assert!(database.help.is_some());
}

#[test]
fn the_sql_server_descriptor_pins_its_fields_sections_and_help() {
    let ms = fields(Engine::Mssql);
    let keys: Vec<(&str, bool)> = ms.iter().map(|f| (f.key, f.required)).collect();
    assert_eq!(
        keys,
        [
            ("host", true),
            ("port", false),
            ("database", false),
            ("user", true),
            ("secret", false),
            ("tls", false),
            ("read_only_user", false)
        ]
    );
    let kinds: Vec<FieldKind> = ms.iter().map(|f| f.kind).collect();
    assert_eq!(
        kinds,
        [
            FieldKind::Text,
            FieldKind::Port { default: 1433 },
            FieldKind::Text,
            FieldKind::Text,
            FieldKind::Secret,
            FieldKind::Tls,
            FieldKind::Text
        ]
    );
    let form = connection_form(Engine::Mssql);
    let titles: Vec<&str> = form.sections.iter().map(|s| s.title).collect();
    assert_eq!(titles, ["Connection", "Secret", "TLS", "Read-only"]);
    assert!(
        form.read_only_help
            .is_some_and(|h| h.contains("best effort") && h.contains("reader"))
    );
    let help = |key: &str| ms.iter().find(|f| f.key == key).and_then(|f| f.help);
    assert!(
        help("tls").is_some_and(|h| h.contains("not encrypted")),
        "{:?}",
        help("tls")
    );
    assert!(help("read_only_user").is_some_and(|h| h.contains("IMPERSONATE")));
    assert!(help("database").is_some());
}

#[test]
fn the_mongodb_descriptor_is_one_uri_and_a_secret() {
    let mg = fields(Engine::Mongodb);
    let keys: Vec<(&str, bool, FieldKind)> =
        mg.iter().map(|f| (f.key, f.required, f.kind)).collect();
    assert_eq!(
        keys,
        [
            ("uri", true, FieldKind::Uri),
            ("secret", false, FieldKind::Secret)
        ]
    );
    let form = connection_form(Engine::Mongodb);
    let titles: Vec<&str> = form.sections.iter().map(|s| s.title).collect();
    assert_eq!(titles, ["Connection", "Secret"]);
    assert!(
        form.read_only_help
            .is_some_and(|h| h.contains("privileges"))
    );
    assert!(mg[0].help.is_some_and(|h| h.contains("keychain")));
}

#[test]
fn the_surrealdb_descriptor_pins_its_fields_sections_and_help() {
    let sr = fields(Engine::Surrealdb);
    let keys: Vec<(&str, bool, FieldKind)> =
        sr.iter().map(|f| (f.key, f.required, f.kind)).collect();
    assert_eq!(
        keys,
        [
            ("host", true, FieldKind::Text),
            ("port", false, FieldKind::Port { default: 8000 }),
            ("namespace", true, FieldKind::Text),
            ("database", true, FieldKind::Text),
            ("user", false, FieldKind::Text),
            ("auth_level", false, FieldKind::Text),
            ("secret", false, FieldKind::Secret),
            ("tls", false, FieldKind::Tls),
        ]
    );
    let form = connection_form(Engine::Surrealdb);
    let titles: Vec<&str> = form.sections.iter().map(|s| s.title).collect();
    assert_eq!(titles, ["Connection", "Secret", "TLS"]);
    assert!(form.read_only_help.is_some_and(|h| h.contains("cancels")
        && h.contains("anonymous")
        && h.contains("ALTER SYSTEM")));
    let help = |key: &str| sr.iter().find(|f| f.key == key).and_then(|f| f.help);
    assert!(help("auth_level").is_some_and(|h| h.contains("root, namespace or database")));
    assert!(help("user").is_some_and(|h| h.contains("anonymous")));
}

#[test]
fn the_redis_descriptor_pins_its_fields_sections_and_help() {
    let rd = fields(Engine::Redis);
    let keys: Vec<(&str, &str, bool, FieldKind)> = rd
        .iter()
        .map(|f| (f.key, f.label, f.required, f.kind))
        .collect();
    assert_eq!(
        keys,
        [
            ("host", "Host", false, FieldKind::Text),
            ("port", "Port", false, FieldKind::Port { default: 6379 }),
            ("database", "DB index", false, FieldKind::Text),
            ("user", "User", false, FieldKind::Text),
            ("secret", "Password", false, FieldKind::Secret),
            ("tls", "TLS", false, FieldKind::Tls),
        ]
    );
    let form = connection_form(Engine::Redis);
    let titles: Vec<&str> = form.sections.iter().map(|s| s.title).collect();
    assert_eq!(titles, ["Connection", "Authentication", "Options"]);
    let help = form.read_only_help.unwrap();
    let pinned = help.contains("ACL SETUSER dbento_ro") && help.contains("+multi +discard");
    assert!(pinned && !help.contains("@connection"), "{help}");
    let host = rd.iter().find(|f| f.key == "host").unwrap();
    assert_eq!(host.placeholder, "127.0.0.1");
}
