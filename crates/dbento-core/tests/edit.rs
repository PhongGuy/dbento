use dbento_core::{
    Color, ConnectionSpec, Edit, EditError, Engine, FolderPath, SecretSource, TlsMode, Workspace,
    plan_edit,
};

pub const BASE: &str = "dbento = 1 # format\n\n# The production database.\n[connections.prod] # main\nengine = \"postgres\"\nhost = \"db.example.com\"\nport = 5432 # default port\ndatabase = \"app\"\nuser = \"app_ro\"\ntls = \"verify\"\ncolor = \"red\"\n\n[folders.\"team/a b\"]\ncolor = \"blue\"\n\n[connections.lite]\npath = \"data/app.db\" # relative\nengine = \"sqlite\"\n";
const FULL: &str = "dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"localhost\"\ndatabase = \"d\"\nuser = \"u\"\nsecret = { env = \"PG_PW\" }\nread_only = true\nfolder = \"team/a b\"\nstatement_timeout = \"30s\"\n";
const FOLDERS_LAST: &str = "dbento = 1\n\n[connections.a]\nengine = \"sqlite\"\npath = \"a.db\"\n\n[folders.f]\ncolor = \"red\"\n";
pub const DOTTED: &str = "dbento = 1\n[connections]\na.engine = \"sqlite\"\na.path = \"a.db\"\nb.engine = \"sqlite\"\nb.path = \"b.db\"\n";
const ROOT_DOTTED: &str = "dbento = 1\nconnections.a.engine = \"sqlite\"\nconnections.a.path = \"a.db\"\n\n[folders.f]\ncolor = \"red\"\n";
pub const INLINE: &str =
    "dbento = 1\n[connections]\nx = { engine = \"sqlite\", path = \"x.db\" }\n";
const INLINE_ROOT: &str =
    "dbento = 1\nconnections = { x = { engine = \"sqlite\", path = \"x.db\" } }\n";

pub fn ws(text: &str) -> (tempfile::TempDir, Workspace) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dbento.toml"), text).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    (dir, ws)
}

fn put(
    ws: &Workspace,
    previous: Option<&str>,
    name: &str,
    spec: ConnectionSpec,
) -> Result<String, EditError> {
    plan_edit(
        ws,
        &Edit::PutConnection {
            previous: previous.map(Into::into),
            name: name.into(),
            spec,
        },
    )
    .map(|p| p.new_text().to_owned())
}

fn sqlite(path: &str) -> ConnectionSpec {
    let mut s = ConnectionSpec::new(Engine::Sqlite);
    s.path = Some(path.into());
    s
}

fn folder(p: &str) -> FolderPath {
    p.parse().unwrap()
}

#[test]
fn source_text_is_the_file_as_loaded() {
    let (_d, w) = ws(BASE);
    assert_eq!(w.source_text(), BASE);
}

#[test]
fn connection_spec_reads_the_keys_as_written() {
    let (_d, w) = ws(BASE);
    let mut want = ConnectionSpec::new(Engine::Postgres);
    (want.host, want.port, want.database) = (
        Some("db.example.com".into()),
        Some(5432),
        Some("app".into()),
    );
    (want.user, want.tls, want.color) = (
        Some("app_ro".into()),
        Some(TlsMode::Verify),
        Some(Color::Red),
    );
    assert_eq!(w.connection_spec("prod").unwrap(), want);
    let mut lite = w.connection_spec("lite").unwrap();
    assert_eq!(lite.path.as_deref(), Some("data/app.db"));
    lite.path = None;
    assert_eq!(lite, ConnectionSpec::new(Engine::Sqlite));
    assert_eq!(w.connection_spec("nope"), None);

    let (_d, w) = ws(FULL);
    let pg = w.connection_spec("pg").unwrap();
    assert_eq!(pg.secret, Some(SecretSource::Env("PG_PW".into())));
    assert_eq!(
        pg.tls, None,
        "absent, although it resolves to off on localhost"
    );
    assert_eq!(pg.port, None, "absent, although it resolves to 5432");
    assert_eq!(pg.read_only, Some(true));
    assert_eq!(pg.folder, Some(folder("team/a b")));
    assert_eq!(pg.statement_timeout.as_deref(), Some("30s"));
}

#[test]
fn an_unchanged_spec_rewrites_nothing() {
    for text in [BASE, FULL, FOLDERS_LAST, DOTTED, ROOT_DOTTED] {
        let (_d, w) = ws(text);
        for c in w.connections() {
            let spec = w.connection_spec(c.name()).unwrap();
            assert_eq!(
                put(&w, Some(c.name()), c.name(), spec).unwrap(),
                text,
                "{}",
                c.name()
            );
        }
    }
}

#[test]
fn a_changed_value_keeps_its_comment_and_place() {
    let (_d, w) = ws(BASE);
    let mut s = w.connection_spec("prod").unwrap();
    s.port = Some(6432);
    assert_eq!(
        put(&w, Some("prod"), "prod", s).unwrap(),
        BASE.replace("port = 5432 # default port", "port = 6432 # default port")
    );
}

#[test]
fn a_cleared_key_goes_and_a_new_key_ends_its_table() {
    let (_d, w) = ws(BASE);
    let mut s = w.connection_spec("prod").unwrap();
    s.tls = None;
    s.read_only = Some(true);
    assert_eq!(
        put(&w, Some("prod"), "prod", s).unwrap(),
        BASE.replace("tls = \"verify\"\n", "")
            .replace("color = \"red\"\n", "color = \"red\"\nread_only = true\n")
    );
}

#[test]
fn odd_key_order_is_kept_and_values_are_quoted() {
    let (_d, w) = ws(BASE);
    let mut s = w.connection_spec("lite").unwrap();
    s.color = Some(Color::Green);
    s.folder = Some(folder("team/a b"));
    assert_eq!(
        put(&w, Some("lite"), "lite", s).unwrap(),
        BASE.replace(
            "engine = \"sqlite\"\n",
            "engine = \"sqlite\"\ncolor = \"green\"\nfolder = \"team/a b\"\n"
        )
    );
}

#[test]
fn a_new_connection_goes_after_the_last_table_in_key_order() {
    let (_d, w) = ws(FOLDERS_LAST);
    // Set in reverse key order, so the written order comes from the plan, not from this test.
    let mut s = ConnectionSpec::new(Engine::Postgres);
    (s.statement_timeout, s.folder, s.color) = (
        Some("5s".into()),
        Some(folder("team/a b")),
        Some(Color::Purple),
    );
    (s.read_only, s.tls, s.secret) = (
        Some(true),
        Some(TlsMode::Off),
        Some(SecretSource::Env("PW".into())),
    );
    (s.user, s.database, s.port, s.host) = (
        Some("u".into()),
        Some("d".into()),
        Some(5433),
        Some("h".into()),
    );
    let plan = plan_edit(
        &w,
        &Edit::PutConnection {
            previous: None,
            name: "pg2".into(),
            spec: s,
        },
    )
    .unwrap();
    assert_eq!(
        plan.new_text(),
        format!(
            "{FOLDERS_LAST}\n[connections.pg2]\nengine = \"postgres\"\nhost = \"h\"\nport = 5433\ndatabase = \"d\"\nuser = \"u\"\nsecret = {{ env = \"PW\" }}\ntls = \"off\"\nread_only = true\ncolor = \"purple\"\nfolder = \"team/a b\"\nstatement_timeout = \"5s\"\n"
        )
    );
    assert_eq!(plan.workspace().source_text(), plan.new_text());
    assert!(plan.workspace().declared("pg2").unwrap().read_only());
    assert_eq!(plan.renamed_connection(), None);
}

#[test]
fn secret_words_are_written_as_words() {
    let (_d, w) = ws(FOLDERS_LAST);
    let mut s = w.connection_spec("a").unwrap();
    (s.engine, s.path, s.secret) = (Engine::Postgres, None, Some(SecretSource::None));
    (s.host, s.database, s.user) = (Some("h".into()), Some("d".into()), Some("u".into()));
    let text = put(&w, Some("a"), "a", s.clone()).unwrap();
    assert!(text.contains("engine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\nsecret = \"none\"\n"), "{text}");
    assert!(!text.contains("path"), "{text}");
    s.secret = Some(SecretSource::Keychain);
    assert!(
        put(&w, Some("a"), "a", s)
            .unwrap()
            .contains("secret = \"keychain\"\n")
    );
}

#[test]
fn the_first_connection_of_an_empty_workspace() {
    let (_d, w) = ws("dbento = 1 # v\n");
    assert_eq!(
        put(&w, None, "new", sqlite("n.db")).unwrap(),
        "dbento = 1 # v\n\n[connections.new]\nengine = \"sqlite\"\npath = \"n.db\"\n"
    );
}

#[test]
fn crlf_stays_crlf() {
    let crlf = BASE.replace('\n', "\r\n");
    let (_d, w) = ws(&crlf);
    let mut s = w.connection_spec("prod").unwrap();
    s.port = Some(6432);
    assert_eq!(
        put(&w, Some("prod"), "prod", s).unwrap(),
        crlf.replace("port = 5432", "port = 6432")
    );
    let added = put(&w, None, "new", sqlite("n.db")).unwrap();
    assert!(
        added.ends_with("\r\n[connections.new]\r\nengine = \"sqlite\"\r\npath = \"n.db\"\r\n"),
        "{added:?}"
    );
    assert!(!added.replace("\r\n", "").contains('\n'), "{added:?}");
}

#[test]
fn dotted_entries_stay_dotted() {
    let (_d, w) = ws(DOTTED);
    let mut s = w.connection_spec("a").unwrap();
    s.read_only = Some(true);
    assert_eq!(
        put(&w, Some("a"), "a", s).unwrap(),
        "dbento = 1\n[connections]\na.engine = \"sqlite\"\na.path = \"a.db\"\na.read_only = true\nb.engine = \"sqlite\"\nb.path = \"b.db\"\n"
    );
    let (_d, w) = ws(ROOT_DOTTED);
    assert_eq!(
        put(&w, None, "c", sqlite("c.db")).unwrap(),
        format!("{ROOT_DOTTED}\n[connections.c]\nengine = \"sqlite\"\npath = \"c.db\"\n")
    );
}

#[test]
fn inline_entries_are_refused_but_others_stay_editable() {
    let (_d, w) = ws(INLINE);
    let s = w.connection_spec("x").unwrap();
    assert_eq!(
        put(&w, Some("x"), "x", s),
        Err(EditError::Unmergeable { entry: "x".into() })
    );
    assert_eq!(
        put(&w, None, "y", sqlite("y.db")).unwrap(),
        format!("{INLINE}\n[connections.y]\nengine = \"sqlite\"\npath = \"y.db\"\n")
    );
    let (_d, w) = ws(INLINE_ROOT);
    assert_eq!(
        put(&w, None, "y", sqlite("y.db")),
        Err(EditError::Unmergeable {
            entry: "connections".into()
        })
    );
}

#[test]
fn refusals_name_the_problem() {
    let (_d, w) = ws(BASE);
    assert_eq!(
        put(&w, None, "bad name", sqlite("x.db")),
        Err(EditError::InvalidName {
            name: "bad name".into()
        })
    );
    assert_eq!(
        put(&w, None, "lite", sqlite("x.db")),
        Err(EditError::NameTaken {
            name: "lite".into()
        })
    );
    assert_eq!(
        put(&w, Some("nope"), "nope", sqlite("x.db")),
        Err(EditError::NotFound {
            name: "nope".into()
        })
    );
    let mut bad = sqlite("x.db");
    bad.host = Some("h".into());
    match put(&w, None, "x", bad) {
        Err(EditError::Invalid { message }) => {
            assert!(message.contains("postgres key"), "{message}")
        }
        other => panic!("{other:?}"),
    }
    let mut timeout = sqlite("x.db");
    timeout.statement_timeout = Some("soon".into());
    assert!(matches!(
        put(&w, None, "x", timeout),
        Err(EditError::Invalid { .. })
    ));
}

#[test]
fn a_missing_final_newline_stays_missing() {
    let plain = "dbento = 1\n[connections.a]\nengine = \"sqlite\"\npath = \"a.db\"";
    let commented = format!("{plain} # last");
    let crlf = plain.replace('\n', "\r\n");
    for text in [plain.to_owned(), commented, crlf] {
        let (_d, w) = ws(&text);
        let spec = w.connection_spec("a").unwrap();
        assert_eq!(put(&w, Some("a"), "a", spec.clone()).unwrap(), text);

        let mut changed = spec;
        changed.read_only = Some(true);
        let edited = put(&w, Some("a"), "a", changed).unwrap();
        assert!(edited.contains("read_only = true"), "{edited:?}");
        assert!(!edited.ends_with('\n'), "{edited:?}");
        let added = put(&w, None, "b", sqlite("b.db")).unwrap();
        assert!(added.ends_with("path = \"b.db\""), "{added:?}");
    }
}

fn rename(
    ws: &Workspace,
    from: &str,
    to: &str,
    spec: ConnectionSpec,
) -> Result<dbento_core::EditPlan, EditError> {
    plan_edit(
        ws,
        &Edit::PutConnection {
            previous: Some(from.into()),
            name: to.into(),
            spec,
        },
    )
}

#[test]
fn a_rename_keeps_comments_and_place() {
    let (_d, w) = ws(BASE);
    let plan = rename(&w, "prod", "prod2", w.connection_spec("prod").unwrap()).unwrap();
    assert_eq!(
        plan.new_text(),
        BASE.replace("[connections.prod] # main", "[connections.prod2] # main")
    );
    assert_eq!(plan.renamed_connection(), Some(("prod", "prod2")));
    assert!(plan.workspace().declared("prod").is_none());
}

#[test]
fn a_dotted_rename_keeps_its_place() {
    let (_d, w) = ws(DOTTED);
    let plan = rename(&w, "a", "n", w.connection_spec("a").unwrap()).unwrap();
    assert_eq!(
        plan.new_text(),
        "dbento = 1\n[connections]\nn.engine = \"sqlite\"\nn.path = \"a.db\"\nb.engine = \"sqlite\"\nb.path = \"b.db\"\n"
    );
}

#[test]
fn rename_refusals() {
    let (d, w) = ws(BASE);
    let s = w.connection_spec("lite").unwrap();
    assert!(
        matches!(rename(&w, "lite", "prod", s.clone()), Err(EditError::NameTaken { name }) if name == "prod")
    );
    assert!(matches!(
        rename(&w, "lite", "no good", s.clone()),
        Err(EditError::InvalidName { .. })
    ));
    std::fs::create_dir_all(d.path().join("queries/lite2")).unwrap();
    match rename(&w, "lite", "lite2", s) {
        Err(e @ EditError::QueriesExist { .. }) => assert_eq!(
            e.to_string(),
            "queries/lite2 already exists; move it before renaming"
        ),
        other => panic!("{other:?}"),
    }
    let (_d, w) = ws(INLINE);
    let s = w.connection_spec("x").unwrap();
    assert_eq!(
        rename(&w, "x", "z", s).err(),
        Some(EditError::Unmergeable { entry: "x".into() })
    );
}

#[test]
fn delete_removes_the_entry_and_its_header_comments() {
    let (_d, w) = ws(BASE);
    let plan = plan_edit(
        &w,
        &Edit::DeleteConnection {
            name: "lite".into(),
        },
    )
    .unwrap();
    assert_eq!(
        plan.new_text(),
        BASE.strip_suffix(
            "\n[connections.lite]\npath = \"data/app.db\" # relative\nengine = \"sqlite\"\n"
        )
        .unwrap()
    );
    let plan = plan_edit(
        &w,
        &Edit::DeleteConnection {
            name: "prod".into(),
        },
    )
    .unwrap();
    assert!(
        !plan.new_text().contains("production database"),
        "{}",
        plan.new_text()
    );
    assert!(plan.workspace().declared("prod").is_none());
    let (_d, w) = ws(DOTTED);
    let plan = plan_edit(&w, &Edit::DeleteConnection { name: "b".into() }).unwrap();
    assert_eq!(
        plan.new_text(),
        "dbento = 1\n[connections]\na.engine = \"sqlite\"\na.path = \"a.db\"\n"
    );
    assert_eq!(
        plan_edit(
            &w,
            &Edit::DeleteConnection {
                name: "nope".into()
            }
        )
        .err(),
        Some(EditError::NotFound {
            name: "nope".into()
        })
    );
    let (_d, w) = ws(INLINE);
    assert_eq!(
        plan_edit(&w, &Edit::DeleteConnection { name: "x".into() }).err(),
        Some(EditError::Unmergeable { entry: "x".into() })
    );
}

#[test]
fn rename_and_delete_keep_a_missing_final_newline() {
    let text = DOTTED.trim_end();
    let (_d, w) = ws(text);
    let plan = rename(&w, "a", "n", w.connection_spec("a").unwrap()).unwrap();
    assert_eq!(
        plan.new_text(),
        "dbento = 1\n[connections]\nn.engine = \"sqlite\"\nn.path = \"a.db\"\nb.engine = \"sqlite\"\nb.path = \"b.db\""
    );
    let plan = plan_edit(&w, &Edit::DeleteConnection { name: "b".into() }).unwrap();
    assert_eq!(
        plan.new_text(),
        "dbento = 1\n[connections]\na.engine = \"sqlite\"\na.path = \"a.db\""
    );
    let crlf = BASE.replace('\n', "\r\n");
    let (_d, w) = ws(crlf.trim_end());
    let plan = plan_edit(
        &w,
        &Edit::DeleteConnection {
            name: "lite".into(),
        },
    )
    .unwrap();
    assert_eq!(
        plan.new_text(),
        crlf.strip_suffix("\r\n\r\n[connections.lite]\r\npath = \"data/app.db\" # relative\r\nengine = \"sqlite\"\r\n")
            .unwrap()
    );
}

const PG_SETTINGS: &str = "dbento = 1\n[connections.p]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\nsettings = { search_path = \"app\" }\n\n[connections.q]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\n\n[connections.q.settings]\nTimeZone = \"UTC\"\n\n[connections.r]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\nsettings.work_mem = \"8MB\"\n\n[connections.keep]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\nsettings = { work_mem = \"4MB\" }\n";

#[test]
fn switching_away_from_postgres_drops_the_settings() {
    let (_dir, w) = ws(PG_SETTINGS);
    let to_mysql = |name: &str| {
        let mut s = w.connection_spec(name).unwrap();
        s.engine = Engine::Mysql;
        s
    };
    let mut to_sqlite = ConnectionSpec::new(Engine::Sqlite);
    to_sqlite.path = Some("q.db".into());
    for (name, spec, gone) in [
        ("p", to_mysql("p"), "search_path"),
        ("q", to_sqlite, "TimeZone"),
        ("r", to_mysql("r"), "8MB"),
    ] {
        let engine = spec.engine;
        let text = put(&w, Some(name), name, spec).unwrap();
        assert!(!text.contains(gone), "{name}: {text}");
        assert!(
            text.contains("settings = { work_mem = \"4MB\" }"),
            "{name}: another connection keeps its settings: {text}"
        );
        let (_d, reopened) = ws(&text);
        let conn = reopened.declared(name).unwrap();
        assert_eq!(conn.engine(), engine, "{name}");
        assert!(conn.settings().is_empty(), "{name}");
    }
}

#[test]
fn a_postgres_save_keeps_its_settings() {
    let (_dir, w) = ws(PG_SETTINGS);
    let mut spec = w.connection_spec("p").unwrap();
    spec.host = Some("h2".into());
    let text = put(&w, Some("p"), "p", spec).unwrap();
    assert!(
        text.contains(
            "host = \"h2\"\ndatabase = \"d\"\nuser = \"u\"\nsettings = { search_path = \"app\" }\n"
        ),
        "{text}"
    );
}
