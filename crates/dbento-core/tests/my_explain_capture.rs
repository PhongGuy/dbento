//! Captures EXPLAIN output from every MySQL and MariaDB matrix server as parser fixtures, and
//! checks the server behaviour the request forms and parsers rely on. Run by hand only.
use std::path::PathBuf;

use crate::support::my_fixtures;
use dbento_testkit::MyFlavor;
use mysql_async::prelude::Queryable;
use serde_json::Value;

/// `cust` 200 rows in 10 regions, `ord` 1000 rows, 5 per customer.
pub(crate) const SHAPES_SETUP: &str = "CREATE TABLE d (i INT PRIMARY KEY);
INSERT INTO d VALUES (0), (1), (2), (3), (4), (5), (6), (7), (8), (9);
CREATE TABLE cust (id INT PRIMARY KEY, name VARCHAR(20), region INT, KEY region (region)) ENGINE=InnoDB;
CREATE TABLE ord (id INT PRIMARY KEY, cust_id INT, amount DECIMAL(10,2), KEY cust_id (cust_id)) ENGINE=InnoDB;
INSERT INTO cust SELECT c.i * 100 + a.i * 10 + b.i + 1, CONCAT('c', c.i * 100 + a.i * 10 + b.i + 1), b.i
  FROM d a, d b, d c WHERE c.i < 2;
INSERT INTO ord SELECT c.i * 100 + a.i * 10 + b.i + 1, (c.i * 100 + a.i * 10 + b.i) % 200 + 1,
  (c.i * 100 + a.i * 10 + b.i) + 0.5 FROM d a, d b, d c;
ANALYZE TABLE cust, ord;";

/// (name, statement, reads); reads are captured in the analyze form too.
pub(crate) const SHAPES: &[(&str, &str, bool)] = &[
    (
        "join_group",
        "SELECT c.name, SUM(o.amount) FROM cust c JOIN ord o ON o.cust_id = c.id WHERE c.region = 3 GROUP BY c.name",
        true,
    ),
    (
        "union",
        "SELECT id FROM cust WHERE region = 1 UNION SELECT cust_id FROM ord WHERE amount > 900",
        true,
    ),
    (
        "filesort",
        "SELECT id, amount FROM ord ORDER BY amount DESC, id LIMIT 5",
        true,
    ),
    (
        "where_subquery",
        "SELECT name FROM cust WHERE id = (SELECT MAX(cust_id) FROM ord)",
        true,
    ),
    (
        "derived",
        "SELECT d.r, d.n FROM (SELECT region AS r, COUNT(*) AS n FROM cust GROUP BY region) d WHERE d.n > 1",
        true,
    ),
    ("distinct", "SELECT DISTINCT region FROM cust", true),
    ("select_one", "SELECT 1", true),
    (
        "window",
        "SELECT id, SUM(amount) OVER (PARTITION BY cust_id ORDER BY id) AS running FROM ord",
        true,
    ),
    (
        "recursive_cte",
        "WITH RECURSIVE n (i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 5) SELECT i FROM n",
        true,
    ),
    (
        "in_exists",
        "SELECT name FROM cust c WHERE c.id IN (SELECT cust_id FROM ord WHERE amount > 500) AND EXISTS (SELECT 1 FROM ord o WHERE o.cust_id = c.id AND o.amount < 100)",
        true,
    ),
    (
        "dependent_sleep",
        "SELECT c.id, (SELECT SLEEP(0.02) FROM cust c2 WHERE c2.id = c.id) AS s FROM cust c WHERE c.id <= 5",
        true,
    ),
    (
        "long_filter",
        "SELECT id FROM ord WHERE amount + 1 > 2 AND amount * 2 < 50000 AND cust_id + id > 3 AND cust_id * 7 < 100000 AND id - cust_id > -1000000 AND amount / 3 < 99999 AND id + cust_id + amount < 123456789",
        true,
    ),
    (
        "insert_values",
        "INSERT INTO ord (id, cust_id, amount) VALUES (5001, 1, 9.99)",
        false,
    ),
    (
        "replace_values",
        "REPLACE INTO ord (id, cust_id, amount) VALUES (1, 1, 9.99)",
        false,
    ),
    (
        "insert_select",
        "INSERT INTO ord (id, cust_id, amount) SELECT id + 5000, cust_id, amount FROM ord WHERE id < 10",
        false,
    ),
    (
        "update",
        "UPDATE ord SET amount = amount + 1 WHERE cust_id = 5",
        false,
    ),
    ("delete", "DELETE FROM ord WHERE id > 990", false),
    (
        "multi_update",
        "UPDATE ord o JOIN cust c ON c.id = o.cust_id SET o.amount = 0 WHERE c.region = 7",
        false,
    ),
    (
        "multi_delete",
        "DELETE o FROM ord o JOIN cust c ON c.id = o.cust_id WHERE c.region = 8",
        false,
    ),
];

fn fixture_dir() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/explain/my"
    ))
}

/// Every `r_total_time_ms` of an object that ran exactly five loops.
fn five_loop_times(v: &Value, out: &mut Vec<f64>) {
    match v {
        Value::Object(m) => {
            if m.get("r_loops").and_then(Value::as_u64) == Some(5)
                && let Some(t) = m.get("r_total_time_ms").and_then(Value::as_f64)
            {
                out.push(t);
            }
            m.values().for_each(|c| five_loop_times(c, out));
        }
        Value::Array(items) => items.iter().for_each(|c| five_loop_times(c, out)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "captures fixtures from the five matrix servers; run by hand"]
async fn capture_explain_fixtures() {
    let dir = fixture_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let (mut labels, mut errors) = (Vec::new(), Vec::new());
    for (label, fx) in my_fixtures("").await {
        fx.exec(SHAPES_SETUP).await;
        let maria = fx.db.flavor() == MyFlavor::Mariadb;
        let mut c = fx.db.admin().await;
        let version: Option<String> = c.query_first("SELECT VERSION()").await.unwrap();
        println!("{label}: {version:?}");
        // Pin JSON v1 on MySQL as Session::explain does, and restore the value read afterwards.
        let mut pinned = None;
        if !maria {
            let global: Option<String> = c
                .query_first("SELECT CAST(@@global.explain_json_format_version AS CHAR)")
                .await
                .unwrap();
            let want = match label.as_str() {
                "mysql84" => "1",
                "mysql97" => "2",
                other => panic!("{other}: not a matrix MySQL label"),
            };
            assert_eq!(
                global.as_deref(),
                Some(want),
                "{label}: global explain_json_format_version"
            );
            let read: Option<String> = c
                .query_first("SELECT CAST(@@session.explain_json_format_version AS CHAR)")
                .await
                .unwrap();
            let read = read.unwrap();
            if read != "1" {
                c.query_drop("SET SESSION explain_json_format_version = 1")
                    .await
                    .unwrap();
            }
            pinned = Some(read);
        }
        for (name, sql, reads) in SHAPES {
            let mut forms = vec![("plan", format!("EXPLAIN FORMAT=JSON {sql}"), "json")];
            if *reads {
                forms.push(if maria {
                    ("analyze", format!("ANALYZE FORMAT=JSON {sql}"), "json")
                } else {
                    (
                        "analyze",
                        format!("EXPLAIN ANALYZE FORMAT=TREE {sql}"),
                        "txt",
                    )
                });
            }
            for (form, text, ext) in forms {
                c.query_drop("START TRANSACTION").await.unwrap();
                let got: Result<Option<String>, _> = c.query_first(text.as_str()).await;
                c.query_drop("ROLLBACK").await.unwrap();
                let (file, body) = match got {
                    Ok(Some(out)) => (format!("{label}-{form}-{name}.{ext}"), out),
                    Ok(None) => panic!("{label} {form} {name}: no output"),
                    Err(mysql_async::Error::Server(e)) => {
                        errors.push(format!("{label} {form} {name}: {} {}", e.code, e.message));
                        (
                            format!("{label}-{form}-{name}.error.txt"),
                            format!("ERROR {}: {}", e.code, e.message),
                        )
                    }
                    Err(e) => panic!("{label} {form} {name}: {e}"),
                };
                if ext == "txt" && !file.ends_with(".error.txt") {
                    for line in body.trim_end().lines() {
                        let rest = line.trim_start_matches(' ');
                        let indent = line.len() - rest.len();
                        assert!(
                            indent.is_multiple_of(4) && rest.starts_with("-> "),
                            "{file}: a tree line outside the grammar (a wrapped label?): {line:?}"
                        );
                    }
                }
                if maria && form == "analyze" && *name == "dependent_sleep" {
                    let mut times = Vec::new();
                    five_loop_times(&serde_json::from_str(&body).unwrap(), &mut times);
                    println!("{label}: five-loop r_total_time_ms {times:?}");
                    assert!(
                        times.iter().any(|t| *t >= 80.0),
                        "{label}: no five-loop block spent 5 x 20 ms; r_total_time_ms is not a total: {times:?}"
                    );
                }
                std::fs::write(dir.join(file), body).unwrap();
            }
        }
        if let Some(read) = pinned {
            if read != "1" {
                c.query_drop(format!("SET SESSION explain_json_format_version = {read}"))
                    .await
                    .unwrap();
            }
            let after: Option<String> = c
                .query_first("SELECT CAST(@@session.explain_json_format_version AS CHAR)")
                .await
                .unwrap();
            assert_eq!(
                after.as_deref(),
                Some(read.as_str()),
                "{label}: the value read was not restored"
            );
        }
        c.query_drop("START TRANSACTION READ ONLY").await.unwrap();
        for sql in [
            "INSERT INTO ord (id, cust_id, amount) VALUES (9001, 1, 1)",
            "REPLACE INTO ord (id, cust_id, amount) VALUES (1, 1, 1)",
            "UPDATE ord SET amount = 0 WHERE id = 1",
            "DELETE FROM ord WHERE id = 1",
        ] {
            let e = c.query_drop(format!("EXPLAIN {sql}")).await.expect_err(sql);
            assert!(
                matches!(&e, mysql_async::Error::Server(s) if s.code == 1792),
                "{label}: EXPLAIN {sql} in a READ ONLY transaction: {e}"
            );
        }
        c.query_drop("ROLLBACK").await.unwrap();
        c.disconnect().await.unwrap();
        labels.push(label);
    }
    assert!(errors.is_empty(), "shapes the servers refused: {errors:#?}");
    assert_eq!(
        labels,
        [
            "mysql84",
            "mysql97",
            "mariadb1011",
            "mariadb114",
            "mariadb123"
        ]
    );
}
