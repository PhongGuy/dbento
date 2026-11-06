mod support;

use dbento_core::{Limits, SessionOptions};
use support::{pg_fixture, run, sqlite_fixture};

#[tokio::test]
async fn a_postgres_session_is_marked_once_it_sends_set() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    assert!(!s.settings_changed());
    assert!(
        run(&mut s, "SELECT 1; SHOW DateStyle", Limits::default())
            .await
            .iter()
            .all(Result::is_ok)
    );
    assert!(!s.settings_changed());
    {
        // Prepared and skipped: never sent, so not marked.
        let mut script = s.script("SET DateStyle = 'SQL, DMY'");
        script.next().await.unwrap().unwrap().skip();
    }
    assert!(!s.settings_changed());
    let results = run(&mut s, "SET DateStyle = 'SQL, DMY'", Limits::default()).await;
    assert!(results[0].is_ok());
    assert!(s.settings_changed());
}

#[tokio::test]
async fn a_set_that_fails_still_marks_the_session() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let results = run(&mut s, "SET DateStyle = 'bogus'", Limits::default()).await;
    assert!(results[0].is_err());
    assert!(s.settings_changed());
}

#[tokio::test]
async fn a_search_path_change_marks_the_session() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let results = run(&mut s, "SET search_path TO public", Limits::default()).await;
    assert!(results[0].is_ok());
    assert!(s.settings_changed());
}

#[tokio::test]
async fn sqlite_sessions_are_never_marked() {
    let fx = sqlite_fixture("CREATE TABLE t(a);", "");
    let mut s = fx.session(SessionOptions::default()).await;
    assert!(
        run(
            &mut s,
            "PRAGMA cache_size = 100; SELECT 1",
            Limits::default()
        )
        .await
        .iter()
        .all(Result::is_ok)
    );
    assert!(!s.settings_changed());
}
