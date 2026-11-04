use dbento_app::results::ResultStore;
use dbento_app::runner::StatementEnd;
use dbento_app::status_bar::{statement_text, status_subject};
use dbento_app::tab_model::StatementResult;
use dbento_core::{ColumnInfo, ExecuteError};
use std::sync::Arc;

fn result(sql: &str) -> StatementResult {
    StatementResult {
        stmt: 0,
        sql: sql.into(),
        engine: None,
        epoch: 0,
        store: ResultStore::new(Arc::from(Vec::<ColumnInfo>::new())),
        elapsed: None,
        source: None,
        plan: None,
    }
}
#[test]
fn the_statement_segment_is_its_first_line() {
    assert_eq!(
        statement_text(&result("SELECT *\n  FROM Artist")),
        "SELECT *"
    );
    assert_eq!(
        statement_text(&result("\n   SELECT 1  \nFROM t")),
        "SELECT 1"
    );
}
#[test]
fn a_statement_that_failed_to_prepare_shows_its_error() {
    let mut r = result("");
    assert_eq!(statement_text(&r), "");
    r.store
        .finish(StatementEnd::Failed(ExecuteError::Cancelled));
    assert_eq!(statement_text(&r), "the statement was cancelled");
}
#[test]
fn the_chip_names_the_failed_statement_over_the_shown_one() {
    let ok = || result("SELECT 1");
    let mut bad = result("SELECT nope FROM t");
    bad.store
        .finish(StatementEnd::Failed(ExecuteError::Cancelled));
    assert_eq!(status_subject(&[ok(), bad], Some(0)), Some(1));
    assert_eq!(status_subject(&[ok(), ok()], Some(0)), Some(0));
    assert_eq!(status_subject(&[ok(), ok()], None), Some(1));
    assert_eq!(status_subject(&[], None), None);
}
