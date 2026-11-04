//! `BeekeeperError` messages and `read_beekeeper` on databases built in a tempdir.
use super::*;
use crate::Color;
use crate::import::beekeeper::crypto::{WRAP_KEY, encrypt};
use crate::import::model::{ItemKind, SkipReason, WarningKind};
use rusqlite::{Connection, params};
use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn error_messages_name_the_path_and_the_way_out() {
    let db = || PathBuf::from("/bk/app.db");
    let cases = [
        (
            BeekeeperError::NotABeekeeperDir { path: "/bk".into() },
            "/bk is not a Beekeeper Studio data folder (no app.db)",
        ),
        (
            BeekeeperError::Open {
                path: db(),
                message: "database is locked".into(),
            },
            "cannot open /bk/app.db: database is locked; close Beekeeper Studio and retry",
        ),
        (
            BeekeeperError::NotABeekeeperDb { path: db() },
            "/bk/app.db is not a Beekeeper Studio database (no saved_connection table)",
        ),
        (
            BeekeeperError::Query {
                path: db(),
                message: "saved_connection.port holds text".into(),
            },
            "cannot read /bk/app.db: saved_connection.port holds text",
        ),
        (
            BeekeeperError::Key {
                path: "/bk/.key".into(),
            },
            "/bk/.key cannot be decrypted as a Beekeeper Studio key file",
        ),
    ];
    for (e, want) in cases {
        assert_eq!(e.to_string(), want);
    }
}

const KEY: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";
const SCHEMA: &str = "
CREATE TABLE saved_connection (id integer PRIMARY KEY, name varchar NOT NULL, connectionType varchar NOT NULL,
  host varchar, username varchar, defaultDatabase varchar, password varchar,
  sshEnabled boolean NOT NULL DEFAULT 0, labelColor varchar DEFAULT 'default',
  workspaceId integer NOT NULL DEFAULT -1, iamAuthOptions text NOT NULL DEFAULT '{}');
CREATE TABLE query_folder (id integer PRIMARY KEY, name varchar NOT NULL, parentId integer);
CREATE TABLE favorite_query (id integer PRIMARY KEY, title varchar NOT NULL, text text, queryFolderId integer);
";

/// A data folder with SCHEMA in app.db and, when `with_key`, a `.key` wrapping KEY.
fn folder(with_key: bool) -> TempDir {
    let dir = TempDir::new().unwrap();
    Connection::open(dir.path().join("app.db"))
        .unwrap()
        .execute_batch(SCHEMA)
        .unwrap();
    if with_key {
        let wrap = format!(r#"{{"encryptionKey":"{KEY}"}}"#);
        std::fs::write(
            dir.path().join(".key"),
            encrypt(WRAP_KEY, wrap.as_bytes(), [0; 16]),
        )
        .unwrap();
    }
    dir
}

fn exec(dir: &TempDir, sql: &str, p: impl rusqlite::Params) {
    Connection::open(dir.path().join("app.db"))
        .unwrap()
        .execute(sql, p)
        .unwrap();
}

/// A Postgres connection at db.example whose password is `pw` encrypted under KEY.
fn pg(dir: &TempDir, id: i64, name: &str, pw: Option<&str>) {
    let cipher = pw.map(|p| encrypt(KEY, &serde_json::to_vec(p).unwrap(), [id as u8; 16]));
    exec(
        dir,
        "INSERT INTO saved_connection (id, name, connectionType, host, username, defaultDatabase, password)
         VALUES (?1, ?2, 'postgresql', 'db.example', 'clerk', 'shop', ?3)",
        params![id, name, cipher],
    );
}

fn read(dir: &TempDir, opts: &BeekeeperOptions) -> BeekeeperRead {
    read_beekeeper(dir.path(), opts).unwrap()
}

fn secrets(r: &BeekeeperRead) -> Vec<(String, String)> {
    r.secrets()
        .ids()
        .map(|id| {
            (
                id.to_owned(),
                r.secrets().get(id).unwrap().expose().to_owned(),
            )
        })
        .collect()
}

fn warnings(r: &BeekeeperRead) -> Vec<(String, WarningKind)> {
    r.set()
        .warnings()
        .iter()
        .map(|w| (w.subject().to_owned(), w.kind().clone()))
        .collect()
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.to_owned(), b.to_owned())
}

#[test]
fn connections_take_names_in_id_order_and_their_passwords_by_id() {
    let dir = folder(true);
    pg(&dir, 1, "Shop", Some("fake-pw-1"));
    exec(
        &dir,
        "INSERT INTO saved_connection (id, name, connectionType) VALUES (2, 'Shop', 'tidb')",
        (),
    );
    pg(&dir, 3, "Shop", Some("fake-pw-3"));
    pg(&dir, 4, "Stock", None);
    let r = read(&dir, &BeekeeperOptions::new());
    let names: Vec<(&str, &str, &str)> = r
        .set()
        .connections()
        .iter()
        .map(|c| (c.source_id(), c.source_name(), c.name()))
        .collect();
    assert_eq!(
        names,
        [
            ("1", "Shop", "shop"),
            ("3", "Shop", "shop-2"),
            ("4", "Stock", "stock")
        ]
    );
    assert_eq!(
        secrets(&r),
        [pair("1", "fake-pw-1"), pair("3", "fake-pw-3")]
    );
    let skipped: Vec<(ItemKind, &str, &SkipReason)> = r
        .set()
        .skipped()
        .iter()
        .map(|s| (s.kind(), s.subject(), s.reason()))
        .collect();
    let tidb = SkipReason::UnsupportedDriver("tidb".into());
    assert_eq!(skipped, [(ItemKind::DataSource, "Shop", &tidb)]);
    assert!(warnings(&r).is_empty(), "{:?}", warnings(&r));
    assert!(!format!("{r:?}").contains("fake-pw"));
}

#[test]
fn a_missing_key_warns_once_and_imports_without_secrets() {
    let dir = folder(false);
    pg(&dir, 1, "Shop", Some("fake-pw-1"));
    pg(&dir, 2, "Stock", Some("fake-pw-2"));
    let r = read(&dir, &BeekeeperOptions::new());
    assert_eq!(r.set().connections().len(), 2);
    assert!(r.secrets().is_empty());
    let key = dir.path().join(".key").display().to_string();
    assert_eq!(warnings(&r), [(key, WarningKind::KeyMissing)]);
}

#[test]
fn a_bad_key_is_an_error_and_no_secrets_never_reads_it() {
    let dir = folder(false);
    pg(&dir, 1, "Shop", Some("fake-pw-1"));
    std::fs::write(dir.path().join(".key"), "garbage").unwrap();
    assert!(matches!(
        read_beekeeper(dir.path(), &BeekeeperOptions::new()),
        Err(BeekeeperError::Key { .. })
    ));
    let r = read(&dir, &BeekeeperOptions::new().secrets(false));
    assert_eq!(r.set().connections().len(), 1);
    assert!(r.secrets().is_empty() && warnings(&r).is_empty());
}

#[test]
fn an_undecryptable_password_leaves_only_that_connection_without_a_secret() {
    let dir = folder(true);
    pg(&dir, 1, "Shop", Some("fake-pw-1"));
    let insert = "INSERT INTO saved_connection (id, name, connectionType, host, username, password)
                  VALUES (?1, ?2, 'postgresql', 'db.example', 'clerk', ?3)";
    exec(
        &dir,
        insert,
        params![
            2,
            "Broken",
            encrypt("wrong key", br#""fake-pw-2""#, [2; 16])
        ],
    );
    exec(
        &dir,
        insert,
        params![3, "Number", encrypt(KEY, b"42", [3; 16])],
    );
    let r = read(&dir, &BeekeeperOptions::new());
    assert_eq!(r.set().connections().len(), 3);
    assert_eq!(secrets(&r), [pair("1", "fake-pw-1")]);
    assert_eq!(
        warnings(&r),
        [
            ("Broken".to_owned(), WarningKind::SecretUnreadable),
            ("Number".to_owned(), WarningKind::SecretUnreadable)
        ]
    );
}

#[test]
fn an_iam_connection_keeps_no_secret_and_an_empty_password_is_no_secret() {
    let dir = folder(true);
    exec(
        &dir,
        r#"INSERT INTO saved_connection (id, name, connectionType, host, username, password, iamAuthOptions)
           VALUES (1, 'Iam', 'postgresql', 'db.example', 'clerk', ?1, '{"iamAuthenticationEnabled":true}')"#,
        params![encrypt(KEY, br#""fake-pw-iam""#, [1; 16])],
    );
    exec(
        &dir,
        "INSERT INTO saved_connection (id, name, connectionType, host, username, password)
         VALUES (2, 'Empty', 'postgresql', 'db.example', 'clerk', '')",
        (),
    );
    let r = read(&dir, &BeekeeperOptions::new());
    assert!(r.secrets().is_empty());
    assert_eq!(
        warnings(&r),
        [("Iam".to_owned(), WarningKind::AuthModel("aws_iam".into()))]
    );
}

#[test]
fn label_colors_become_tags_and_an_unknown_one_warns() {
    let insert =
        "INSERT INTO saved_connection (id, name, connectionType, defaultDatabase, labelColor)
                  VALUES (?1, ?2, 'sqlite', '/nonexistent/x.db', ?3)";
    let dir = folder(true);
    let rows = [
        (1, "Red", Some("red")),
        (2, "Pink", Some("pink")),
        (3, "Blue", Some("blue")),
        (4, "Default", Some("default")),
        (5, "Null", None),
        (6, "Odd", Some("chartreuse")),
        (7, "Orange", Some("orange")),
        (8, "Yellow", Some("yellow")),
        (9, "Green", Some("green")),
        (10, "Purple", Some("purple")),
    ];
    for (id, name, color) in rows {
        exec(&dir, insert, params![id, name, color]);
    }
    let r = read(&dir, &BeekeeperOptions::new());
    let tags: Vec<_> = r
        .set()
        .connections()
        .iter()
        .map(|c| (c.source_name(), c.draft().color()))
        .collect();
    assert_eq!(
        tags,
        [
            ("Red", Some(Color::Red)),
            ("Pink", Some(Color::Red)),
            ("Blue", Some(Color::Blue)),
            ("Default", None),
            ("Null", None),
            ("Odd", None),
            ("Orange", Some(Color::Orange)),
            ("Yellow", Some(Color::Yellow)),
            ("Green", Some(Color::Green)),
            ("Purple", Some(Color::Purple)),
        ]
    );
    let colors: Vec<_> = warnings(&r)
        .into_iter()
        .filter(|(_, k)| !matches!(k, WarningKind::FileMissing))
        .collect();
    assert_eq!(
        colors,
        [(
            "Odd".to_owned(),
            WarningKind::ColorUnknown("chartreuse".into())
        )]
    );
}

#[test]
fn a_connection_name_that_is_not_utf8_warns() {
    let dir = folder(true);
    exec(
        &dir,
        "INSERT INTO saved_connection (id, name, connectionType, defaultDatabase)
         VALUES (1, CAST(X'4c697465ff' AS TEXT), 'sqlite', '/nonexistent/x.db')",
        (),
    );
    let r = read(&dir, &BeekeeperOptions::new());
    assert_eq!(r.set().connections()[0].name(), "lite");
    assert_eq!(
        warnings(&r)[0],
        ("Lite\u{fffd}".to_owned(), WarningKind::NotUtf8)
    );
}

#[test]
fn saved_queries_become_unbound_files_under_their_folder_path() {
    let dir = folder(true);
    Connection::open(dir.path().join("app.db"))
        .unwrap()
        .execute_batch(
            "
INSERT INTO query_folder VALUES (1, 'Reports', NULL), (2, 'Monthly / Q?', 1);
INSERT INTO favorite_query VALUES
 (1, 'Top orders', 'select 1;', 1),
 (2, 'a/b: c?', 'select 2;', 2),
 (3, 'Blank', '  ', NULL),
 (4, 'Null', NULL, NULL),
 (5, CAST(X'546f70ff' AS TEXT), CAST(X'73656c65637420ff3b' AS TEXT), NULL);
",
        )
        .unwrap();
    let r = read(&dir, &BeekeeperOptions::new());
    let app = dir.path().join("app.db").display().to_string();
    let origin = |id: u32| format!("{app}#favorite_query/{id}");
    let queries: Vec<(Option<&str>, String, String, &[u8])> = r
        .set()
        .queries()
        .iter()
        .map(|q| {
            (
                q.connection(),
                q.relative().display().to_string(),
                q.origin().display().to_string(),
                q.content(),
            )
        })
        .collect();
    assert_eq!(
        queries,
        [
            (
                None,
                "Reports/Top orders.sql".to_owned(),
                origin(1),
                &b"select 1;"[..]
            ),
            (
                None,
                "Reports/Monthly _ Q_/a_b_ c_.sql".to_owned(),
                origin(2),
                &b"select 2;"[..]
            ),
            (
                None,
                "Top\u{fffd}.sql".to_owned(),
                origin(5),
                &b"select \xff;"[..]
            ),
        ]
    );
    let skipped: Vec<(ItemKind, String, SkipReason)> = r
        .set()
        .skipped()
        .iter()
        .map(|s| (s.kind(), s.subject().to_owned(), s.reason().clone()))
        .collect();
    assert_eq!(
        skipped,
        [
            (ItemKind::Query, origin(3), SkipReason::EmptyQuery),
            (ItemKind::Query, origin(4), SkipReason::EmptyQuery)
        ]
    );
    assert_eq!(
        warnings(&r),
        [
            (origin(5), WarningKind::NotUtf8),
            (origin(5), WarningKind::NotUtf8)
        ]
    );
}

#[test]
fn the_app_db_file_itself_reads_and_another_path_is_refused() {
    let dir = folder(true);
    pg(&dir, 1, "Shop", Some("fake-pw-1"));
    let r = read_beekeeper(&dir.path().join("app.db"), &BeekeeperOptions::new()).unwrap();
    assert_eq!(secrets(&r), [pair("1", "fake-pw-1")]);
    assert!(matches!(
        read_beekeeper(&dir.path().join("nope"), &BeekeeperOptions::new()),
        Err(BeekeeperError::NotABeekeeperDir { .. })
    ));
}
