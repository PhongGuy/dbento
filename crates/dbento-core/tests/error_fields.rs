//! Database errors carry detail, hint and a byte position into the statement as sent.
mod support;
use dbento_core::{Engine, ExecuteError, Limits, SessionOptions, statement_ranges};
use support::{pg_fixture, run, sqlite_fixture};

/// Statement `n` of `script` as the app records it: its range, trimmed.
fn stmt(engine: Engine, script: &str, n: usize) -> String {
    script[statement_ranges(engine, script)[n].clone()]
        .trim()
        .to_owned()
}

fn fields(e: &ExecuteError) -> (String, Option<String>, Option<String>, Option<usize>) {
    match e {
        ExecuteError::Database {
            code,
            detail,
            hint,
            position,
            ..
        } => (code.clone(), detail.clone(), hint.clone(), *position),
        other => panic!("not a database error: {other:?}"),
    }
}

#[test]
fn the_constructor_keeps_display_and_leaves_the_rest_empty() {
    let e = ExecuteError::database("42P01", "relation \"x\" does not exist");
    assert_eq!(e.to_string(), "relation \"x\" does not exist (42P01)");
    assert_eq!(fields(&e), ("42P01".to_owned(), None, None, None));
}

#[tokio::test]
async fn postgres_positions_are_bytes_from_the_statement_start_comments_included() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let script = "SELECT 1;\n-- the café check\nSELECT 'é', nope_col FROM pg_class";
    let r = run(&mut s, script, Limits::default()).await;
    let (code, _, _, position) = fields(r[1].as_ref().err().expect("the second fails"));
    assert_eq!(code, "42703");
    let sql = stmt(Engine::Postgres, script, 1);
    let at = position.expect("a position");
    assert!(sql[at..].starts_with("nope_col"), "{at} in {sql:?}");
}

#[tokio::test]
async fn postgres_keeps_detail_and_hint() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let r = run(
        &mut s,
        "CREATE TABLE u(a int PRIMARY KEY); INSERT INTO u VALUES (1); INSERT INTO u VALUES (1)",
        Limits::default(),
    )
    .await;
    let (code, detail, _, position) = fields(r[2].as_ref().err().unwrap());
    assert_eq!(code, "23505");
    assert_eq!(detail.as_deref(), Some("Key (a)=(1) already exists."));
    assert_eq!(position, None);
    let r = run(&mut s, "SELECT lower(1)", Limits::default()).await;
    let (code, _, hint, position) = fields(r[0].as_ref().err().unwrap());
    assert_eq!(code, "42883");
    assert!(hint.is_some_and(|h| h.contains("explicit type casts")));
    assert_eq!(position, Some("SELECT ".len()));
}

#[tokio::test]
async fn sqlite_positions_count_from_the_failing_statement_after_a_gap() {
    let fx = sqlite_fixture("CREATE TABLE t(a);", "");
    let mut s = fx.session(SessionOptions::default()).await;
    for script in [
        "SELECT 1;\n  SELECT nope_col FROM t",
        "SELECT 1;;\n-- why\nSELECT nope_col FROM t",
    ] {
        let r = run(&mut s, script, Limits::default()).await;
        let err = r.last().unwrap().as_ref().err().expect("the second fails");
        let (code, _, _, position) = fields(err);
        assert_eq!(code, "1", "{script:?}");
        let sql = stmt(Engine::Sqlite, script, 1);
        let at = position.expect("a position");
        assert!(sql[at..].starts_with("nope_col"), "{at} in {sql:?}");
    }
}
