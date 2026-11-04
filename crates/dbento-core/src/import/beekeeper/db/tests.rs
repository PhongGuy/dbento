//! Reading app.db read-only: locating it, probing columns, folders, text bytes and error codes.
use super::*;
use crate::import::beekeeper::BeekeeperError;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const MODERN: &str = "
CREATE TABLE connection_folder (id integer PRIMARY KEY, name varchar NOT NULL, parentId integer);
CREATE TABLE saved_connection (
  id integer PRIMARY KEY, name varchar NOT NULL, connectionType varchar NOT NULL,
  host varchar, port integer, username varchar, defaultDatabase varchar, password varchar,
  sshEnabled boolean NOT NULL DEFAULT 0, ssl boolean NOT NULL DEFAULT 0,
  sslCaFile varchar, sslCertFile varchar, sslKeyFile varchar,
  sslRejectUnauthorized boolean NOT NULL DEFAULT 1, labelColor varchar DEFAULT 'default',
  workspaceId integer NOT NULL DEFAULT -1, socketPathEnabled boolean NOT NULL DEFAULT 0,
  socketPath varchar, readOnlyMode boolean NOT NULL DEFAULT 0,
  azureAuthOptions text NOT NULL DEFAULT '{}', iamAuthOptions text NOT NULL DEFAULT '{}',
  connectionFolderId integer);
CREATE TABLE query_folder (id integer PRIMARY KEY, name varchar NOT NULL, parentId integer);
CREATE TABLE favorite_query (id integer PRIMARY KEY, title varchar NOT NULL, text text, queryFolderId integer);
";

/// A data folder whose app.db is built by `sql`.
fn db(sql: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("app.db");
    Connection::open(&path).unwrap().execute_batch(sql).unwrap();
    (dir, path)
}

fn modern(rows: &str) -> (TempDir, PathBuf) {
    db(&format!("{MODERN}{rows}"))
}

fn files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let p = e.unwrap().path();
            let bytes = std::fs::read(&p).unwrap();
            (p, bytes)
        })
        .collect();
    out.sort();
    out
}

#[test]
fn the_path_is_the_data_folder_or_its_app_db() {
    let (dir, app) = modern("");
    let want = (app.clone(), dir.path().to_path_buf());
    assert_eq!(locate(dir.path()).unwrap(), want);
    assert_eq!(locate(&app).unwrap(), want);
    let other = TempDir::new().unwrap();
    std::fs::write(other.path().join("app-dev.db"), b"").unwrap();
    for bad in [
        other.path().to_path_buf(),
        other.path().join("app-dev.db"),
        other.path().join("absent"),
    ] {
        assert!(
            matches!(locate(&bad), Err(BeekeeperError::NotABeekeeperDir { .. })),
            "{bad:?}"
        );
    }
}

#[test]
fn the_default_raw_connection_has_the_migration_defaults() {
    let d = RawConnection::default();
    assert_eq!((d.workspace_id, d.ssl_reject_unauthorized), (-1, true));
}

#[test]
fn connections_are_read_in_id_order_with_their_folder_path() {
    let (_d, app) = modern(
        "
INSERT INTO connection_folder VALUES (1, 'Prod', NULL), (2, 'EU', 1), (3, 'Loop A', 4), (4, 'Loop B', 3), (5, 'Orphan', 99);
INSERT INTO saved_connection (id, name, connectionType, host, port, username, defaultDatabase, password, ssl, sslCaFile, sslRejectUnauthorized, labelColor, readOnlyMode, connectionFolderId)
  VALUES (7, 'Second', 'postgresql', 'db.example', 5433, 'clerk', 'shop', 'cipher-text', 1, '/ca.pem', 0, 'red', 1, 2);
INSERT INTO saved_connection (id, name, connectionType, connectionFolderId) VALUES (3, 'First', 'sqlite', 3);
INSERT INTO saved_connection (id, name, connectionType, connectionFolderId) VALUES (5, 'Middle', 'sqlite', 5);
INSERT INTO saved_connection (id, name, connectionType, connectionFolderId) VALUES (6, 'Gone', 'sqlite', 42);
",
    );
    let raw = read_db(&app).unwrap();
    let ids: Vec<(i64, &str, Option<&str>)> = raw
        .connections
        .iter()
        .map(|c| (c.id, c.name.as_str(), c.folder.as_ref().map(|f| f.as_str())))
        .collect();
    assert_eq!(
        ids,
        [
            (3, "First", Some("Loop B/Loop A")),
            (5, "Middle", Some("Orphan")),
            (6, "Gone", None),
            (7, "Second", Some("Prod/EU"))
        ]
    );
    let c = &raw.connections[3];
    assert_eq!(
        (
            c.connection_type.as_deref(),
            c.host.as_deref(),
            c.port,
            c.username.as_deref(),
            c.default_database.as_deref()
        ),
        (
            Some("postgresql"),
            Some("db.example"),
            Some(5433),
            Some("clerk"),
            Some("shop")
        )
    );
    assert_eq!(
        c.password.as_ref().map(Ciphertext::as_str),
        Some("cipher-text")
    );
    assert!(c.ssl && !c.ssl_reject_unauthorized && c.read_only && !c.ssh_enabled);
    assert_eq!(
        (
            c.ssl_ca_file.as_deref(),
            c.label_color.as_deref(),
            c.workspace_id
        ),
        (Some("/ca.pem"), Some("red"), -1)
    );
}

#[test]
fn an_old_schema_reads_with_migration_defaults_and_no_queries() {
    let (_d, app) = db("
CREATE TABLE saved_connection (id integer PRIMARY KEY, name varchar NOT NULL, connectionType varchar NOT NULL, host varchar, username varchar, password varchar);
INSERT INTO saved_connection (id, name, connectionType, host, username) VALUES (1, 'Legacy', 'postgresql', 'legacy.example', 'legacy');
");
    let raw = read_db(&app).unwrap();
    let want = RawConnection {
        id: 1,
        name: "Legacy".into(),
        connection_type: Some("postgresql".into()),
        host: Some("legacy.example".into()),
        username: Some("legacy".into()),
        ..RawConnection::default()
    };
    assert_eq!(raw.connections, [want]);
    assert!(raw.queries.is_empty());
}

#[test]
fn auth_options_are_read_from_their_json_columns() {
    let (_d, app) = modern(
        r#"
INSERT INTO saved_connection (id, name, connectionType, iamAuthOptions, azureAuthOptions) VALUES
 (1, 'Iam', 'postgresql', '{"iamAuthenticationEnabled":true,"authType":"iam_cli"}', '{}'),
 (2, 'Azure', 'postgresql', '{}', '{"azureAuthEnabled":true}'),
 (3, 'Off', 'postgresql', '{"iamAuthenticationEnabled":false}', 'not json');
"#,
    );
    let flags: Vec<(bool, bool)> = read_db(&app)
        .unwrap()
        .connections
        .iter()
        .map(|c| (c.iam_enabled, c.azure_enabled))
        .collect();
    assert_eq!(flags, [(true, false), (false, true), (false, false)]);
}

#[test]
fn a_name_that_is_not_utf8_is_converted_lossily_and_flagged() {
    let (_d, app) = modern(
        "INSERT INTO saved_connection (id, name, connectionType, host) VALUES
         (1, CAST(X'53686f70ff' AS TEXT), 'postgresql', CAST(X'6462ff' AS TEXT)),
         (2, 'Fine', 'sqlite', NULL);",
    );
    let c = read_db(&app).unwrap().connections;
    assert_eq!(
        (c[0].name.as_str(), c[0].name_not_utf8, c[0].host.as_deref()),
        ("Shop\u{fffd}", true, Some("db\u{fffd}"))
    );
    assert!(!c[1].name_not_utf8);
}

#[test]
fn saved_queries_keep_their_folder_chain_and_text_bytes() {
    let (_d, app) = modern(
        "
INSERT INTO query_folder VALUES (1, 'Reports', NULL), (2, 'Monthly', 1);
INSERT INTO favorite_query VALUES
 (2, 'Top', 'select 1;', 2),
 (1, CAST(X'546f70ff' AS TEXT), CAST(X'73656c65637420ff3b' AS TEXT), NULL),
 (3, 'Nothing', NULL, 9);
",
    );
    let q = read_db(&app).unwrap().queries;
    /// A saved query as (id, title, title_not_utf8, text, folder).
    type Row<'a> = (i64, &'a str, bool, Option<&'a [u8]>, Vec<&'a str>);
    let rows: Vec<Row> = q
        .iter()
        .map(|q| {
            (
                q.id,
                q.title.as_str(),
                q.title_not_utf8,
                q.text.as_deref(),
                q.folder.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (1, "Top\u{fffd}", true, Some(&b"select \xff;"[..]), vec![]),
            (
                2,
                "Top",
                false,
                Some(&b"select 1;"[..]),
                vec!["Reports", "Monthly"]
            ),
            (3, "Nothing", false, None, vec![]),
        ]
    );
}

#[test]
fn a_file_that_is_not_sqlite_or_lacks_saved_connection_is_not_a_beekeeper_db() {
    let dir = TempDir::new().unwrap();
    let text = dir.path().join("text.db");
    std::fs::write(
        &text,
        "plain text, not an SQLite database file at all. ".repeat(20),
    )
    .unwrap();
    let empty = dir.path().join("empty.db");
    std::fs::write(&empty, b"").unwrap();
    let (_d, other) =
        db("CREATE TABLE favorite_query (id integer PRIMARY KEY, title varchar, text text);");
    let (_e, no_name) =
        db("CREATE TABLE saved_connection (id integer PRIMARY KEY, connectionType varchar);");
    for path in [text, empty, other, no_name] {
        assert!(
            matches!(read_db(&path), Err(BeekeeperError::NotABeekeeperDb { .. })),
            "{path:?}"
        );
    }
}

#[test]
fn a_locked_database_is_an_open_error_that_says_to_close_beekeeper() {
    let (_d, app) = modern("");
    let writer = Connection::open(&app).unwrap();
    writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
    match read_db(&app) {
        Err(e @ BeekeeperError::Open { .. }) => {
            assert!(
                e.to_string().contains("close Beekeeper Studio and retry"),
                "{e}"
            )
        }
        other => panic!("{:?}", other.map(|r| r.connections.len())),
    }
    writer.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn a_hot_journal_is_an_open_error() {
    // A copy taken while a writer has spilled pages is what a crash leaves behind.
    let (_d, app) = modern("");
    let writer = Connection::open(&app).unwrap();
    writer
        .execute_batch(
            "PRAGMA cache_size = 1; BEGIN; CREATE TABLE big(x);
             INSERT INTO big SELECT randomblob(4000) FROM
               (WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 200) SELECT n FROM c);",
        )
        .unwrap();
    let crashed = TempDir::new().unwrap();
    for name in ["app.db", "app.db-journal"] {
        std::fs::copy(app.with_file_name(name), crashed.path().join(name)).unwrap();
    }
    match read_db(&crashed.path().join("app.db")) {
        Err(BeekeeperError::Open { .. }) => {}
        other => panic!("{:?}", other.map(|r| r.connections.len())),
    }
    drop(writer);
}

#[test]
fn an_unexpected_column_type_is_a_query_error_that_quotes_no_value() {
    let (_d, app) = modern(
        "INSERT INTO saved_connection (id, name, connectionType, port) VALUES (1, 'Shop', 'postgresql', 'fake-pw-in-port');",
    );
    match read_db(&app) {
        Err(e @ BeekeeperError::Query { .. }) => {
            let text = format!("{e} {e:?}");
            assert!(text.contains("port") && !text.contains("fake-pw"), "{text}");
        }
        other => panic!("{:?}", other.map(|r| r.connections.len())),
    }
}

#[test]
fn reading_leaves_the_folder_byte_identical() {
    let (dir, app) = modern(
        "INSERT INTO saved_connection (id, name, connectionType) VALUES (1, 'Shop', 'sqlite');",
    );
    let before = files(dir.path());
    read_db(&app).unwrap();
    assert_eq!(files(dir.path()), before);
}

#[test]
fn debug_output_never_shows_the_stored_ciphertext() {
    let (_d, app) = modern(
        "INSERT INTO saved_connection (id, name, connectionType, password) VALUES (1, 'Shop', 'postgresql', 'cipher-fake-pw');",
    );
    let text = format!("{:?}", read_db(&app).unwrap().connections);
    assert!(
        text.contains("Shop") && !text.contains("cipher-fake-pw"),
        "{text}"
    );
}
