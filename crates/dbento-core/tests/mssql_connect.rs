//! Connecting to SQL Server with tls = "off": the server info, and USE or SET marking a session.
mod support;

use dbento_core::{Engine, Flavor, Limits, SessionOptions};
use support::{ms_off, run, text};

#[tokio::test]
async fn off_connects_to_the_stock_certificate_and_reports_the_server() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    assert_eq!(s.engine(), Engine::Mssql);
    let server = s.server().unwrap();
    assert_eq!(server.flavor, Flavor::SqlServer);
    assert!(server.version.0 >= 16, "{:?}", server.version);
    let ran = run(&mut s, "SELECT 1 AS one", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(ran.columns, vec![("one".to_owned(), "INT".to_owned())]);
    assert_eq!(ran.rows, vec![vec![text("1")]]);
    s.close().await.unwrap();
}

#[tokio::test]
async fn a_use_marks_the_session_as_having_changed_settings() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    run(&mut s, "SELECT 1 AS one", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert!(!s.settings_changed(), "a plain SELECT");
    run(&mut s, "USE master", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert!(s.settings_changed(), "USE");
    s.close().await.unwrap();
}

#[tokio::test]
async fn a_session_set_marks_the_session_but_a_variable_assignment_does_not() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    run(
        &mut s,
        "DECLARE @x INT; SET @x = 1; SELECT @x",
        Limits::default(),
    )
    .await
    .remove(0)
    .unwrap();
    assert!(!s.settings_changed(), "a variable assignment");
    run(&mut s, "SET LANGUAGE British", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert!(s.settings_changed(), "SET LANGUAGE");
    s.close().await.unwrap();
}

#[tokio::test]
async fn a_use_of_another_database_is_the_current_database() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    assert_eq!(s.current_database().await, None, "a fresh session");
    let ran = run(&mut s, "SELECT DB_NAME() AS d", Limits::default())
        .await
        .remove(0)
        .unwrap();
    let own = ran.rows[0][0].clone();
    let other = if own == text("tempdb") {
        "master"
    } else {
        "tempdb"
    };
    run(&mut s, &format!("USE {other}"), Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(s.current_database().await.as_deref(), Some(other));
    s.close().await.unwrap();
}
