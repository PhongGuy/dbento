mod support;
use dbento_core::{Engine, Limits, SessionOptions, statement_ranges};
use support::sqlite_fixture;

fn texts(engine: Engine, sql: &str) -> Vec<&str> {
    statement_ranges(engine, sql)
        .into_iter()
        .map(|r| sql[r].trim())
        .collect()
}
fn pg(sql: &str) -> Vec<&str> {
    texts(Engine::Postgres, sql)
}
fn lite(sql: &str) -> Vec<&str> {
    texts(Engine::Sqlite, sql)
}

#[test]
fn postgres_ranges_are_the_scripts_pieces_without_blanks() {
    assert_eq!(pg("SELECT 1; SELECT 2"), ["SELECT 1;", "SELECT 2"]);
    assert_eq!(
        pg("SELECT ';'; -- c;\nSELECT $$a;b$$, E'x\\';y';"),
        ["SELECT ';';", "-- c;\nSELECT $$a;b$$, E'x\\';y';"]
    );
    let atomic = "CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT 1; \
                  SELECT CASE WHEN true THEN 2 END; END;";
    assert_eq!(pg(&format!("{atomic} SELECT 3")), [atomic, "SELECT 3"]);
    assert_eq!(pg("SELECT 1;; /* x; */ ;\n"), ["SELECT 1;"]);
    assert_eq!(pg("SELECT 'open; SELECT 2"), ["SELECT 'open; SELECT 2"]);
    assert!(pg("").is_empty() && pg("  -- only a comment\n").is_empty());
}

#[test]
fn sqlite_ranges_follow_sqlite_complete() {
    assert_eq!(lite("SELECT 1; SELECT 2"), ["SELECT 1;", "SELECT 2"]);
    let trigger = "CREATE TRIGGER tr AFTER INSERT ON t BEGIN UPDATE t SET x = 1; \
                   UPDATE t SET x = CASE WHEN x THEN 2 END; END;";
    assert_eq!(lite(&format!("{trigger}\nSELECT 1")), [trigger, "SELECT 1"]);
    let temp = "create temporary trigger tr after delete on t begin select 1; end;";
    assert_eq!(lite(&format!("{temp} select 2;")), [temp, "select 2;"]);
    let explain = "EXPLAIN CREATE TEMP TRIGGER tr AFTER DELETE ON t BEGIN SELECT 1; END;";
    assert_eq!(lite(explain), [explain]);
    assert_eq!(
        lite("SELECT ';' AS \";\", [a;b], `c;d` FROM t; SELECT 2"),
        ["SELECT ';' AS \";\", [a;b], `c;d` FROM t;", "SELECT 2"]
    );
    assert_eq!(
        lite("SELECT 1; /* ; */ -- ;\nSELECT 2;"),
        ["SELECT 1;", "/* ; */ -- ;\nSELECT 2;"]
    );
    // Empty statements belong to the statement after them, as SQLite reports its text.
    assert_eq!(lite("SELECT 1;; ;SELECT 2;"), ["SELECT 1;", "; ;SELECT 2;"]);
    assert_eq!(lite("SELECT 1; -- end\n;\n"), ["SELECT 1;"]);
    assert_eq!(
        lite("SELECT 1;\r\nSELECT 'it''s; ø';"),
        ["SELECT 1;", "SELECT 'it''s; ø';"]
    );
    // An unterminated quote: what came before keeps its ranges, the rest is one.
    assert_eq!(
        lite("SELECT 1; SELECT 'x; SELECT 3"),
        ["SELECT 1;", "SELECT 'x; SELECT 3"]
    );
    assert_eq!(lite("SELECT 1; /* open"), ["SELECT 1;"]);
    assert!(lite("").is_empty() && lite(" ; -- c").is_empty());
}

#[test]
fn ranges_ascend_without_overlap() {
    let sql = "SELECT 1; ;SELECT 2; -- c\nSELECT 3";
    for engine in [Engine::Postgres, Engine::Sqlite] {
        let r = statement_ranges(engine, sql);
        assert_eq!(r.len(), 3, "{engine:?}");
        assert!(r.windows(2).all(|w| w[0].end <= w[1].start), "{engine:?}");
        assert!(r.iter().all(|r| r.start < r.end && r.end <= sql.len()));
    }
}

/// The text of every statement `Script` prepares on `sql`, running each so the next prepares.
async fn prepared(sql: &str) -> Vec<String> {
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let mut out = Vec::new();
    {
        let mut script = s.script(sql);
        while let Some(next) = script.next().await {
            let pending = next.expect("every case prepares");
            out.push(pending.sql().to_owned());
            let mut stream = pending
                .execute(Limits::default())
                .await
                .expect("every case runs");
            while stream.next_batch().await.expect("streams").is_some() {}
        }
    }
    s.close().await.unwrap();
    out
}

const CASES: [&str; 4] = [
    "CREATE TABLE t(x); CREATE TRIGGER tr AFTER INSERT ON t BEGIN UPDATE t SET x = CASE \
     WHEN x > 0 THEN 1 END; DELETE FROM t WHERE x IS NULL; END; INSERT INTO t VALUES (1);",
    "-- lead\nSELECT ';' AS \";\";; ;SELECT 2 /* ; */;\r\nSELECT 'it''s; fine' -- tail",
    "CREATE TABLE u(y); CREATE TEMP TRIGGER tu AFTER DELETE ON u BEGIN SELECT 1; END; \
     DELETE FROM u; EXPLAIN SELECT 1;",
    "SELECT [x;y] FROM (SELECT 1 AS [x;y]); SELECT `a;b` FROM (SELECT 2 AS `a;b`)",
];

#[tokio::test]
async fn sqlite_ranges_are_the_statements_script_prepares() {
    let torture = include_str!("../../dbento-cli/tests/fixtures/sqlite_torture.sql");
    let chinook = include_str!("../../../datasets/chinook/Chinook_Sqlite.sql");
    for sql in CASES.into_iter().chain([torture, chinook]) {
        let head: String = sql.chars().take(60).collect();
        assert_eq!(lite(sql), prepared(sql).await, "{head}");
    }
}

fn my(sql: &str) -> Vec<&str> {
    texts(Engine::Mysql, sql)
}

#[test]
fn mysql_ranges_stop_before_the_delimiter_and_skip_quotes_and_comments() {
    assert_eq!(
        statement_ranges(Engine::Mysql, "SELECT 1; SELECT 2"),
        [0..8, 9..18]
    );
    assert_eq!(
        my("SELECT ';', \"a;b\", `c;d`; SELECT 'it''s; x', 'back\\'; y'"),
        [
            "SELECT ';', \"a;b\", `c;d`",
            "SELECT 'it''s; x', 'back\\'; y'"
        ]
    );
    assert_eq!(
        my("SELECT \"x\\\";y\"; SELECT `a\\`; SELECT 3"),
        ["SELECT \"x\\\";y\"", "SELECT `a\\`", "SELECT 3"]
    );
    assert_eq!(
        my("SELECT 1 # x; y\n; SELECT 2 -- z; w\n; SELECT 3 /* ; */"),
        ["SELECT 1 # x; y", "SELECT 2 -- z; w", "SELECT 3 /* ; */"]
    );
    assert_eq!(my("SELECT 1 --1; SELECT 2"), ["SELECT 1 --1", "SELECT 2"]);
    assert_eq!(my("SELECT 5 --\tx; y"), ["SELECT 5 --\tx; y"]);
    assert_eq!(
        my("/* c */ SELECT 1; /* note */; # only\n;"),
        ["/* c */ SELECT 1"]
    );
    assert_eq!(
        my("/*!40101 SET NAMES utf8mb4 */; /*! a; b */; SELECT 1"),
        ["/*!40101 SET NAMES utf8mb4 */", "/*! a; b */", "SELECT 1"]
    );
    assert_eq!(my("SELECT 'open; SELECT 2"), ["SELECT 'open; SELECT 2"]);
    assert_eq!(my("SELECT 1; /* open ; x"), ["SELECT 1"]);
    assert!(my("").is_empty() && my("  -- only\n ; ;").is_empty());
}

#[test]
fn mysql_delimiter_lines_switch_the_terminator_and_are_never_sent() {
    let script = "DELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END //\nDELIMITER ;\nCALL p();\nSELECT 3";
    assert_eq!(
        my(script),
        [
            "CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END",
            "CALL p()",
            "SELECT 3"
        ]
    );
    assert_eq!(
        my("delimiter $$\nSELECT '$$' $$\nDELIMITER ;"),
        ["SELECT '$$'"]
    );
    assert_eq!(
        my("SELECT 1, delimiter FROM t; SELECT 2"),
        ["SELECT 1, delimiter FROM t", "SELECT 2"]
    );
    assert!(my("DELIMITER //\n").is_empty());
}

#[test]
fn a_mysql_delimiter_takes_one_token_and_a_bare_one_is_never_sent() {
    let script =
        "DELIMITER // -- switch\nSELECT 1; SELECT 2 //\nDELIMITER ; # back\nSELECT 3; SELECT 4";
    assert_eq!(my(script), ["SELECT 1; SELECT 2", "SELECT 3", "SELECT 4"]);
    assert_eq!(
        my("SELECT 1;\nDELIMITER\nSELECT 2; SELECT 3"),
        ["SELECT 1", "SELECT 2", "SELECT 3"]
    );
    assert_eq!(my("SELECT 1;\ndelimiter"), ["SELECT 1"]);
    assert!(my("DELIMITER \t\n").is_empty());
}

fn ms(sql: &str) -> Vec<&str> {
    texts(Engine::Mssql, sql)
}

#[test]
fn sql_server_batches_end_at_go_lines_which_belong_to_none() {
    assert_eq!(
        statement_ranges(Engine::Mssql, "SELECT 1\nGO\nSELECT 2"),
        [0..9, 12..20]
    );
    assert_eq!(
        ms("SELECT 1\ngo 3 -- three times\nSELECT 2\r\n  Go\t\r\nSELECT 3"),
        ["SELECT 1", "SELECT 2", "SELECT 3"]
    );
    assert_eq!(
        ms("CREATE PROCEDURE p AS SELECT 1;\nGO\nEXEC p;\nGO--done\n"),
        ["CREATE PROCEDURE p AS SELECT 1;", "EXEC p;"]
    );
    assert_eq!(ms("SELECT 1 GO\nSELECT 2"), ["SELECT 1 GO\nSELECT 2"]);
    for not_go in [
        "GO;",
        "GO 0",
        "GO x",
        "GO 1x",
        "GO 99999999999",
        "GO /* c */",
        "GOTO done",
        "/* c */ GO",
    ] {
        let sql = format!("SELECT 1\n{not_go}\nSELECT 2");
        assert_eq!(ms(&sql), [sql.as_str()], "{not_go}");
    }
    assert!(ms("").is_empty() && ms("GO").is_empty() && ms("GO\n  go 2\n").is_empty());
    assert_eq!(
        ms("SELECT 1\nGO\n/* only a comment */\nGO\n-- note\n"),
        ["SELECT 1"]
    );
}

#[test]
fn go_inside_strings_identifiers_and_nested_comments_is_text() {
    for inner in [
        "SELECT '\nGO\n' AS a",
        "SELECT N'it''s\nGO\n' AS a",
        "SELECT [x\nGO\n]]y] AS a",
        "SELECT \"x\nGO\n\"\"y\" AS a",
        "SELECT 1 -- GO",
        "SELECT 1\n/* outer\n/* inner */\nGO\n*/",
    ] {
        assert_eq!(
            ms(&format!("{inner}\nGO\nSELECT 2")),
            [inner, "SELECT 2"],
            "{inner}"
        );
    }
}

#[test]
fn an_unterminated_string_or_comment_makes_the_rest_one_batch() {
    assert_eq!(
        ms("SELECT 1\nGO\nSELECT 'open\nGO\nSELECT 2"),
        ["SELECT 1", "SELECT 'open\nGO\nSELECT 2"]
    );
    assert_eq!(
        ms("SELECT 1\nGO\nSELECT 2 /* open /* x */\nGO\nSELECT 3"),
        ["SELECT 1", "SELECT 2 /* open /* x */\nGO\nSELECT 3"]
    );
    assert_eq!(
        ms("SELECT 1\n/* it's '/*' */\nGO\nSELECT 2"),
        ["SELECT 1\n/* it's '/*' */\nGO\nSELECT 2"]
    );
    assert_eq!(
        ms("SELECT [open\nGO\nSELECT 2"),
        ["SELECT [open\nGO\nSELECT 2"]
    );
    assert_eq!(ms("SELECT 1\nGO\n/* open\nGO\n"), ["SELECT 1"]);
}

#[test]
fn mongodb_ranges_split_at_semicolons_and_line_breaks() {
    let src = "use shop\ndb.t.find()\n  .sort({a: 1}); db.t.insertOne({s: ';'}) // c;\n";
    assert_eq!(
        texts(Engine::Mongodb, src),
        [
            "use shop",
            "db.t.find()\n  .sort({a: 1});",
            "db.t.insertOne({s: ';'})"
        ]
    );
}
