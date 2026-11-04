//! `read_dbeaver` on small projects built in temporary folders.
use super::*;
use crate::import::dbeaver::credentials::encrypt;
use crate::import::model::{ItemKind, SkipReason, WarningKind};
use crate::import::{OnConflict, plan};
use crate::{Color, Secret};
use std::path::Path;
use tempfile::TempDir;

fn put(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

const SOURCES: &str = r#"{"connections":{
  "pg-1":{"provider":"postgresql","driver":"postgres-jdbc","name":"Shop DB","configuration":{"host":"db.shop.example","database":"shop","type":"prod"}},
  "pg-2":{"provider":"postgresql","driver":"postgres-jdbc","name":"Shop DB","configuration":{"host":"db2.shop.example","database":"shop"}},
  "pg-3":{"provider":"postgresql","driver":"postgres-timescale-jdbc","name":"Metrics","configuration":{"host":"ts.example"}},
  "pg-4":{"provider":"postgresql","driver":"postgres-jdbc","name":"Pgpass","configuration":{"host":"db.pp.example","database":"pp","auth-model":"postgres_pgpass"}},
  "sq-1":{"provider":"sqlite","driver":"sqlite_jdbc","name":"Local","configuration":{"database":"local.db"}}
}}"#;

const CREDS: &str = r##"{"pg-1":{"#connection":{"user":"clerk","password":"fake-pw-1"}},"pg-2":{"#connection":{"user":"clerk2","password":"fake-pw-2"}},"pg-3":{"#connection":{"user":"ts","password":"fake-pw-3"}},"pg-4":{"#connection":{"user":"pp","password":"fake-pw-4"}}}"##;

fn project() -> TempDir {
    let tmp = TempDir::new().unwrap();
    put(tmp.path(), ".dbeaver/data-sources.json", SOURCES.as_bytes());
    put(
        tmp.path(),
        ".dbeaver/credentials-config.json",
        &encrypt(CREDS.as_bytes(), [3; 16]),
    );
    tmp
}

const SCRIPT_SOURCES: &str = r#"{"connections":{
  "pg-1":{"provider":"postgresql","driver":"postgres-jdbc","name":"Shop","configuration":{"host":"db.shop.example","database":"shop","properties":{"user":"clerk"}}},
  "pg-2":{"provider":"postgresql","driver":"postgres-timescale-jdbc","name":"Metrics","configuration":{"host":"ts.example"}}
}}"#;

fn script_project() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path();
    put(d, ".dbeaver/data-sources.json", SCRIPT_SOURCES.as_bytes());
    put(d, ".dbeaver/project-metadata.json", br#"{"resources":{"Scripts/reports/top.sql":{"default-datasource":"pg-1"},"Scripts/metrics.sql":{"default-datasource":"pg-2"},"Scripts/gone.sql":{"default-datasource":"pg-9"}}}"#);
    put(d, "Scripts/reports/top.sql", b"select 1;\n");
    put(d, "Scripts/metrics.sql", b"select 2;\n");
    put(d, "Scripts/gone.sql", b"select 3;\n");
    put(d, "Scripts/UPPER.SQL", b"select 4;\n");
    put(d, "Scripts/notes.txt", b"not sql");
    put(d, "Other/outside.sql", b"select 5;\n");
    tmp
}

fn empty_project() -> TempDir {
    let tmp = TempDir::new().unwrap();
    put(
        tmp.path(),
        ".dbeaver/data-sources.json",
        br#"{"connections":{}}"#,
    );
    tmp
}

#[test]
fn connections_take_names_skips_warnings_and_only_native_imported_secrets() {
    let p = project();
    let read = read_dbeaver(p.path(), &DBeaverOptions::new()).unwrap();
    let set = read.set();
    let names: Vec<(&str, &str)> = set
        .connections()
        .iter()
        .map(|c| (c.source_id(), c.name()))
        .collect();
    assert_eq!(
        names,
        [
            ("pg-1", "shop-db"),
            ("pg-2", "shop-db-2"),
            ("pg-4", "pgpass"),
            ("sq-1", "local")
        ]
    );
    let users: Vec<Option<&str>> = set.connections().iter().map(|c| c.draft().user()).collect();
    assert_eq!(users, [Some("clerk"), Some("clerk2"), Some("pp"), None]);
    let skipped: Vec<(ItemKind, &str, &SkipReason)> = set
        .skipped()
        .iter()
        .map(|s| (s.kind(), s.subject(), s.reason()))
        .collect();
    let fork = SkipReason::UnsupportedDriver("postgresql/postgres-timescale-jdbc".into());
    assert_eq!(skipped, [(ItemKind::DataSource, "Metrics", &fork)]);
    let pick = |subject: &str| -> Vec<&WarningKind> {
        set.warnings()
            .iter()
            .filter(|w| w.subject() == subject)
            .map(|w| w.kind())
            .collect()
    };
    assert!(pick("project").is_empty());
    assert_eq!(set.connections()[0].draft().color(), Some(Color::Red));
    assert!(pick("Pgpass").contains(&&WarningKind::AuthModel("postgres_pgpass".into())));
    assert_eq!(read.secrets().ids().collect::<Vec<_>>(), ["pg-1", "pg-2"]);
    assert_eq!(
        read.secrets().get("pg-1").map(Secret::expose),
        Some("fake-pw-1")
    );
    assert!(!format!("{read:?}").contains("fake-pw"));
}

#[test]
fn without_secrets_users_are_still_read_and_no_secret_is_kept() {
    let p = project();
    let read = read_dbeaver(p.path(), &DBeaverOptions::new().secrets(false)).unwrap();
    assert!(read.secrets().is_empty());
    assert_eq!(read.set().connections()[0].draft().user(), Some("clerk"));
    let (set, secrets) = read.into_parts();
    assert_eq!((set.connections().len(), secrets.len()), (4, 0));
}

#[test]
fn without_a_credentials_file_the_default_user_fills_in() {
    let tmp = TempDir::new().unwrap();
    put(tmp.path(), ".dbeaver/data-sources.json", SOURCES.as_bytes());
    let read = read_dbeaver(tmp.path(), &DBeaverOptions::new()).unwrap();
    let no_user = read
        .set()
        .skipped()
        .iter()
        .filter(|s| s.reason() == &SkipReason::NoUser)
        .count();
    assert_eq!((read.set().connections().len(), no_user), (1, 3));
    let opts = DBeaverOptions::new().default_user(Some("fallback".into()));
    let read = read_dbeaver(tmp.path(), &opts).unwrap();
    let users: Vec<Option<&str>> = read
        .set()
        .connections()
        .iter()
        .map(|c| c.draft().user())
        .collect();
    assert_eq!(
        users,
        [Some("fallback"), Some("fallback"), Some("fallback"), None]
    );
    assert!(read.secrets().is_empty());
}

#[test]
fn scripts_are_read_below_scripts_and_bound_only_to_imported_connections() {
    let p = script_project();
    let read = read_dbeaver(p.path(), &DBeaverOptions::new()).unwrap();
    let got: Vec<(Option<&str>, String, &[u8])> = read
        .set()
        .queries()
        .iter()
        .map(|q| {
            (
                q.connection(),
                q.relative().display().to_string(),
                q.content(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (None, "UPPER.SQL".to_owned(), &b"select 4;\n"[..]),
            (None, "gone.sql".to_owned(), &b"select 3;\n"[..]),
            (None, "metrics.sql".to_owned(), &b"select 2;\n"[..]),
            (
                Some("pg-1"),
                "reports/top.sql".to_owned(),
                &b"select 1;\n"[..]
            ),
        ]
    );
    assert_eq!(
        read.set().queries()[3].origin(),
        p.path().join("Scripts/reports/top.sql")
    );
}

#[test]
fn a_bound_script_lands_in_its_connection_folder() {
    let p = script_project();
    let ws = TempDir::new().unwrap();
    let read = read_dbeaver(p.path(), &DBeaverOptions::new()).unwrap();
    plan(ws.path(), read.set(), OnConflict::Skip)
        .unwrap()
        .apply()
        .unwrap();
    assert_eq!(
        std::fs::read(ws.path().join("queries/shop/reports/top.sql")).unwrap(),
        b"select 1;\n"
    );
    assert!(ws.path().join("queries/metrics.sql").is_file());
}

#[test]
fn a_non_utf8_script_is_copied_byte_for_byte_with_a_warning() {
    let tmp = empty_project();
    put(tmp.path(), "Scripts/latin1.sql", b"select '\xe9';\n");
    let read = read_dbeaver(tmp.path(), &DBeaverOptions::new()).unwrap();
    assert_eq!(read.set().queries()[0].content(), b"select '\xe9';\n");
    let path = tmp.path().join("Scripts/latin1.sql").display().to_string();
    let w = &read.set().warnings()[0];
    assert_eq!(
        (w.subject(), w.kind()),
        (path.as_str(), &WarningKind::NotUtf8)
    );
}

#[cfg(unix)]
#[test]
fn symlinked_scripts_and_folders_are_skipped_and_never_followed() {
    let outside = TempDir::new().unwrap();
    put(outside.path(), "secret.sql", b"select 'outside';\n");
    put(outside.path(), "dir/inner.sql", b"select 'inner';\n");
    let tmp = empty_project();
    put(tmp.path(), "Scripts/real.sql", b"select 1;\n");
    std::os::unix::fs::symlink(
        outside.path().join("secret.sql"),
        tmp.path().join("Scripts/link.sql"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("dir"),
        tmp.path().join("Scripts/linked-dir"),
    )
    .unwrap();
    let read = read_dbeaver(tmp.path(), &DBeaverOptions::new()).unwrap();
    let rel: Vec<String> = read
        .set()
        .queries()
        .iter()
        .map(|q| q.relative().display().to_string())
        .collect();
    assert_eq!(rel, ["real.sql"]);
    let skipped: Vec<(ItemKind, String, &SkipReason)> = read
        .set()
        .skipped()
        .iter()
        .map(|s| (s.kind(), s.subject().to_owned(), s.reason()))
        .collect();
    let at = |rel: &str| tmp.path().join(rel).display().to_string();
    assert_eq!(
        skipped,
        [
            (
                ItemKind::Query,
                at("Scripts/link.sql"),
                &SkipReason::Symlink
            ),
            (
                ItemKind::Query,
                at("Scripts/linked-dir"),
                &SkipReason::Symlink
            ),
        ]
    );
    let root_linked = empty_project();
    std::os::unix::fs::symlink(outside.path(), root_linked.path().join("Scripts")).unwrap();
    let read = read_dbeaver(root_linked.path(), &DBeaverOptions::new()).unwrap();
    assert!(read.set().queries().is_empty());
    assert_eq!(read.set().skipped()[0].reason(), &SkipReason::Symlink);
}

// One SQLite connection per colour case, so no user or credentials are needed.
const COLOR_SOURCES: &str = r#"{"connections":{
  "c1":{"provider":"sqlite","driver":"sqlite_jdbc","name":"Prod Blue","configuration":{"database":"a.db","type":"prod","color":"0,0,255"}},
  "c2":{"provider":"sqlite","driver":"sqlite_jdbc","name":"Dev Plain","configuration":{"database":"a.db","type":"dev"}},
  "c3":{"provider":"sqlite","driver":"sqlite_jdbc","name":"Test","configuration":{"database":"a.db","type":"test"}},
  "c4":{"provider":"sqlite","driver":"sqlite_jdbc","name":"Staging","configuration":{"database":"a.db","type":"staging"}},
  "c5":{"provider":"sqlite","driver":"sqlite_jdbc","name":"Custom Prod","configuration":{"database":"a.db","type":"p1"}},
  "c6":{"provider":"sqlite","driver":"sqlite_jdbc","name":"Dev Yellow","configuration":{"database":"a.db","type":"dev","color":"255,255,0"}},
  "c7":{"provider":"sqlite","driver":"sqlite_jdbc","name":"Themed","configuration":{"database":"a.db","type":"dev","color":"org.jkiss.dbeaver.color.connectionType.qa.background"}},
  "c8":{"provider":"sqlite","driver":"sqlite_jdbc","name":"Test White","configuration":{"database":"a.db","type":"test","color":"255,255,255"}},
  "c9":{"provider":"sqlite","driver":"sqlite_jdbc","name":"Test Odd","configuration":{"database":"a.db","type":"test","color":"not,a,colour"}}
},
"connection-types":{"staging":{"name":"Staging","color":"255,165,0"},"p1":{"name":"Production","color":"250,207,207"}}}"#;

#[test]
fn connection_types_and_colours_become_tags() {
    let tmp = TempDir::new().unwrap();
    put(
        tmp.path(),
        ".dbeaver/data-sources.json",
        COLOR_SOURCES.as_bytes(),
    );
    let read = read_dbeaver(tmp.path(), &DBeaverOptions::new()).unwrap();
    let set = read.set();
    let colors: Vec<(&str, Option<Color>)> = set
        .connections()
        .iter()
        .map(|c| (c.source_name(), c.draft().color()))
        .collect();
    assert_eq!(
        colors,
        [
            ("Prod Blue", Some(Color::Red)),
            ("Dev Plain", None),
            ("Test", Some(Color::Green)),
            ("Staging", Some(Color::Orange)),
            ("Custom Prod", None),
            ("Dev Yellow", Some(Color::Yellow)),
            ("Themed", None),
            ("Test White", None),
            ("Test Odd", None),
        ]
    );
    let unknown: Vec<(&str, &WarningKind)> = set
        .warnings()
        .iter()
        .filter(|w| matches!(w.kind(), WarningKind::ColorUnknown(_)))
        .map(|w| (w.subject(), w.kind()))
        .collect();
    let theme = "org.jkiss.dbeaver.color.connectionType.qa.background";
    assert_eq!(
        unknown,
        [
            (
                "Custom Prod",
                &WarningKind::ColorUnknown("250,207,207".into())
            ),
            ("Themed", &WarningKind::ColorUnknown(theme.into())),
            (
                "Test Odd",
                &WarningKind::ColorUnknown("not,a,colour".into())
            ),
        ]
    );
    assert!(
        !set.warnings()
            .iter()
            .any(|w| w.kind() == &WarningKind::ColorsNotImported)
    );
}
