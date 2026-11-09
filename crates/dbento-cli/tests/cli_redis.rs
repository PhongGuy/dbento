//! Redis through `dbento query` and `dbento run`: a value prints, a flush is refused by its
//! reason, a best-effort session names the reader ACL, and saved files keep their language.
#![cfg(feature = "redis")]
use crate::common::{dbento, write_tree};
use dbento_testkit::{RD_SECRET_VAR, RdUser, RedisDb, redis_db};
use serde_json::{Value, json};

/// `rd` reader read-only, `wr` writer, `wo` writer read-only, plus `files`.
fn workspace(db: &RedisDb, files: &[(&str, &str)]) -> tempfile::TempDir {
    let toml = format!(
        "dbento = 1\n{}{}{}",
        db.connection_toml("rd", RdUser::Reader, "read_only = true"),
        db.connection_toml("wr", RdUser::Writer, ""),
        db.connection_toml("wo", RdUser::Writer, "read_only = true")
    );
    let mut all = vec![("dbento.toml", toml.as_str())];
    all.extend_from_slice(files);
    write_tree(&all)
}

/// Exit code, stdout and stderr of `dbento <args>` in `ws`; reader and writer share a password.
fn dbento_in(db: &RedisDb, ws: &tempfile::TempDir, args: &[&str]) -> (i32, String, String) {
    let mut cmd = dbento();
    cmd.current_dir(ws.path())
        .args(args)
        .env(RD_SECRET_VAR, db.password(RdUser::Reader).unwrap());
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn statements(stdout: &str) -> Vec<Value> {
    let doc: Value = serde_json::from_str(stdout).unwrap();
    doc["statements"].as_array().unwrap().clone()
}

#[tokio::test]
async fn a_value_prints_and_a_flush_is_refused_by_its_reason() {
    let db = redis_db().await;
    let ws = workspace(&db, &[]);
    let q =
        |conn: &str, text: &str| dbento_in(&db, &ws, &["query", conn, text, "--format", "json"]);
    let (code, _, err) = q("wr", "SET clirt:k hello");
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = q("rd", "GET clirt:k");
    assert_eq!(code, 0, "{err}");
    let s = statements(&out);
    assert_eq!(s[0]["columns"][0]["name"], "value", "{out}");
    assert_eq!(s[0]["rows"], json!([["hello"]]), "{out}");
    let (code, out, err) = q("wr", "FLUSHDB");
    assert_eq!(code, 2, "{err}");
    assert_eq!(statements(&out)[0]["reason"], "flush_keys", "{out}");
    let (code, out, err) = q("rd", "EXISTS clirt:k");
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        statements(&out)[0]["rows"],
        json!([["1"]]),
        "the flush never ran: {out}"
    );
}

#[tokio::test]
async fn a_best_effort_session_names_the_reader_acl() {
    let db = redis_db().await;
    let ws = workspace(&db, &[]);
    let (code, _, err) = dbento_in(&db, &ws, &["query", "wo", "GET clirt:none"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        err.contains("best effort") && err.contains("dbento_ro"),
        "{err}"
    );
    let (code, _, err) = dbento_in(&db, &ws, &["query", "rd", "GET clirt:none"]);
    assert_eq!(code, 0, "{err}");
    assert!(!err.contains("best effort"), "{err}");
}

#[tokio::test]
async fn a_saved_redis_file_runs_and_a_sql_file_is_refused() {
    let db = redis_db().await;
    let files = [
        (
            "queries/rd/get.redis",
            "# nothing is stored here\nGET clirt:absent\n",
        ),
        ("queries/rd/old.sql", "SELECT 1\n"),
    ];
    let ws = workspace(&db, &files);
    let (code, out, err) = dbento_in(
        &db,
        &ws,
        &["run", "queries/rd/get.redis", "--format", "json"],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(statements(&out)[0]["rows"], json!([[null]]), "{out}");
    let (code, _, err) = dbento_in(&db, &ws, &["run", "queries/rd/old.sql"]);
    assert_eq!(code, 3, "{err}");
    assert!(
        err.contains("Redis commands") && err.contains(".redis"),
        "{err}"
    );
}
