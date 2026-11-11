//! The CLI on derived connections: `query`, `run` and `connections` by `<parent>/<database>`.
use crate::common::{dbento, write_tree};
use dbento_testkit::{SECRET_VAR, pg_db};

const OFFLINE: &str = "dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"db.invalid\"\ndatabase = \"d\"\nuser = \"u\"\ndatabases = [\"a\", \"b\"]\n[connections.wide]\nengine = \"postgres\"\nhost = \"db.invalid\"\ndatabase = \"d\"\nuser = \"u\"\ndatabases = \"all\"\n[connections.plain]\nengine = \"postgres\"\nhost = \"db.invalid\"\ndatabase = \"d\"\nuser = \"u\"\n";

#[tokio::test]
async fn query_and_run_reach_a_child_database() {
    let db = pg_db().await;
    let other = format!("{}_c", db.database());
    let admin = db.admin().await;
    admin
        .batch_execute(&format!("CREATE DATABASE \"{other}\""))
        .await
        .unwrap();
    let toml = format!(
        "dbento = 1\n{}",
        db.connection_toml("pg", &format!("databases = [\"{other}\"]"))
    );
    let file = format!("queries/pg/{other}/q.sql");
    let ws = write_tree(&[
        ("dbento.toml", toml.as_str()),
        (file.as_str(), "select current_database()"),
    ]);
    let child = format!("pg/{other}");
    for args in [
        vec![
            "query",
            child.as_str(),
            "select current_database()",
            "--format",
            "json",
        ],
        vec!["run", file.as_str(), "--format", "json"],
    ] {
        let out = dbento()
            .current_dir(ws.path())
            .env(SECRET_VAR, db.password())
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(doc["connection"], child.as_str(), "{args:?}");
        assert_eq!(
            doc["statements"][0]["rows"],
            serde_json::json!([[&other]]),
            "{args:?}"
        );
    }
    admin
        .batch_execute(&format!("DROP DATABASE \"{other}\" WITH (FORCE)"))
        .await
        .unwrap();
}

#[test]
fn a_name_that_does_not_resolve_says_why_before_connecting() {
    let ws = write_tree(&[("dbento.toml", OFFLINE)]);
    for (name, says) in [
        (
            "pg/c",
            "\"c\" is not in the `databases` list of connection \"pg\"",
        ),
        ("plain/x", "connection \"plain\" sets no `databases`"),
        ("wide/my.db", "database name \"my.db\" must match"),
        ("nope", "no connection named \"nope\""),
    ] {
        let out = dbento()
            .current_dir(ws.path())
            .args(["query", name, "select 1"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(3), "{name}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(says), "{name}: {err}");
    }
}

#[test]
fn connections_lists_parents_and_listed_children() {
    let ws = write_tree(&[("dbento.toml", OFFLINE)]);
    let out = dbento()
        .current_dir(ws.path())
        .args(["connections", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let rows = doc["connections"].as_array().unwrap();
    let names: Vec<&str> = rows.iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["pg", "pg/a", "pg/b", "plain", "wide"]);
    assert!(
        rows[1]["target"].as_str().unwrap().ends_with("/a"),
        "{}",
        rows[1]
    );
}

#[test]
fn the_mcp_help_says_a_parent_exposes_its_databases() {
    let out = dbento().args(["mcp", "--help"]).output().unwrap();
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("databases = \"all\""), "{help}");
}
