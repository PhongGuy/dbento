use dbento_core::{InitError, OpenError, Secret, Workspace};
use std::fs;

fn dir_with(toml: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    dir
}

#[test]
fn open_and_discover_find_the_workspace_or_report_not_found() {
    let lone = tempfile::tempdir().unwrap();
    assert!(matches!(
        Workspace::open(lone.path()),
        Err(OpenError::NotFound { .. })
    ));
    assert!(matches!(
        Workspace::discover(lone.path()),
        Err(OpenError::NotFound { .. })
    ));
    let dir = dir_with("dbento = 1\n");
    let deep = dir.path().join("a/b/c");
    fs::create_dir_all(&deep).unwrap();
    assert_eq!(
        Workspace::discover(&deep).unwrap().dir(),
        fs::canonicalize(dir.path()).unwrap()
    );
}

#[test]
fn init_writes_an_openable_workspace_and_refuses_to_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    Workspace::init(dir.path()).unwrap();
    assert!(dir.path().join("queries").is_dir());
    let text = fs::read_to_string(dir.path().join("dbento.toml")).unwrap();
    assert!(text.contains('#'), "the template is commented");
    assert!(
        Workspace::open(dir.path())
            .unwrap()
            .connections()
            .is_empty()
    );
    assert!(matches!(
        Workspace::init(dir.path()),
        Err(InitError::Exists { .. })
    ));
    assert_eq!(
        fs::read_to_string(dir.path().join("dbento.toml")).unwrap(),
        text
    );
}

#[test]
fn saved_queries_are_bound_by_their_folder() {
    let dir = dir_with("dbento = 1\n[connections.prod]\nengine = \"sqlite\"\npath = \"p.db\"\n");
    let ws = Workspace::open(dir.path()).unwrap();
    assert!(
        ws.saved_queries().unwrap().is_empty(),
        "no queries folder yet"
    );
    for path in [
        "queries/prod/report.sql",
        "queries/prod/deep/x.sql",
        "queries/adhoc.sql",
        "queries/unknown/y.sql",
        "queries/notes.txt",
    ] {
        let p = dir.path().join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "select 1").unwrap();
    }
    let found: Vec<(String, Option<String>)> = ws
        .saved_queries()
        .unwrap()
        .iter()
        .map(|q| {
            (
                q.path().to_string_lossy().replace('\\', "/"),
                q.connection().map(str::to_owned),
            )
        })
        .collect();
    assert_eq!(
        found,
        vec![
            ("queries/adhoc.sql".to_owned(), None),
            (
                "queries/prod/deep/x.sql".to_owned(),
                Some("prod".to_owned())
            ),
            (
                "queries/prod/report.sql".to_owned(),
                Some("prod".to_owned())
            ),
            ("queries/unknown/y.sql".to_owned(), None),
        ]
    );
}

#[test]
fn a_secret_never_prints_itself() {
    let s = Secret::new("hunter2");
    assert_eq!(s.expose(), "hunter2");
    assert!(!format!("{s:?}").contains("hunter2"));
    assert!(!format!("{s}").contains("hunter2"));
}
