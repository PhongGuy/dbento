//! Results from units on a real server: one script result per statement index, frames ruling
//! over the lexer, and the three kinds of statement failure.
use crate::support;

use crate::support::{Cell, run, sr_root, text};
use dbento_core::{ExecuteError, Limits, SessionOptions, Writes};
use dbento_testkit::surreal_db;

fn null() -> Cell {
    Cell::Null
}

#[tokio::test]
async fn select_create_and_return_give_columns_and_rows() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let ran = run(
        &mut s,
        "CREATE p:1 SET name = 'ann', age = 30; SELECT age, id, name FROM p; RETURN 1 + 1;",
        Limits::default(),
    )
    .await;
    assert_eq!(ran.len(), 3);
    let created = ran[0].as_ref().unwrap();
    assert_eq!(created.columns[0].0, "id");
    assert_eq!(created.rows.len(), 1);
    assert_eq!(created.outcome.rows_affected, None);
    let selected = ran[1].as_ref().unwrap();
    let names: Vec<_> = selected.columns.iter().map(|c| c.0.as_str()).collect();
    assert_eq!(names, ["id", "age", "name"]);
    assert_eq!(
        selected.rows,
        [vec![text("p:1"), text("30"), text("ann")]],
        "id first, then the wire's sorted order"
    );
    assert_eq!(selected.columns[1].1, "int");
    let value = ran[2].as_ref().unwrap();
    assert_eq!(value.columns, [("value".to_owned(), "int".to_owned())]);
    assert_eq!(value.rows, [vec![text("2")]]);
    s.close().await.unwrap();
}

#[tokio::test]
async fn a_multi_statement_let_unit_keeps_its_value_and_gives_one_result_per_statement() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let ran = run(&mut s, "LET $a = 41; RETURN $a + 1", Limits::default()).await;
    assert_eq!(ran.len(), 2, "a LET unit is two statements, so two results");
    let first = ran[0].as_ref().unwrap();
    assert!(
        first.columns.is_empty(),
        "a LET has neither rows nor a value"
    );
    assert_eq!(first.sql, "LET $a = 41; RETURN $a + 1");
    let second = ran[1].as_ref().unwrap();
    assert_eq!(second.rows, [vec![text("42")]]);
    assert_eq!(second.sql, "LET $a = 41; RETURN $a + 1");
}

#[tokio::test]
async fn a_unit_confirms_once_and_is_last_only_on_a_lone_last_statement() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let mut script = s.script("LET $a = 1; DELETE t; RETURN 1");
    let mut first = script.next().await.unwrap().unwrap();
    assert_eq!(first.classification().writes, Writes::Yes);
    assert!(!first.classification().unguarded.is_empty());
    assert!(!first.is_last().await);
    let _ = first.execute(Limits::default()).await.unwrap();
    let mut second = script.next().await.unwrap().unwrap();
    assert_eq!(second.classification().writes, Writes::Yes);
    assert!(second.classification().unguarded.is_empty());
    assert!(!second.classification().transaction_control);
    assert!(!second.is_last().await);
    let _ = second.execute(Limits::default()).await;
    drop(script);
    let mut script = s.script("RETURN 1; RETURN 2");
    let mut first = script.next().await.unwrap().unwrap();
    assert!(!first.is_last().await);
    first.skip();
    let mut second = script.next().await.unwrap().unwrap();
    assert!(second.is_last().await);
    second.skip();
    assert!(script.next().await.is_none());
}

#[tokio::test]
async fn skipping_the_first_statement_skips_the_unit_and_skipping_a_later_one_drains_it() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let mut script = s.script("LET $a = 1; CREATE sk:1; CREATE sk:2;");
    script.next().await.unwrap().unwrap().skip();
    assert!(
        script.next().await.is_none(),
        "the unit was the whole script"
    );
    drop(script);
    assert_eq!(db.count("sk").await, 0, "nothing was sent");
    let mut script = s.script("LET $a = 1; CREATE sk:1; CREATE sk:2; RETURN 9");
    let first = script.next().await.unwrap().unwrap();
    let _ = support::collect(first, Limits::default()).await.unwrap();
    script.next().await.unwrap().unwrap().skip();
    script.next().await.unwrap().unwrap().skip();
    let last = script.next().await.unwrap().unwrap();
    let last = support::collect(last, Limits::default()).await.unwrap();
    assert_eq!(last.rows, [vec![text("9")]]);
    drop(script);
    s.settle().await.unwrap();
    assert_eq!(db.count("sk").await, 2, "skipped statements still ran");
}

#[tokio::test]
async fn a_dropped_script_leaves_the_session_clean() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    {
        let mut script = s.script("LET $a = 1; RETURN $a; RETURN 2; RETURN 3");
        let first = script.next().await.unwrap().unwrap();
        let _ = support::collect(first, Limits::default()).await.unwrap();
    }
    let ran = run(&mut s, "RETURN 'after'", Limits::default()).await;
    assert_eq!(ran[0].as_ref().unwrap().rows, [vec![text("after")]]);
}

#[tokio::test]
async fn rows_of_different_shapes_get_a_union_and_later_fields_a_trailing_column() {
    let db = surreal_db().await;
    db.admin("CREATE w:1 SET a = 1; CREATE w:2 SET b = 'x'; CREATE w:3 SET a = 'y'")
        .await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let ran = run(&mut s, "SELECT * FROM w", Limits::default()).await;
    let ran = ran[0].as_ref().unwrap();
    assert_eq!(
        ran.columns,
        [
            ("id".to_owned(), "record".to_owned()),
            ("a".to_owned(), "mixed".to_owned()),
            ("b".to_owned(), "string".to_owned())
        ],
        "a finished stream needs no trailing column"
    );
    assert_eq!(ran.rows[1], [text("w:2"), null(), text("x")]);
    // Row 301 is past every frame the sample can hold, so its field cannot be in the header.
    db.admin(
        "FOR $i IN 1..=300 { CREATE type::record('m', $i) SET a = $i }; CREATE m:301 SET late = 1",
    )
    .await;
    let ran = run(&mut s, "SELECT * FROM m ORDER BY id", Limits::default()).await;
    let ran = ran[0].as_ref().unwrap();
    let names: Vec<_> = ran.columns.iter().map(|c| c.0.as_str()).collect();
    assert_eq!(names, ["id", "a", "(more fields)"]);
    assert_eq!(ran.rows.len(), 301);
    assert_eq!(ran.rows[0][2], null());
    assert_eq!(ran.rows[300][2], text("{late: 1}"));
}

#[tokio::test]
async fn a_value_or_a_row_that_is_not_an_object_is_one_value_column() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let ran = run(
        &mut s,
        "RETURN [1, 'a']; RETURN {a: 1}; RETURN NONE; RETURN [];",
        Limits::default(),
    )
    .await;
    let list = ran[0].as_ref().unwrap();
    assert_eq!(list.columns, [("value".to_owned(), "mixed".to_owned())]);
    assert_eq!(list.rows, [vec![text("1")], vec![text("a")]]);
    let object = ran[1].as_ref().unwrap();
    assert_eq!(object.columns, [("value".to_owned(), "object".to_owned())]);
    assert_eq!(object.rows, [vec![text("{a: 1}")]]);
    assert!(ran[2].as_ref().unwrap().columns.is_empty());
    assert!(ran[3].as_ref().unwrap().columns.is_empty());
}

#[tokio::test]
async fn delete_and_a_write_returning_nothing_have_no_columns_and_no_count() {
    let db = surreal_db().await;
    db.admin("CREATE d:1; CREATE d:2").await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let ran = run(
        &mut s,
        "DELETE d:1; UPDATE d SET x = 1 RETURN NONE;",
        Limits::default(),
    )
    .await;
    for one in &ran {
        let one = one.as_ref().unwrap();
        assert!(one.columns.is_empty());
        assert_eq!(one.outcome.rows, 0);
        assert_eq!(one.outcome.rows_affected, None);
    }
}

#[tokio::test]
async fn a_statement_error_ends_the_script_and_says_the_later_statements_ran() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    db.admin("CREATE u:1").await;
    let ran = run(
        &mut s,
        "LET $x = 1; CREATE u:1; CREATE u:2; RETURN 'stops here'; RETURN 'next unit never runs'",
        Limits::default(),
    )
    .await;
    assert_eq!(ran.len(), 2, "the LET's result, then the error");
    let Err(ExecuteError::Database { message, .. }) = &ran[1] else {
        panic!("{:?}", ran[1].as_ref().err());
    };
    assert!(message.contains("already exists"), "{message}");
    assert!(
        message.contains("later statements of this unit ran"),
        "{message}"
    );
    assert_eq!(db.count("u").await, 2, "CREATE u:2 did run");
}

// A failed LET leaves $k as NONE, and its DELETE still runs in the same call and matches.
#[tokio::test]
async fn a_failed_let_does_not_stop_its_unit_and_the_message_says_so() {
    let db = surreal_db().await;
    db.admin("CREATE k:1 SET a = NONE; CREATE k:2 SET a = 2")
        .await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let ran = run(
        &mut s,
        "LET $k = (<int>'not a number'); DELETE k WHERE a = $k;",
        Limits::default(),
    )
    .await;
    assert_eq!(ran.len(), 1);
    let Err(ExecuteError::Database { message, .. }) = &ran[0] else {
        panic!("{:?}", ran[0].as_ref().err());
    };
    assert!(
        message.contains("later statements of this unit ran"),
        "{message}"
    );
    assert_eq!(db.count("k").await, 1, "the row with no `a` was removed");
}

#[tokio::test]
async fn a_failed_text_transaction_shows_undone_results_then_the_real_error() {
    let db = surreal_db().await;
    db.admin("CREATE z:1").await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let ran = run(
        &mut s,
        "BEGIN; CREATE y:1; CREATE z:1; CREATE y:2; COMMIT;",
        Limits::default(),
    )
    .await;
    assert_eq!(ran.len(), 3, "BEGIN, the undone CREATE, then the error");
    assert!(!ran[0].as_ref().unwrap().outcome.rolled_back);
    let undone = ran[1].as_ref().unwrap();
    assert!(undone.columns.is_empty());
    assert!(undone.outcome.rolled_back);
    let Err(ExecuteError::Database { message, .. }) = &ran[2] else {
        panic!("{:?}", ran[2].as_ref().err());
    };
    assert!(message.contains("already exists"), "{message}");
    assert!(
        !message.contains("later statements"),
        "a transaction: {message}"
    );
    assert_eq!(db.count("y").await, 0);
}

#[tokio::test]
async fn a_cancelled_text_transaction_ends_with_no_error() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let ran = run(&mut s, "BEGIN; CREATE c:1; CANCEL;", Limits::default()).await;
    assert_eq!(ran.len(), 3);
    assert!(ran[1].as_ref().unwrap().outcome.rolled_back);
    assert!(ran.iter().all(Result::is_ok));
    assert_eq!(db.count("c").await, 0);
}

#[tokio::test]
async fn a_transaction_without_commit_reports_the_server_error_and_writes_nothing() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let ran = run(&mut s, "BEGIN; CREATE b:1", Limits::default()).await;
    let last = ran.last().unwrap();
    let Err(ExecuteError::Database { message, .. }) = last else {
        panic!("{:?}", last.as_ref().err());
    };
    assert!(message.contains("Missing COMMIT"), "{message}");
    assert_eq!(db.count("b").await, 0);
}

#[tokio::test]
async fn a_comment_only_script_yields_nothing_and_a_parse_error_runs_nothing() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    assert!(
        run(&mut s, "-- nothing;\n", Limits::default())
            .await
            .is_empty()
    );
    let ran = run(
        &mut s,
        "LET $a = 1; CREATE pe:1; CREATE (((",
        Limits::default(),
    )
    .await;
    let Err(ExecuteError::Database {
        message, detail, ..
    }) = &ran[0]
    else {
        panic!("{:?}", ran[0].as_ref().err());
    };
    assert!(message.contains("-->"), "{message}");
    assert_eq!(detail.as_deref(), Some("Validation"));
    assert_eq!(db.count("pe").await, 0);
}

// The lexer reads a regex from `/` to `/` here, one statement; the server reads two divides and
// three statements. The frames win: the extras run, drain and are counted.
#[tokio::test]
async fn statements_the_lexer_did_not_see_drain_and_count_in_extra_results() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let ran = run(
        &mut s,
        "RETURN ->? / 2; CREATE ex:1; RETURN 7 / 2",
        Limits::default(),
    )
    .await;
    assert_eq!(
        ran.len(),
        1,
        "one result for the one statement the lexer saw"
    );
    assert_eq!(ran[0].as_ref().unwrap().outcome.extra_results, 2);
    assert_eq!(db.count("ex").await, 1);
}
