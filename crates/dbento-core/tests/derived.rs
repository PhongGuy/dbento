//! Derived connections: a parent's `databases` key, the children it defines, the secrets they
//! share and the saved queries that bind to them.
use dbento_core::{OpenError, Workspace};

fn ws_with(toml: &str) -> (tempfile::TempDir, Result<Workspace, OpenError>) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path());
    (dir, ws)
}

fn open(toml: &str) -> (tempfile::TempDir, Workspace) {
    let (dir, ws) = ws_with(toml);
    (dir, ws.expect("the workspace opens"))
}

/// The message of the `Invalid` error `toml` fails to load with.
fn invalid(toml: &str) -> String {
    match ws_with(toml).1 {
        Err(OpenError::Invalid { message, .. }) => message,
        other => panic!("expected Invalid, got {other:?}"),
    }
}

/// `[connections.<name>]` on a server engine, with `extra` lines.
fn server(name: &str, engine: &str, extra: &str) -> String {
    format!(
        "[connections.{name}]\nengine = \"{engine}\"\nhost = \"db.internal\"\ndatabase = \"app\"\nuser = \"app\"\n{extra}\n"
    )
}

mod load {
    use super::{invalid, open, server};
    use dbento_core::Databases;

    #[test]
    fn a_parent_lists_its_databases_or_takes_all() {
        let (_d, ws) = open(&format!(
            "dbento = 1\n{}{}{}",
            server("pg", "postgres", "databases = [\"booking\", \"user_db\"]"),
            server("ms", "mssql", "databases = \"all\""),
            server("plain", "postgres", ""),
        ));
        assert_eq!(
            ws.declared("pg").unwrap().databases(),
            Some(&Databases::Listed(vec![
                "booking".to_owned(),
                "user_db".to_owned()
            ]))
        );
        assert_eq!(
            ws.declared("ms").unwrap().databases(),
            Some(&Databases::All)
        );
        assert_eq!(ws.declared("plain").unwrap().databases(), None);
        assert_eq!(
            ws.declared("pg").unwrap().database(),
            Some("app"),
            "the parent keeps its own default database"
        );
    }

    #[test]
    fn databases_is_refused_outside_postgres_and_sql_server() {
        assert_eq!(
            invalid(&format!(
                "dbento = 1\n{}",
                server("m", "mysql", "databases = \"all\"")
            )),
            "`databases` is not valid for mysql: leave `database` empty to browse every database as a schema"
        );
        for (engine, keys) in [
            ("sqlite", "path = \"x.db\""),
            ("duckdb", "path = \"x.duckdb\""),
            ("mongodb", "uri = \"mongodb://h\""),
            ("surrealdb", "host = \"h\""),
            ("redis", "host = \"h\""),
            ("dynamodb", "region = \"eu-west-1\""),
        ] {
            let toml = format!(
                "dbento = 1\n[connections.x]\nengine = \"{engine}\"\n{keys}\ndatabases = \"all\"\n"
            );
            assert_eq!(
                invalid(&toml),
                format!("`databases` is a postgres and mssql key, not valid for {engine}"),
                "{engine}"
            );
        }
    }

    #[test]
    fn a_bad_value_does_not_load() {
        let entry = |x: &str| format!("`databases` entry `{x}` must match [A-Za-z0-9_-]+");
        let word = "`databases` must be \"all\" or a list of database names".to_owned();
        for (value, says) in [
            (
                "[]",
                "`databases` lists no database; use \"all\" or name at least one".to_owned(),
            ),
            (
                "[\"a\", \"b\", \"a\"]",
                "`databases` lists `a` twice".to_owned(),
            ),
            ("[\"my.db\"]", entry("my.db")),
            ("[\"a/b\"]", entry("a/b")),
            ("[\"\"]", entry("")),
            ("\"every\"", word.clone()),
            ("\"ALL\"", word),
        ] {
            let toml = format!(
                "dbento = 1\n{}",
                server("pg", "postgres", &format!("databases = {value}"))
            );
            assert_eq!(invalid(&toml), says, "{value}");
        }
    }

    #[test]
    fn sql_server_refuses_names_that_differ_only_in_case() {
        let toml = format!(
            "dbento = 1\n{}",
            server("ms", "mssql", "databases = [\"Booking\", \"booking\"]")
        );
        assert_eq!(
            invalid(&toml),
            "`databases` lists `Booking` and `booking`, which SQL Server treats as one database"
        );
        let (_d, ws) = open(&format!(
            "dbento = 1\n{}",
            server("pg", "postgres", "databases = [\"Booking\", \"booking\"]")
        ));
        assert_eq!(
            ws.declared("pg").unwrap().databases(),
            Some(&Databases::Listed(vec![
                "Booking".to_owned(),
                "booking".to_owned()
            ])),
            "Postgres names are case-sensitive, so both are distinct"
        );
    }
}

mod spec {
    use super::{open, server};
    use dbento_core::{Color, ConnectionSpec, Databases, Edit, Engine, Workspace, plan_edit};

    fn rewrite(ws: &Workspace, name: &str, spec: ConnectionSpec) -> String {
        plan_edit(
            ws,
            &Edit::PutConnection {
                previous: Some(name.into()),
                name: name.into(),
                spec,
            },
        )
        .expect("the edit plans")
        .new_text()
        .to_owned()
    }

    #[test]
    fn the_spec_reads_databases_and_an_edit_keeps_it() {
        let (_d, ws) = open(&format!(
            "dbento = 1\n{}",
            server("pg", "postgres", "databases = [\"booking\", \"user_db\"]")
        ));
        let mut spec = ws.connection_spec("pg").unwrap();
        assert_eq!(
            spec.databases,
            Some(Databases::Listed(vec!["booking".into(), "user_db".into()]))
        );
        spec.color = Some(Color::Red);
        let text = rewrite(&ws, "pg", spec);
        assert!(
            text.contains("databases = [\"booking\", \"user_db\"]"),
            "{text}"
        );
        assert!(text.contains("color = \"red\""), "{text}");
    }

    #[test]
    fn setting_databases_writes_the_word_or_the_list() {
        let (_d, ws) = open(&format!("dbento = 1\n{}", server("pg", "postgres", "")));
        let mut spec = ws.connection_spec("pg").unwrap();
        spec.databases = Some(Databases::All);
        let text = rewrite(&ws, "pg", spec.clone());
        assert!(text.contains("databases = \"all\""), "{text}");
        spec.databases = Some(Databases::Listed(vec!["a".into()]));
        let text = rewrite(&ws, "pg", spec);
        assert!(text.contains("databases = [\"a\"]"), "{text}");
    }

    #[test]
    fn the_key_survives_sql_server_and_goes_on_mysql() {
        let (_d, ws) = open(&format!(
            "dbento = 1\n{}",
            server("pg", "postgres", "databases = \"all\"")
        ));
        let mut spec = ws.connection_spec("pg").unwrap();
        spec.engine = Engine::Mssql;
        let text = rewrite(&ws, "pg", spec.clone());
        assert!(text.contains("databases = \"all\""), "{text}");
        spec.engine = Engine::Mysql;
        let text = rewrite(&ws, "pg", spec);
        assert!(!text.contains("databases"), "{text}");
        assert!(text.contains("engine = \"mysql\""), "{text}");
    }
}

mod targets {
    use super::open;
    use dbento_core::{Edit, EditError, SecretKey, WorkspaceItem, plan_edit};

    const PARENT: &str = "dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\ndatabases = [\"booking\"]\n";

    #[test]
    fn only_a_declared_connection_has_a_keychain_target() {
        let (_d, ws) = open(PARENT);
        assert_eq!(ws.secret_target("pg"), Some(SecretKey::new(ws.dir(), "pg")));
        assert_eq!(
            ws.key_passphrase_target("pg"),
            Some(SecretKey::key_passphrase(ws.dir(), "pg"))
        );
        for name in ["pg/booking", "pg/other", "nope", ""] {
            assert_eq!(ws.secret_target(name), None, "{name}");
            assert_eq!(ws.key_passphrase_target(name), None, "{name}");
            assert!(ws.declared(name).is_none(), "{name}");
        }
        assert!(ws.declared("pg").is_some());
    }

    #[test]
    fn an_edit_never_reaches_a_child() {
        let (_d, ws) = open(PARENT);
        let child = "pg/booking".to_owned();
        let spec = ws.connection_spec("pg").unwrap();
        for edit in [
            Edit::DeleteConnection {
                name: child.clone(),
            },
            Edit::PutConnection {
                previous: Some(child.clone()),
                name: "renamed".into(),
                spec: spec.clone(),
            },
            Edit::Move {
                item: WorkspaceItem::Connection(child.clone()),
                to: None,
            },
        ] {
            assert!(
                matches!(plan_edit(&ws, &edit), Err(EditError::NotFound { ref name }) if *name == child),
                "{edit:?}"
            );
        }
        let added = Edit::PutConnection {
            previous: None,
            name: child,
            spec,
        };
        assert!(matches!(
            plan_edit(&ws, &added),
            Err(EditError::InvalidName { .. })
        ));
        assert!(ws.connection_spec("pg/booking").is_none());
    }
}

mod resolve {
    use std::borrow::Cow;
    use std::time::Duration;

    use super::open;
    use dbento_core::{Color, FolderPath, ResolveError, SecretSource};

    const TREE: &str = r#"dbento = 1
[folders."Prod"]
color = "red"

[connections.pg]
engine = "postgres"
host = "db.internal"
database = "app"
user = "app"
read_only = true
folder = "Prod"
statement_timeout = "30s"
settings = { search_path = "app" }
databases = ["booking", "user_db"]

[connections.allpg]
engine = "postgres"
host = "db.internal"
database = "postgres"
user = "app"
secret = { env = "ALL_PW" }
databases = "all"

[connections.ms]
engine = "mssql"
host = "db.internal"
user = "app"
read_only_user = "dbento_reader"
databases = ["Booking"]

[connections.plain]
engine = "postgres"
host = "db.internal"
database = "plain"
user = "app"
"#;

    #[test]
    fn a_listed_child_is_its_parent_in_another_database() {
        let (_d, ws) = open(TREE);
        let parent = ws.declared("pg").unwrap();
        let child = ws.resolve("pg/booking").unwrap();
        assert!(matches!(child, Cow::Owned(_)));
        assert_eq!(child.name(), "pg/booking");
        assert_eq!(child.parent(), Some("pg"));
        assert_eq!(parent.parent(), None);
        assert_eq!(child.database(), Some("booking"));
        assert_eq!(child.databases(), None);
        assert_eq!(child.engine(), parent.engine());
        assert_eq!(
            (child.host(), child.port(), child.user()),
            (parent.host(), parent.port(), parent.user())
        );
        assert_eq!(child.secret_source(), parent.secret_source());
        assert_eq!(child.tls(), parent.tls());
        assert!(child.read_only());
        assert_eq!(child.statement_timeout(), Some(Duration::from_secs(30)));
        assert_eq!(child.folder(), Some(&"Prod".parse::<FolderPath>().unwrap()));
        assert_eq!(
            child.effective_color(),
            Some(Color::Red),
            "the folder colour reaches the child"
        );
        assert!(child.is_production());
        assert_eq!(child.settings(), parent.settings());
        assert!(
            child.display_target().ends_with("/booking"),
            "{}",
            child.display_target()
        );
        assert!(!child.same_session(parent));
        assert!(!child.same_session(&ws.resolve("pg/user_db").unwrap()));
        assert!(child.same_session(&ws.resolve("pg/booking").unwrap()));
    }

    #[test]
    fn a_sql_server_child_keeps_the_reader_and_the_exact_spelling() {
        let (_d, ws) = open(TREE);
        let child = ws.resolve("ms/Booking").unwrap();
        assert_eq!(child.read_only_user(), Some("dbento_reader"));
        assert_eq!(child.database(), Some("Booking"));
    }

    #[test]
    fn all_resolves_any_valid_name_without_a_server() {
        let (_d, ws) = open(TREE);
        let child = ws.resolve("allpg/anything_at-all").unwrap();
        assert_eq!(child.database(), Some("anything_at-all"));
        assert_eq!(child.secret_source(), &SecretSource::Env("ALL_PW".into()));
    }

    #[test]
    fn a_name_that_does_not_resolve_says_why() {
        let (_d, ws) = open(TREE);
        for (name, says) in [
            ("nope", "no connection named \"nope\" in dbento.toml"),
            ("nope/x", "no connection named \"nope/x\" in dbento.toml"),
            (
                "plain/x",
                "connection \"plain\" sets no `databases`, so it has no child connections",
            ),
            (
                "pg/hotel",
                "\"hotel\" is not in the `databases` list of connection \"pg\"",
            ),
            (
                "pg/Booking",
                "\"Booking\" is not in the `databases` list of connection \"pg\"",
            ),
            (
                "ms/booking",
                "\"booking\" is not in the `databases` list of connection \"ms\"",
            ),
            (
                "allpg/my.db",
                "database name \"my.db\" must match [A-Za-z0-9_-]+ to be used as a connection",
            ),
            (
                "allpg/a/b",
                "database name \"a/b\" must match [A-Za-z0-9_-]+ to be used as a connection",
            ),
            (
                "allpg/",
                "database name \"\" must match [A-Za-z0-9_-]+ to be used as a connection",
            ),
        ] {
            let e = ws
                .resolve(name)
                .err()
                .unwrap_or_else(|| panic!("{name} resolved"));
            assert_eq!(e.to_string(), says, "{name}");
            assert!(ws.connection(name).is_none(), "{name}");
        }
        assert!(matches!(
            ws.resolve("pg/hotel"),
            Err(ResolveError::NotListed { ref parent, ref database }) if parent == "pg" && database == "hotel"
        ));
    }

    #[test]
    fn declared_and_connections_see_only_the_file() {
        let (_d, ws) = open(TREE);
        assert!(ws.declared("pg/booking").is_none());
        assert!(
            ws.secret_target("pg/booking").is_none(),
            "a resolved child still has no keychain target"
        );
        assert!(matches!(ws.connection("pg"), Some(Cow::Borrowed(_))));
        let names: Vec<&str> = ws.connections().iter().map(|c| c.name()).collect();
        assert_eq!(names, ["allpg", "ms", "pg", "plain"]);
    }
}

mod secrets {
    use super::open;
    use dbento_core::{
        MemoryStore, Secret, SecretKey, SecretStore, resolve_secret, secret_env_var, secret_hint,
    };

    const PARENTS: &str = "dbento = 1\n[connections.stayzy]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\ndatabases = \"all\"\n[connections.my-srv]\nengine = \"mssql\"\nhost = \"h\"\nuser = \"u\"\ndatabases = [\"app\"]\n";

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn a_child_keys_its_secret_by_its_parent() {
        let (_d, ws) = open(PARENTS);
        let parent = ws.declared("stayzy").unwrap();
        let child = ws.connection("stayzy/booking").unwrap();
        assert_eq!(parent.secret_owner(), "stayzy");
        assert_eq!(child.secret_owner(), "stayzy");
        assert_eq!(
            SecretKey::for_connection(&ws, &child),
            SecretKey::for_connection(&ws, parent)
        );
        assert_eq!(
            SecretKey::key_passphrase_for(&ws, &child),
            SecretKey::key_passphrase_for(&ws, parent)
        );
        assert_eq!(
            SecretKey::for_connection(&ws, &child).connection(),
            "stayzy"
        );
    }

    #[test]
    fn a_child_reads_the_parents_keychain_entry_then_its_variable() {
        let (_d, ws) = open(PARENTS);
        let child = ws.connection("my-srv/app").unwrap();
        let store = MemoryStore::new();
        let env = |v: &str| (v == "DBENTO_SECRET_MY_SRV").then(|| "from-env".to_owned());
        let got = resolve_secret(&ws, &child, &store, &env).unwrap();
        assert_eq!(
            got.map(|s| s.expose().to_owned()).as_deref(),
            Some("from-env")
        );
        let key = SecretKey::for_connection(&ws, ws.declared("my-srv").unwrap());
        store.set(&key, &Secret::new("from-keychain")).unwrap();
        let got = resolve_secret(&ws, &child, &store, &no_env).unwrap();
        assert_eq!(
            got.map(|s| s.expose().to_owned()).as_deref(),
            Some("from-keychain")
        );
    }

    #[test]
    fn the_missing_secret_hint_names_the_parent() {
        let (_d, ws) = open(PARENTS);
        let child = ws.connection("stayzy/booking").unwrap();
        assert_eq!(
            secret_hint(&ws, &child, &MemoryStore::new()).as_deref(),
            Some(
                "no secret was found for 'stayzy': run `dbento secret set stayzy` or set DBENTO_SECRET_STAYZY"
            )
        );
        assert_eq!(secret_env_var(child.secret_owner()), "DBENTO_SECRET_STAYZY");
    }
}
