//! DynamoDB through `dbento query`: items print with their documents, and a read-only
//! connection refuses a write before sending it and says why in IAM terms.
#![cfg(feature = "dynamodb")]
use crate::common::{dbento, write_tree};
use dbento_testkit::{DY_SECRET, DY_SECRET_VAR, DynamoDb, dynamo_db};
use serde_json::Value;

/// `dy` writable and `ro` read-only, both on the leased region.
fn workspace(db: &DynamoDb) -> tempfile::TempDir {
    let toml = format!(
        "dbento = 1\n{}{}",
        db.connection_toml("dy", ""),
        db.connection_toml("ro", "read_only = true")
    );
    write_tree(&[("dbento.toml", toml.as_str())])
}

fn dbento_in(ws: &tempfile::TempDir, args: &[&str]) -> (i32, String, String) {
    let out = dbento()
        .current_dir(ws.path())
        .args(args)
        .env(DY_SECRET_VAR, DY_SECRET)
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[tokio::test]
async fn json_rows_carry_the_item_as_plain_json() {
    let db = dynamo_db().await;
    let ws = workspace(&db);
    let (code, out, err) = dbento_in(
        &ws,
        &["query", "dy", "SELECT * FROM \"types\"", "--format", "json"],
    );
    assert_eq!(code, 0, "{err}");
    let doc: Value = serde_json::from_str(&out).unwrap();
    let row = &doc["statements"][0]["rows"][0];
    assert_eq!(row["document"]["id"], "t1", "{out}");
    assert_eq!(
        row["document"]["ss"],
        serde_json::json!(["x", "y"]),
        "{out}"
    );
    assert_eq!(row["document"]["bool"], true, "{out}");
    // A 38-digit N survives as written, which a float would not.
    assert!(
        out.contains("12345678901234567890123456789012345678"),
        "{out}"
    );
    assert!(
        doc["statements"][0].get("consumed_capacity").is_none(),
        "Moto reports none"
    );
}

#[tokio::test]
async fn a_read_only_connection_refuses_a_write_and_names_iam() {
    let db = dynamo_db().await;
    let ws = workspace(&db);
    let (code, out, err) = dbento_in(
        &ws,
        &[
            "query",
            "ro",
            "INSERT INTO \"h\" VALUE {'id': 'cli1'}",
            "--format",
            "json",
        ],
    );
    assert_eq!(code, 1, "{err}");
    let doc: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(doc["statements"][0]["error"]["code"], "read_only", "{out}");
    assert!(err.contains("best effort") && err.contains("IAM"), "{err}");
    assert_eq!(db.count("h").await, 0, "nothing was sent");
    let (code, _, err) = dbento_in(&ws, &["query", "ro", "SELECT * FROM \"h\""]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("(0 rows,"), "{err}");
}
