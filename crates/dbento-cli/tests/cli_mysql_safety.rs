//! The no-WHERE confirmation on MySQL and MariaDB, through `dbento query` as for every engine.
use crate::common::{dbento, write_tree};
use dbento_testkit::{MY_SECRET_VAR, MyDb, my_matrix};

fn workspace(db: &MyDb) -> tempfile::TempDir {
    let toml = format!("dbento = 1\n{}", db.connection_toml("my", ""));
    write_tree(&[("dbento.toml", toml.as_str())])
}

/// Exit code, stdout and stderr of `dbento` with `args`; `answer` goes to a pretended terminal.
fn cli(
    db: &MyDb,
    ws: &tempfile::TempDir,
    args: &[&str],
    answer: Option<&str>,
) -> (i32, String, String) {
    let mut cmd = dbento();
    cmd.current_dir(ws.path())
        .env(MY_SECRET_VAR, db.password())
        .args(args);
    if let Some(answer) = answer {
        cmd.env("DBENTO_TEST_ASSUME_TTY", "1").write_stdin(answer);
    }
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn reasons(stdout: &str) -> Vec<String> {
    let doc: serde_json::Value = serde_json::from_str(stdout).unwrap();
    doc["statements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["reason"].as_str().unwrap_or("").to_owned())
        .collect()
}

#[tokio::test]
async fn unguarded_mysql_writes_are_refused_without_a_terminal_and_asked_on_one() {
    for (label, db) in my_matrix().await {
        let ws = workspace(&db);
        let (code, _, err) = cli(
            &db,
            &ws,
            &[
                "query",
                "my",
                "CREATE TABLE t (id INT); CREATE TABLE u (id INT); INSERT INTO t VALUES (1), (2)",
            ],
            None,
        );
        assert_eq!(code, 0, "{label}: {err}");
        let (code, out, _) = cli(
            &db,
            &ws,
            &[
                "query",
                "my",
                "SELECT COUNT(*) AS n FROM t; DELETE FROM t",
                "--format",
                "json",
            ],
            None,
        );
        assert_eq!(
            (code, reasons(&out)),
            (2, vec![String::new(), "delete_without_where".to_owned()]),
            "{label}"
        );
        for (sql, reason) in [
            ("UPDATE t SET id = 3", "update_without_where"),
            ("UPDATE t, u SET t.id = 3", "update_without_where"),
            (
                "DELETE FROM t /*!99999 WHERE id = 1 */",
                "delete_without_where",
            ),
            ("WITH x AS (SELECT 1) DELETE FROM t", "delete_without_where"),
            ("TRUNCATE TABLE t", "truncate"),
            ("DROP TABLE t", "drop"),
        ] {
            let (code, out, _) = cli(&db, &ws, &["query", "my", sql, "--format", "json"], None);
            assert_eq!(
                (code, reasons(&out)),
                (2, vec![reason.to_owned()]),
                "{label}: {sql}"
            );
        }
        assert_eq!(
            db.scalar("SELECT COUNT(*) FROM t").await.as_deref(),
            Some("2"),
            "{label}"
        );
        let (code, _, _) = cli(
            &db,
            &ws,
            &["query", "my", "DELETE FROM t WHERE id = 9"],
            None,
        );
        assert_eq!(code, 0, "{label}: a guarded write runs without asking");
        let (code, _, err) = cli(&db, &ws, &["query", "my", "DELETE FROM t"], Some("y\n"));
        assert_eq!(code, 0, "{label}: {err}");
        assert!(
            err.contains("DELETE without WHERE on 'my'; run it? [y/N]"),
            "{label}: {err}"
        );
        assert_eq!(
            db.scalar("SELECT COUNT(*) FROM t").await.as_deref(),
            Some("0"),
            "{label}"
        );
    }
}

#[tokio::test]
async fn a_delete_hidden_by_no_backslash_escapes_is_still_asked_for() {
    for (label, db) in my_matrix().await {
        let ws = workspace(&db);
        let setup = "CREATE TABLE t (id INT); INSERT INTO t VALUES (1), (2)";
        assert_eq!(cli(&db, &ws, &["query", "my", setup], None).0, 0, "{label}");
        // Under NO_BACKSLASH_ESCAPES the server ends the string at \' and runs the DELETE.
        let sql =
            r"SET SESSION sql_mode = 'NO_BACKSLASH_ESCAPES'; SELECT 'a\'; DELETE FROM t; -- '";
        let (code, out, _) = cli(&db, &ws, &["query", "my", sql, "--format", "json"], None);
        assert_eq!(
            (code, reasons(&out)),
            (2, vec![String::new(), "delete_without_where".to_owned()]),
            "{label}"
        );
        assert_eq!(
            db.scalar("SELECT COUNT(*) FROM t").await.as_deref(),
            Some("2"),
            "{label}"
        );
    }
}
