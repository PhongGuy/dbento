//! The no-WHERE confirmation and the best-effort note on SQL Server, through `dbento query`.
use crate::common::{dbento, write_tree};
use dbento_testkit::{MS_SECRET_VAR, MsDb, mssql_db};

const READER: &str = "CREATE USER dbento_reader WITHOUT LOGIN; \
    ALTER ROLE db_datareader ADD MEMBER dbento_reader; \
    ALTER ROLE db_denydatawriter ADD MEMBER dbento_reader; \
    GRANT VIEW DEFINITION TO dbento_reader; \
    DENY ALTER ANY USER TO dbento_reader";

fn workspace(db: &MsDb, extra: &str) -> tempfile::TempDir {
    let toml = format!(
        "dbento = 1\n{}",
        db.connection_toml("ms", &format!("tls = \"off\"\n{extra}"))
    );
    write_tree(&[("dbento.toml", toml.as_str())])
}

/// Exit code, stdout and stderr of `dbento` with `args`; `answer` goes to a pretended terminal.
fn cli(
    db: &MsDb,
    ws: &tempfile::TempDir,
    args: &[&str],
    answer: Option<&str>,
) -> (i32, String, String) {
    let mut cmd = dbento();
    cmd.current_dir(ws.path())
        .env(MS_SECRET_VAR, db.password())
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
async fn unguarded_sql_server_writes_are_refused_without_a_terminal_and_asked_on_one() {
    dbento_testkit::skip_without_mssql!();
    let db = mssql_db().await;
    db.scalar("CREATE TABLE dbo.t (id int PRIMARY KEY, v nvarchar(10))")
        .await;
    db.scalar("INSERT dbo.t VALUES (1, N'a'), (2, N'b'), (3, N'c')")
        .await;
    let ws = workspace(&db, "");
    let script = "SELECT COUNT(*) AS n FROM dbo.t\nGO\nDELETE FROM dbo.t";
    let (code, out, _) = cli(&db, &ws, &["query", "ms", script, "--format", "json"], None);
    assert_eq!(code, 2);
    assert_eq!(reasons(&out), ["", "delete_without_where"]);
    let (code, _, err) = cli(
        &db,
        &ws,
        &["query", "ms", "UPDATE dbo.t SET v = N'z'"],
        Some("n\n"),
    );
    assert_eq!(code, 2, "{err}");
    assert!(
        err.contains("UPDATE without WHERE on 'ms'; run it? [y/N]"),
        "{err}"
    );
    assert_eq!(
        db.scalar("SELECT COUNT(*) FROM dbo.t WHERE v = N'z'")
            .await
            .as_deref(),
        Some("0")
    );
    let (code, _, err) = cli(
        &db,
        &ws,
        &["query", "ms", "DELETE FROM dbo.t WHERE id = 9"],
        None,
    );
    assert_eq!(code, 0, "{err}");
    // A batch asks once, for its first result; its later results ride on that answer.
    let batch = "DELETE FROM dbo.t; SELECT COUNT(*) AS n FROM dbo.t";
    let (code, _, err) = cli(&db, &ws, &["query", "ms", batch], Some("y\n"));
    assert_eq!(code, 0, "{err}");
    assert_eq!(err.matches("run it? [y/N]").count(), 1, "{err}");
    assert_eq!(
        db.scalar("SELECT COUNT(*) FROM dbo.t").await.as_deref(),
        Some("0")
    );
}

#[tokio::test]
async fn a_best_effort_read_only_session_says_so_on_stderr() {
    dbento_testkit::skip_without_mssql!();
    let db = mssql_db().await;
    let ws = workspace(&db, "");
    let (code, _, err) = cli(
        &db,
        &ws,
        &["query", "ms", "SELECT 1 AS one", "--read-only"],
        None,
    );
    assert_eq!(code, 0, "{err}");
    assert!(
        err.contains("best effort") && err.contains("read_only_user"),
        "{err}"
    );
    db.scalar("CREATE TABLE dbo.t (a int)").await;
    let (code, _, err) = cli(
        &db,
        &ws,
        &["query", "ms", "INSERT dbo.t VALUES (1)", "--read-only"],
        None,
    );
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("without read_only_user, so Dbento refused the batch unsent")
            && err.contains("(read_only)"),
        "{err}"
    );
    assert_eq!(
        db.scalar("SELECT COUNT(*) FROM dbo.t").await.as_deref(),
        Some("0")
    );
    let (code, _, err) = cli(&db, &ws, &["query", "ms", "SELECT 1 AS one"], None);
    assert_eq!(code, 0, "{err}");
    assert!(!err.contains("best effort"), "{err}");
    db.scalar(READER).await;
    let ws = workspace(&db, "read_only = true\nread_only_user = \"dbento_reader\"");
    let (code, out, err) = cli(
        &db,
        &ws,
        &["query", "ms", "SELECT USER_NAME() AS u", "--format", "json"],
        None,
    );
    assert_eq!(code, 0, "{err}");
    assert!(!err.contains("best effort"), "{err}");
    assert!(out.contains("dbento_reader"), "{out}");
}
