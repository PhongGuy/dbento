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
