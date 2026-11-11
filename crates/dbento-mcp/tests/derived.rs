//! Derived connections over MCP: what is listed and exposed, and that each child is its own
//! connection for grants and sessions.
use crate::common::TestServer;
use dbento_core::Decision;
use dbento_testkit::pg_db;
use serde_json::{Value, json};

const TREE: &str = r#"dbento = 1
[connections.stayzy]
engine = "postgres"
host = "db.invalid"
database = "stayzy_user"
user = "app"
color = "red"
databases = ["booking", "user_db"]
[connections.wide]
engine = "postgres"
host = "db.invalid"
database = "postgres"
user = "app"
databases = "all"
[connections.lite]
engine = "sqlite"
path = "data/app.db"
"#;

fn names(list: &Value) -> Vec<String> {
    let mut v: Vec<String> = list["connections"]
        .as_array()
        .expect("connections")
        .iter()
        .map(|c| c["name"].as_str().expect("name").to_owned())
        .collect();
    v.sort();
    v
}

fn entry(list: &Value, name: &str) -> Value {
    list["connections"]
        .as_array()
        .expect("connections")
        .iter()
        .find(|c| c["name"] == name)
        .cloned()
        .unwrap_or_else(|| panic!("{name} is not listed"))
}

#[tokio::test]
async fn list_connections_shows_parents_and_listed_children() {
    let server = TestServer::start(TREE).await;
    let list = server.ok("list_connections", json!({})).await;
    assert_eq!(
        names(&list),
        ["lite", "stayzy", "stayzy/booking", "stayzy/user_db", "wide"]
    );
    assert_eq!(entry(&list, "stayzy")["databases"], "listed");
    assert_eq!(entry(&list, "stayzy")["parent"], Value::Null);
    assert_eq!(entry(&list, "wide")["databases"], "all");
    assert_eq!(entry(&list, "stayzy/booking")["parent"], "stayzy");
    assert_eq!(entry(&list, "stayzy/booking")["databases"], Value::Null);
    assert_eq!(entry(&list, "stayzy/booking")["production"], true);
    assert_eq!(entry(&list, "lite")["databases"], Value::Null);
}

#[tokio::test]
async fn with_no_expose_list_every_child_resolves_and_an_unlisted_one_does_not() {
    let server = TestServer::start(TREE).await;
    let e = server
        .err("list_schemas", json!({"connection": "stayzy/hotel"}))
        .await;
    assert_eq!(e["code"], "no_connection", "{e}");
    for name in ["stayzy/booking", "wide/anything"] {
        let e = server
            .err("list_schemas", json!({"connection": name}))
            .await;
        assert_ne!(e["code"], "no_connection", "{name}: {e}");
    }
}

#[tokio::test]
async fn a_parent_exposes_its_children_and_a_child_only_itself() {
    let server = TestServer::start_with(TREE, |o| o.expose(["stayzy"])).await;
    let list = server.ok("list_connections", json!({})).await;
    assert_eq!(names(&list), ["stayzy", "stayzy/booking", "stayzy/user_db"]);
    let e = server
        .err("list_schemas", json!({"connection": "stayzy/hotel"}))
        .await;
    assert_eq!(e["code"], "no_connection", "{e}");

    let server = TestServer::start_with(TREE, |o| o.expose(["stayzy/booking"])).await;
    let list = server.ok("list_connections", json!({})).await;
    assert_eq!(names(&list), ["stayzy/booking"]);
    for name in ["stayzy", "stayzy/user_db"] {
        let e = server
            .err("list_schemas", json!({"connection": name}))
            .await;
        assert_eq!(e["code"], "no_connection", "{name}: {e}");
    }

    // An "all" child is exposed by its parent though never listed: it gets as far as connecting.
    let server = TestServer::start_with(TREE, |o| o.expose(["wide"])).await;
    let e = server
        .err("list_schemas", json!({"connection": "wide/anything"}))
        .await;
    assert_ne!(e["code"], "no_connection", "{e}");
}

#[tokio::test]
async fn a_childs_saved_queries_follow_its_exposure() {
    let server = TestServer::start_with(TREE, |o| o.expose(["stayzy/booking"])).await;
    for rel in ["queries/stayzy/q.sql", "queries/stayzy/booking/q.sql"] {
        let path = server.root().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "select 1").unwrap();
    }
    let list = server.ok("list_saved_queries", json!({})).await;
    assert_eq!(
        list["queries"],
        json!([{"path": "queries/stayzy/booking/q.sql", "connection": "stayzy/booking"}])
    );
}

#[tokio::test]
async fn a_grant_for_one_database_does_not_cover_another() {
    let db = pg_db().await;
    let other = format!("{}_b", db.database());
    let admin = db.admin().await;
    admin
        .batch_execute(&format!("CREATE DATABASE \"{other}\""))
        .await
        .unwrap();
    let extra = format!("databases = [\"{}\", \"{other}\"]", db.database());
    let server = TestServer::postgres(&db, &extra).await;
    let (a, b) = (format!("pg/{}", db.database()), format!("pg/{other}"));
    let sql = "CREATE TABLE granted(x int)";
    server.decided(&a, sql, Decision::Approve).await;
    let e = server
        .err("execute", json!({"connection": &b, "sql": sql}))
        .await;
    assert_eq!(e["code"], "approval_required", "{e}");
    server
        .ok("execute", json!({"connection": &a, "sql": sql}))
        .await;
    let here = server
        .ok(
            "query",
            json!({"connection": &a, "sql": "SELECT current_database()"}),
        )
        .await;
    assert_eq!(here["rows"], json!([[db.database()]]));
    let there = server
        .ok(
            "query",
            json!({"connection": &b, "sql": "SELECT current_database()"}),
        )
        .await;
    assert_eq!(there["rows"], json!([[&other]]));
    drop(server);
    admin
        .batch_execute(&format!("DROP DATABASE \"{other}\" WITH (FORCE)"))
        .await
        .unwrap();
}
