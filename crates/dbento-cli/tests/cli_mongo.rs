//! MongoDB through `dbento query` and `dbento run`: each row's document in JSON, the
//! refusal reasons and syntax code, and the saved-file language check.
#![cfg(feature = "mongodb")]
use crate::common::{dbento, write_tree};
use dbento_testkit::{MG_SECRET_VAR, MgDb, MgUser, mongo_db};
use serde_json::{Value, json};

fn workspace(db: &MgDb, user: MgUser, files: &[(&str, &str)]) -> tempfile::TempDir {
    let toml = format!("dbento = 1\n{}", db.connection_toml("mg", user, ""));
    let mut all = vec![("dbento.toml", toml.as_str())];
    all.extend_from_slice(files);
    write_tree(&all)
}

/// Exit code, stdout and stderr of `dbento <args>` in `ws`, logged in as `user`.
fn dbento_in(
    db: &MgDb,
    user: MgUser,
    ws: &tempfile::TempDir,
    args: &[&str],
) -> (i32, String, String) {
    let mut cmd = dbento();
    cmd.current_dir(ws.path()).args(args);
    if let Some(password) = db.password(user) {
        cmd.env(MG_SECRET_VAR, password);
    }
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
async fn json_output_carries_each_rows_document() {
    let db = mongo_db().await;
    let ws = workspace(&db, MgUser::Writer, &[]);
    let w = |text: &str| {
        dbento_in(
            &db,
            MgUser::Writer,
            &ws,
            &["query", "mg", text, "--format", "json"],
        )
    };
    let (code, _, err) = w("db.shapes.insertMany([{_id: 1, a: null}, {_id: 2, b: NumberLong(5)}])");
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = w("db.shapes.find().sort({_id: 1})");
    assert_eq!(code, 0, "{err}");
    let s = statements(&out);
    let names: Vec<&str> = s[0]["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["_id", "a", "b"], "{out}");
    assert_eq!(
        s[0]["rows"],
        json!([
            {"cells": ["1", null, null], "document": {"_id": 1, "a": null}},
            {"cells": ["2", null, "5"], "document": {"_id": 2, "b": 5}}
        ]),
        "{out}"
    );
}

#[tokio::test]
async fn empty_filter_writes_are_refused_by_reason_and_javascript_is_a_syntax_error() {
    let db = mongo_db().await;
    let ws = workspace(&db, MgUser::Writer, &[]);
    let run = |text: &str| {
        dbento_in(
            &db,
            MgUser::Writer,
            &ws,
            &["query", "mg", text, "--format", "json"],
        )
    };
    let (code, out, err) = run("db.many.deleteMany({})");
    assert_eq!(code, 2, "{err}");
    assert_eq!(
        statements(&out)[0]["reason"],
        "delete_all_documents",
        "{out}"
    );
    let (code, out, err) = run("db.many.updateMany({}, {$set: {x: 1}})");
    assert_eq!(code, 2, "{err}");
    assert_eq!(
        statements(&out)[0]["reason"],
        "update_all_documents",
        "{out}"
    );
    assert_eq!(db.count("many").await, 10_000);
    let (code, out, err) = run("let x = 1");
    assert_eq!(code, 1, "{err}");
    let s = statements(&out);
    assert_eq!(s[0]["error"]["code"], "syntax", "{out}");
    assert!(
        s[0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("syntax error"),
        "{out}"
    );
}

#[tokio::test]
async fn a_saved_js_file_runs_and_a_sql_file_is_refused() {
    let db = mongo_db().await;
    let files = [
        ("queries/mg/seven.js", "db.many.find({_id: 7})\n"),
        ("queries/mg/old.sql", "SELECT 1\n"),
    ];
    let ws = workspace(&db, MgUser::Reader, &files);
    let (code, out, err) = dbento_in(
        &db,
        MgUser::Reader,
        &ws,
        &["run", "queries/mg/seven.js", "--format", "json"],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        statements(&out)[0]["rows"][0]["cells"],
        json!(["7", "7", "row 7"]),
        "{out}"
    );
    let (code, _, err) = dbento_in(&db, MgUser::Reader, &ws, &["run", "queries/mg/old.sql"]);
    assert_eq!(code, 3, "{err}");
    assert!(
        err.contains("MongoDB shell") && err.contains(".js"),
        "{err}"
    );
}
