//! What goes to a MySQL server and what comes back: scripts, literals, values and counts.
mod support;
use dbento_core::{CellRef, ColumnSource, Engine, Limits, SessionOptions, quote_literal};
use mysql_async::prelude::*;
use support::{Cell, my_fixtures, run, text};

#[test]
fn mysql_literals_quote_for_either_backslash_mode() {
    assert_eq!(quote_literal(Engine::Mysql, "it's"), "'it''s'");
    assert_eq!(
        quote_literal(Engine::Mysql, "a\\b"),
        "CONVERT(X'615c62' USING utf8mb4)"
    );
}

#[tokio::test]
async fn quoted_literals_read_back_under_both_backslash_modes() {
    for (label, fx) in my_fixtures("").await {
        for mode in ["", "NO_BACKSLASH_ESCAPES"] {
            let mut s = fx.session(SessionOptions::default()).await;
            run(
                &mut s,
                &format!("SET SESSION sql_mode = '{mode}'"),
                Limits::default(),
            )
            .await
            .remove(0)
            .unwrap();
            for value in ["it's", "a\\b", "é\\\n'"] {
                let sql = format!("SELECT {} AS v", quote_literal(Engine::Mysql, value));
                let ran = run(&mut s, &sql, Limits::default())
                    .await
                    .remove(0)
                    .unwrap();
                assert_eq!(ran.rows, vec![vec![text(value)]], "{label} {mode:?}: {sql}");
            }
        }
    }
}

#[tokio::test]
async fn a_delimiter_script_creates_and_calls_a_procedure() {
    for (label, fx) in my_fixtures("").await {
        let mut s = fx.session(SessionOptions::default()).await;
        let sql = "DELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1 AS a; SELECT 2 AS b; END //\nDELIMITER ;\nCALL p();\nSELECT 3 AS c";
        let ran: Vec<_> = run(&mut s, sql, Limits::default())
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();
        let sent: Vec<&str> = ran.iter().map(|r| r.sql.as_str()).collect();
        assert_eq!(
            sent,
            [
                "CREATE PROCEDURE p() BEGIN SELECT 1 AS a; SELECT 2 AS b; END",
                "CALL p()",
                "SELECT 3 AS c"
            ],
            "{label}"
        );
        assert_eq!(ran[1].rows, vec![vec![text("1")]], "{label}");
        assert_eq!(ran[2].rows, vec![vec![text("3")]], "{label}");
    }
}

#[tokio::test]
async fn values_arrive_as_the_servers_text_or_bytes_with_their_types() {
    for (label, fx) in my_fixtures("").await {
        fx.db.admin().await
            .query_drop("CREATE TABLE v_t (id INT PRIMARY KEY, u BIGINT UNSIGNED, b VARBINARY(4), t VARCHAR(5)); \
                         INSERT INTO v_t VALUES (1, 18446744073709551615, X'616263', 'é'), (2, NULL, X'00FF', NULL)")
            .await
            .unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        let sql = "SELECT id, u, b, t, 'lit' AS x FROM v_t ORDER BY id";
        let ran = run(&mut s, sql, Limits::default()).await.remove(0).unwrap();
        let types: Vec<(&str, &str)> = ran
            .columns
            .iter()
            .map(|(n, t)| (n.as_str(), t.as_str()))
            .collect();
        assert_eq!(
            types,
            [
                ("id", "INT"),
                ("u", "BIGINT UNSIGNED"),
                ("b", "VARBINARY"),
                ("t", "VARCHAR"),
                ("x", "VARCHAR")
            ],
            "{label}"
        );
        assert_eq!(
            ran.rows,
            vec![
                vec![
                    text("1"),
                    text("18446744073709551615"),
                    Cell::Bytes(b"abc".to_vec()),
                    text("é"),
                    text("lit")
                ],
                vec![
                    text("2"),
                    Cell::Null,
                    Cell::Bytes(vec![0, 255]),
                    Cell::Null,
                    text("lit")
                ],
            ],
            "{label}"
        );
        // Sources and the number bit, from one batch; the unread rest is drained later.
        let mut script = s.script(sql);
        let stream = &mut script
            .next()
            .await
            .unwrap()
            .unwrap()
            .execute(Limits::default())
            .await
            .unwrap();
        let cols = stream.columns().to_vec();
        assert!(
            matches!(cols[0].source(), Some(ColumnSource::Mysql { schema, table, column, .. })
                if schema == fx.db.database() && table == "v_t" && column == "id"),
            "{label}: {:?}",
            cols[0].source()
        );
        assert_eq!(cols[4].source(), None, "{label}");
        let batch = stream.next_batch().await.unwrap().unwrap();
        assert_eq!(batch.cell(0, 0), CellRef::Text("1"));
        assert!(
            batch.is_number(0, 0) && batch.is_number(0, 1) && !batch.is_number(0, 3),
            "{label}"
        );
    }
}

#[tokio::test]
async fn bytes_that_are_not_utf8_arrive_as_bytes_never_null_or_lossy() {
    for (label, fx) in my_fixtures("").await {
        let mut s = fx.session(SessionOptions::default()).await;
        // With no result conversion the server sends the latin1 column's raw bytes, C3 28.
        let sql = "SET character_set_results = NULL; SELECT CAST(X'C328' AS CHAR CHARACTER SET latin1) AS l1";
        let ran = run(&mut s, sql, Limits::default()).await.remove(1).unwrap();
        assert_eq!(
            ran.rows,
            vec![vec![Cell::Bytes(vec![0xc3, 0x28])]],
            "{label}"
        );
    }
}

#[tokio::test]
async fn statements_without_rows_report_what_they_changed() {
    for (label, fx) in my_fixtures("").await {
        let mut s = fx.session(SessionOptions::default()).await;
        let sql = "CREATE TABLE r_t (a INT); INSERT INTO r_t VALUES (1), (2), (3); UPDATE r_t SET a = a; \
                   # none match\nDELETE FROM r_t WHERE a > 9";
        let affected: Vec<Option<u64>> = run(&mut s, sql, Limits::default())
            .await
            .into_iter()
            .map(|r| r.unwrap().outcome.rows_affected)
            .collect();
        assert_eq!(affected, [None, Some(3), Some(3), Some(0)], "{label}");
        assert!(!s.settings_changed(), "{label}");
        run(&mut s, "-- a variable\nSET @x = 1", Limits::default())
            .await
            .remove(0)
            .unwrap();
        assert!(s.settings_changed(), "{label}");
        let mut v = fx.session(SessionOptions::default()).await;
        run(&mut v, "/*!40101 SET NAMES utf8mb4 */", Limits::default())
            .await
            .remove(0)
            .unwrap();
        assert!(v.settings_changed(), "{label}: a versioned SET");
    }
}
