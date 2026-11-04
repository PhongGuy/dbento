// Pins behaviours added after review: strict root keys and case-insensitive query files.
use dbento_core::{OpenError, Workspace};
use std::fs;

#[test]
fn a_misspelled_top_level_key_is_an_open_error() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("dbento.toml"),
        "dbento = 1\n[conections.x]\nengine = \"sqlite\"\n",
    )
    .unwrap();
    let err = Workspace::open(dir.path()).expect_err("unknown root key");
    assert!(!matches!(err, OpenError::NotFound { .. }), "{err:?}");
}

#[test]
fn saved_queries_match_the_sql_extension_in_any_case() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("dbento.toml"), "dbento = 1\n").unwrap();
    fs::create_dir(dir.path().join("queries")).unwrap();
    fs::write(dir.path().join("queries/a.SQL"), "select 1").unwrap();
    fs::write(dir.path().join("queries/b.sql"), "select 2").unwrap();
    fs::write(dir.path().join("queries/c.txt"), "no").unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    assert_eq!(ws.saved_queries().unwrap().len(), 2);
}

#[test]
fn a_refused_init_leaves_the_directory_untouched() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("dbento.toml"), "dbento = 1\n").unwrap();
    Workspace::init(dir.path()).expect_err("already initialised");
    assert!(!dir.path().join("queries").exists());
}

#[test]
fn a_statement_timeout_of_zero_or_beyond_i32_milliseconds_is_invalid() {
    for t in ["0s", "0ms", "2147483648ms", "30days"] {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("dbento.toml"),
            format!(
                "dbento = 1\n[connections.p]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\n\
                 statement_timeout = \"{t}\"\n"
            ),
        )
        .unwrap();
        let err = Workspace::open(dir.path()).expect_err(t);
        assert!(
            matches!(err, OpenError::Invalid { ref message, .. } if message.contains("statement_timeout")),
            "{t}: {err:?}"
        );
    }
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("dbento.toml"),
        "dbento = 1\n[connections.p]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\n\
         statement_timeout = \"2147483647ms\"\n",
    )
    .unwrap();
    Workspace::open(dir.path()).expect("the largest timeout Postgres accepts");
}
