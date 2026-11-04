//! Vendored Chinook answers the same questions on SQLite and on every matrix server.
mod common;
use common::{LITE_TOML, dbento, write_tree};
use dbento_testkit::{SECRET_VAR, chinook_pg_sql, chinook_sqlite_sql, matrix};
use serde_json::json;

fn answers(stdout: &[u8]) -> Vec<serde_json::Value> {
    let doc: serde_json::Value = serde_json::from_slice(stdout).expect("json output");
    doc["statements"]
        .as_array()
        .expect("statements")
        .iter()
        .map(|s| s["rows"].clone())
        .collect()
}

#[test]
fn chinook_on_sqlite() {
    let ws = write_tree(&[("dbento.toml", LITE_TOML)]);
    std::fs::create_dir(ws.path().join("data")).unwrap();
    rusqlite::Connection::open(ws.path().join("data/app.db"))
        .unwrap()
        .execute_batch(chinook_sqlite_sql())
        .unwrap();
    let sql = "select count(*) from Track; select Name from Artist where ArtistId = 1";
    let out = dbento()
        .current_dir(ws.path())
        .args(["query", "lite", sql, "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        answers(&out.stdout),
        vec![json!([["3503"]]), json!([["AC/DC"]])]
    );
}

#[tokio::test]
async fn chinook_on_every_matrix_server() {
    for (label, db) in matrix().await {
        db.admin()
            .await
            .batch_execute(chinook_pg_sql())
            .await
            .unwrap_or_else(|e| panic!("{label}: {e}"));
        let toml = format!(
            "dbento = 1\n{}",
            db.connection_toml("pg", "read_only = true")
        );
        let ws = write_tree(&[("dbento.toml", toml.as_str())]);
        let sql = "select count(*) from track; select name from artist where artist_id = 1";
        let out = dbento()
            .current_dir(ws.path())
            .env(SECRET_VAR, db.password())
            .args(["query", "pg", sql, "--format", "json"])
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{label}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            answers(&out.stdout),
            vec![json!([["3503"]]), json!([["AC/DC"]])],
            "{label}"
        );
    }
}
