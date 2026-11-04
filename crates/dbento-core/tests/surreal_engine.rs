//! The SurrealDB keys, as the workspace parses, shows and saves them.
use dbento_core::{
    AuthLevel, ConnectionSpec, Edit, Engine, OpenError, SecretSource, TlsMode, Workspace, plan_edit,
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

const SR: &str = "dbento = 1\n[connections.graph]\nengine = \"surrealdb\"\nhost = \"db.internal\"\n\
                  namespace = \"app\"\ndatabase = \"main\"\nuser = \"reader\"\n\
                  auth_level = \"database\"\n";
const ANON: &str = "dbento = 1\n[connections.graph]\nengine = \"surreal\"\nhost = \"h\"\n\
                    namespace = \"app\"\ndatabase = \"main\"\n";

#[test]
fn a_surrealdb_entry_opens_with_its_keys_and_defaults() {
    let (_d, ws) = ws_with(SR);
    let ws = ws.unwrap();
    let c = ws.connection("graph").unwrap();
    assert_eq!(
        (c.engine(), c.engine().name()),
        (Engine::Surrealdb, "surrealdb")
    );
    assert_eq!(
        (c.host(), c.port(), c.namespace(), c.database()),
        (Some("db.internal"), Some(8000), Some("app"), Some("main"))
    );
    assert_eq!(
        (c.user(), c.auth_level()),
        (Some("reader"), Some(AuthLevel::Database))
    );
    assert_eq!(c.tls(), Some(TlsMode::Verify));
    assert_eq!(c.secret_source(), &SecretSource::Keychain);
    assert_eq!(c.display_target(), "reader@db.internal:8000/app/main");
}

#[test]
fn no_user_is_anonymous_and_reads_no_secret() {
    let (_d, ws) = ws_with(ANON);
    let ws = ws.unwrap();
    let c = ws.connection("graph").unwrap();
    assert_eq!(c.engine(), Engine::Surrealdb);
    assert_eq!((c.user(), c.auth_level()), (None, None));
    assert_eq!(c.secret_source(), &SecretSource::None);
    assert_eq!(c.display_target(), "h:8000/app/main");
    let (_d, ws) = ws_with(&format!("{ANON}secret = \"none\"\n"));
    ws.unwrap();
    for line in ["secret = \"keychain\"\n", "secret = { env = \"X\" }\n"] {
        let m = invalid(&format!("{ANON}{line}"));
        assert!(m.contains("`secret` needs `user`"), "{line}: {m}");
    }
    let m = invalid(&format!("{ANON}auth_level = \"root\"\n"));
    assert!(m.contains("`auth_level` needs `user`"), "{m}");
}

#[test]
fn a_blank_user_is_anonymous() {
    for blank in ["", "   "] {
        let user = format!("{ANON}user = \"{blank}\"\n");
        let (_d, ws) = ws_with(&user);
        let ws = ws.unwrap();
        let c = ws.connection("graph").unwrap();
        assert_eq!((c.user(), c.auth_level()), (None, None));
        assert_eq!(c.secret_source(), &SecretSource::None);
        assert_eq!(c.display_target(), "h:8000/app/main");
        let m = invalid(&format!("{user}auth_level = \"root\"\n"));
        assert!(m.contains("`auth_level` needs `user`"), "{m}");
        let m = invalid(&format!("{user}secret = \"keychain\"\n"));
        assert!(m.contains("`secret` needs `user`"), "{m}");
    }
}

#[test]
fn the_tls_key_picks_the_scheme_and_a_local_host_gets_no_exception() {
    let tls = |host: &str, line: &str| {
        let toml = format!(
            "dbento = 1\n[connections.graph]\nengine = \"surrealdb\"\nhost = \"{host}\"\n\
             namespace = \"a\"\ndatabase = \"b\"\n{line}"
        );
        let (_d, ws) = ws_with(&toml);
        ws.unwrap().connection("graph").unwrap().tls()
    };
    assert_eq!(tls("localhost", ""), Some(TlsMode::Verify));
    assert_eq!(tls("localhost", "tls = \"off\"\n"), Some(TlsMode::Off));
    let (_d, ws) = ws_with(&format!("{SR}tls_ca = \"ca.pem\"\n"));
    let ws = ws.unwrap();
    let ca = ws.connection("graph").unwrap().tls_ca().unwrap();
    assert!(ca.ends_with("ca.pem"), "{ca:?}");
}

#[test]
fn the_required_keys_are_host_namespace_and_database() {
    let head = "dbento = 1\n[connections.c]\nengine = \"surrealdb\"\n";
    for (keys, want) in [
        (
            "namespace = \"a\"\ndatabase = \"b\"\n",
            "surrealdb needs `host`",
        ),
        (
            "host = \"h\"\ndatabase = \"b\"\n",
            "surrealdb needs `namespace`",
        ),
        (
            "host = \"h\"\nnamespace = \"a\"\n",
            "surrealdb needs `database`",
        ),
    ] {
        let m = invalid(&format!("{head}{keys}"));
        assert!(m.contains(want), "{keys}: {m}");
    }
    for keys in [
        "host = \"h\"\nnamespace = \" \"\ndatabase = \"b\"\n",
        "host = \"h\"\nnamespace = \"a\"\ndatabase = \"\"\n",
    ] {
        let m = invalid(&format!("{head}{keys}"));
        assert!(m.contains("must not be empty"), "{keys}: {m}");
    }
    let no_level = SR.replace("auth_level = \"database\"\n", "");
    let m = invalid(&no_level);
    assert!(
        m.contains("surrealdb needs `auth_level` with `user`"),
        "{m}"
    );
    let m = invalid(&SR.replace("\"database\"\n", "\"record\"\n"));
    assert!(
        m.contains("auth_level `record` must be \"root\", \"namespace\" or \"database\""),
        "{m}"
    );
    for (word, want) in [
        ("root", AuthLevel::Root),
        ("namespace", AuthLevel::Namespace),
        ("database", AuthLevel::Database),
    ] {
        let toml = SR.replace(
            "auth_level = \"database\"",
            &format!("auth_level = \"{word}\""),
        );
        let (_d, ws) = ws_with(&toml);
        let level = ws.unwrap().connection("graph").unwrap().auth_level();
        assert_eq!(level, Some(want), "{word}");
    }
}

#[test]
fn keys_of_other_engines_are_refused_by_name() {
    for line in [
        "path = \"x\"",
        "settings = { a = \"b\" }",
        "uri = \"mongodb://h\"",
        "tls_cert = \"c.pem\"",
        "tls_key = \"k.pem\"",
        "read_only_user = \"r\"",
    ] {
        let key = line.split(' ').next().unwrap();
        let m = invalid(&format!("{SR}{line}\n"));
        assert!(m.contains(&format!("`{key}`")), "{key}: {m}");
    }
}

#[test]
fn the_new_keys_are_refused_on_every_other_engine() {
    for (engine, keys) in [
        ("postgres", "host = \"h\"\ndatabase = \"d\"\nuser = \"u\"\n"),
        ("mysql", "host = \"h\"\nuser = \"u\"\n"),
        ("mssql", "host = \"h\"\nuser = \"u\"\n"),
        ("sqlite", "path = \"a.db\"\n"),
        ("duckdb", "path = \"a.duckdb\"\n"),
        ("mongodb", "uri = \"mongodb://h/x\"\n"),
    ] {
        for line in ["namespace = \"a\"", "auth_level = \"root\""] {
            let key = line.split(' ').next().unwrap();
            let m = invalid(&format!(
                "dbento = 1\n[connections.c]\nengine = \"{engine}\"\n{keys}{line}\n"
            ));
            assert!(
                m.contains(&format!(
                    "`{key}` is a surrealdb key, not valid for {engine}"
                )),
                "{engine}: {m}"
            );
        }
    }
    let m = invalid("dbento = 1\n[connections.x]\nengine = \"oracle\"\n");
    assert!(m.contains("surrealdb"), "{m}");
}

#[test]
fn a_surrealdb_entry_round_trips_through_a_save() {
    let (_dir, ws) = ws_with("dbento = 1\n");
    let mut spec = ConnectionSpec::new(Engine::Surrealdb);
    spec.host = Some("h".into());
    spec.namespace = Some("app".into());
    spec.database = Some("main".into());
    spec.user = Some("reader".into());
    spec.auth_level = Some(AuthLevel::Namespace);
    spec.read_only = Some(true);
    let edit = Edit::PutConnection {
        previous: None,
        name: "graph".into(),
        spec: spec.clone(),
    };
    let text = plan_edit(&ws.unwrap(), &edit)
        .unwrap()
        .new_text()
        .to_owned();
    assert!(text.contains("namespace = \"app\""), "{text}");
    assert!(text.contains("auth_level = \"namespace\""), "{text}");
    assert_eq!(
        ws_with(&text).1.unwrap().connection_spec("graph"),
        Some(spec)
    );
}

#[test]
fn a_changed_namespace_or_level_is_a_different_session() {
    let (_a, a) = ws_with(SR);
    let a = a.unwrap();
    let a = a.connection("graph").unwrap();
    assert!(a.same_session(a));
    for other in [
        SR.replace("namespace = \"app\"", "namespace = \"web\""),
        SR.replace("auth_level = \"database\"", "auth_level = \"namespace\""),
    ] {
        let (_b, b) = ws_with(&other);
        let b = b.unwrap();
        assert!(!a.same_session(b.connection("graph").unwrap()), "{other}");
    }
}
