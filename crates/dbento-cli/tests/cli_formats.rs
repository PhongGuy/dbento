mod common;
use common::{MANY, dbento, query_json, sqlite_ws};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn query(ws: &tempfile::TempDir, args: &[&str]) -> (i32, String, String) {
    let out = dbento()
        .current_dir(ws.path())
        .arg("query")
        .arg("lite")
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn table_is_the_default_and_aligns_columns() {
    let ws = sqlite_ws("");
    let (code, out, err) = query(
        &ws,
        &["select 1 as id, 'hello' as note union all select 2, null"],
    );
    assert_eq!(code, 0);
    assert_eq!(out, "id | note\n---+------\n1  | hello\n2  |\n");
    assert!(err.contains("(2 rows, "), "{err}");
}

#[test]
fn table_escapes_control_characters_and_separates_results() {
    let ws = sqlite_ws("CREATE TABLE t(a)");
    let (code, out, err) = query(
        &ws,
        &["select 'a' || char(10) || 'b' as v; insert into t values (1); select x'00ff' as b"],
    );
    assert_eq!(code, 0);
    assert_eq!(out, "v\n----\na\\nb\n\nb\n------\n\\x00ff\n");
    assert!(err.contains("OK, 1 row affected"), "{err}");
}

#[test]
fn table_reports_ddl_without_a_row_count() {
    let ws = sqlite_ws("CREATE TABLE t(a)");
    let (code, _, err) = query(&ws, &["create table u(a)"]);
    assert_eq!(code, 0);
    assert!(err.starts_with("OK ("), "{err}");
    assert!(!err.contains("affected"), "{err}");
}

#[test]
fn table_escapes_terminal_escape_sequences_from_untrusted_cells() {
    let ws = sqlite_ws("");
    let (code, out, _) = query(
        &ws,
        &["select char(27) || '[31mred' || char(127) || char(7)"],
    );
    assert_eq!(code, 0);
    assert!(!out.contains('\x1b') && !out.contains('\x7f') && !out.contains('\x07'));
    assert!(out.contains("\\x1b[31mred\\x7f\\x07"), "{out}");
}

#[test]
fn table_escapes_column_names_and_measures_the_escaped_text() {
    let ws = sqlite_ws("");
    let (code, out, _) = query(&ws, &["select 'x' as \"\x1b[31m\""]);
    assert_eq!(code, 0);
    assert_eq!(out, "\\x1b[31m\n--------\nx\n");
    // The escaped ESC is four visible columns wide, so a one-letter header widens to fit it.
    let (code, out, _) = query(&ws, &["select char(27) as c, 'x' as d"]);
    assert_eq!(code, 0);
    assert_eq!(out, "c    | d\n-----+--\n\\x1b | x\n");
}

#[test]
fn table_stops_at_1000_rows_by_default_while_json_and_csv_do_not() {
    let ws = sqlite_ws("");
    let (_, out, err) = query(&ws, &[MANY]);
    assert_eq!(out.lines().count(), 1002);
    assert!(
        err.contains("(1000 rows, showing the first 1000 rows; pass --max-rows to change"),
        "{err}"
    );
    let (_, out, err) = query(&ws, &[MANY, "--max-rows", "3"]);
    assert_eq!(out.lines().count(), 5);
    assert!(err.contains("(3 rows, truncated by --max-rows"), "{err}");
    let (_, doc) = query_json(ws.path(), MANY, &[]);
    assert_eq!(doc["statements"][0]["row_count"], 5000);
    // A SQL row carries no document, so it stays a plain array of cells.
    assert!(doc["statements"][0]["rows"][0].is_array(), "{doc}");
    let (_, out, _) = query(&ws, &[MANY, "--format", "csv"]);
    assert_eq!(out.lines().count(), 5001);
}

#[test]
fn csv_quotes_like_postgres_and_separates_result_sets() {
    let ws = sqlite_ws("");
    let sql = "select 'a,b' as x, 'say \"hi\"' as y, '' as z, null as w, x'00ff' as b; select 'l1' || char(10) || 'l2' as m";
    let (code, out, _) = query(&ws, &[sql, "--format", "csv"]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "x,y,z,w,b\n\"a,b\",\"say \"\"hi\"\"\",\"\",,\\x00ff\n\nm\n\"l1\nl2\"\n"
    );
}

#[test]
fn errors_and_refusals_go_to_stderr_in_text_formats() {
    let ws = sqlite_ws("CREATE TABLE t(a)");
    let (code, out, err) = query(&ws, &["select * from nope", "--format", "csv"]);
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(
        err.contains("error: statement 1: no such table: nope (1)"),
        "{err}"
    );
    let (code, _, err) = query(&ws, &["select 1; delete from t"]);
    assert_eq!(code, 2);
    assert!(
        err.contains("refused: statement 2: DELETE without WHERE; pass --yes to run it"),
        "{err}"
    );
}

#[test]
fn a_closed_stdout_ends_the_run_with_exit_1() {
    let ws = sqlite_ws("");
    let sql = "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 5000000) SELECT x FROM r";
    let mut child = Command::new(assert_cmd::cargo::cargo_bin("dbento"))
        .current_dir(ws.path())
        .args(["query", "lite", sql, "--format", "csv"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    assert_eq!(first, "x\n");
    let started = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "dbento kept running after stdout closed"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(1));
    let mut err = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut err).unwrap();
    assert!(!err.contains("panicked"), "{err}");
}
