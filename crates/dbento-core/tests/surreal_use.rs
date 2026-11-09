//! `USE` as a unit: a pre-check, then RPC `use`, and the refusals around it.
use crate::support;

use crate::support::{run, sr_session, text};
use dbento_core::{ExecuteError, Limits, SessionOptions};
use dbento_testkit::surreal_db;

const RO: Option<(&str, &str)> = Some(("ro", "root"));

#[tokio::test]
async fn a_use_unit_yields_one_empty_result_and_moves_the_session() {
    let db = surreal_db().await;
    db.admin("DEFINE DATABASE other; DEFINE NAMESPACE elsewhere")
        .await;
    let mut s = sr_session(&db, RO, "", SessionOptions::default()).await;
    let ran = run(
        &mut s,
        "USE DB other; RETURN session::db();",
        Limits::default(),
    )
    .await;
    assert_eq!(ran.len(), 2);
    let used = ran[0].as_ref().unwrap();
    assert!(used.columns.is_empty());
    assert_eq!(used.outcome.rows, 0);
    assert_eq!(ran[1].as_ref().unwrap().rows, [vec![text("other")]]);
    // A later script stays there: the `use` was an RPC, not text.
    let ran = run(&mut s, "RETURN session::db()", Limits::default()).await;
    assert_eq!(ran[0].as_ref().unwrap().rows, [vec![text("other")]]);
}

#[tokio::test]
async fn use_ns_alone_keeps_the_database_name_and_checks_only_the_namespace() {
    let db = surreal_db().await;
    db.admin("DEFINE NAMESPACE elsewhere; USE NS elsewhere; DEFINE DATABASE fresh")
        .await;
    let mut s = sr_session(&db, RO, "", SessionOptions::default()).await;
    let ran = run(
        &mut s,
        "USE NS elsewhere; USE DB fresh; RETURN [session::ns(), session::db()]",
        Limits::default(),
    )
    .await;
    let rows = &ran[2].as_ref().unwrap().rows;
    assert_eq!(rows, &[vec![text("elsewhere")], vec![text("fresh")]]);
}

#[tokio::test]
async fn a_missing_target_stops_the_script_and_creates_nothing() {
    let db = surreal_db().await;
    let before = support::sr_targets(&db).await;
    let mut s = sr_session(&db, RO, "", SessionOptions::default()).await;
    for (text_of_use, name) in [
        ("USE DB nosuchdb; CREATE leak:1;", "nosuchdb"),
        ("USE NS nosuchns; CREATE leak:1;", "nosuchns"),
        ("USE NS nosuchns DB nosuchdb; CREATE leak:1;", "nosuchns"),
    ] {
        let ran = run(&mut s, text_of_use, Limits::default()).await;
        assert_eq!(ran.len(), 1, "{text_of_use}");
        let Err(ExecuteError::Database { message, .. }) = &ran[0] else {
            panic!("{:?}", ran[0].as_ref().err());
        };
        assert!(message.contains(name), "{message}");
    }
    assert_eq!(db.count("leak").await, 0);
    assert_eq!(support::sr_targets(&db).await, before);
}

#[tokio::test]
async fn a_namespace_login_checks_its_database_and_a_database_login_is_refused_by_the_server() {
    let db = surreal_db().await;
    db.admin("DEFINE DATABASE other").await;
    let mut s = sr_session(
        &db,
        Some(("ne", "namespace")),
        "",
        SessionOptions::default(),
    )
    .await;
    let ran = run(
        &mut s,
        "USE DB other; RETURN session::db()",
        Limits::default(),
    )
    .await;
    assert_eq!(ran[1].as_ref().unwrap().rows, [vec![text("other")]]);
    let ran = run(&mut s, "USE DB nosuchdb", Limits::default()).await;
    assert!(ran[0].is_err());
    let mut s = sr_session(&db, Some(("de", "database")), "", SessionOptions::default()).await;
    let ran = run(&mut s, "USE DB other; CREATE leak:1", Limits::default()).await;
    assert_eq!(ran.len(), 1, "a failed USE stops the script");
    assert!(ran[0].is_err());
    assert_eq!(db.count("leak").await, 0);
}

#[tokio::test]
async fn any_other_use_is_refused_before_sending() {
    let db = surreal_db().await;
    let mut s = sr_session(&db, RO, "", SessionOptions::default()).await;
    for bad in [
        "LET $a = 1; USE DB app; CREATE leak:1",
        "BEGIN; USE DB app; CREATE leak:1; COMMIT;",
        "{ USE DB app; CREATE leak:1; }",
        "USE DB $name; CREATE leak:1",
    ] {
        let ran = run(&mut s, bad, Limits::default()).await;
        assert_eq!(ran.len(), 1, "{bad}");
        let Err(ExecuteError::Unsupported { reason }) = &ran[0] else {
            panic!("{bad}: {:?}", ran[0].as_ref().err());
        };
        assert!(reason.contains("USE"), "{reason}");
    }
    assert_eq!(db.count("leak").await, 0);
}

// What `execute` refuses, `Pending` still classifies, for the MCP `execute` tool.
#[tokio::test]
async fn prepare_never_refuses() {
    let db = surreal_db().await;
    let mut s = sr_session(&db, RO, "read_only = true", SessionOptions::default()).await;
    let mut script = s.script("LET $a = 1; USE DB app; CREATE leak:1");
    let pending = script.next().await.unwrap().unwrap();
    assert_eq!(pending.classification().writes, dbento_core::Writes::Yes);
}

// Only the user's own `USE` marks the session; a catalog call into another database does not.
#[tokio::test]
async fn a_use_marks_the_session_but_a_plain_statement_or_catalog_call_does_not() {
    let db = surreal_db().await;
    db.admin("DEFINE DATABASE other; DEFINE NAMESPACE elsewhere; USE NS elsewhere; DEFINE DATABASE fresh")
        .await;
    let mut s = sr_session(&db, RO, "", SessionOptions::default()).await;
    assert!(!s.settings_changed());
    run(&mut s, "RETURN 1", Limits::default())
        .await
        .remove(0)
        .unwrap();
    s.relations("other").await.unwrap();
    assert!(!s.settings_changed());
    run(&mut s, "USE NS elsewhere DB fresh", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert!(s.settings_changed());
}

#[tokio::test]
async fn a_use_names_the_namespace_and_database_the_session_is_in() {
    let db = surreal_db().await;
    db.admin("DEFINE NAMESPACE elsewhere; USE NS elsewhere; DEFINE DATABASE fresh")
        .await;
    let mut s = sr_session(&db, RO, "", SessionOptions::default()).await;
    assert_eq!(s.current_database().await, None, "a fresh session");
    let ran = run(
        &mut s,
        "RETURN session::ns(); RETURN session::db()",
        Limits::default(),
    )
    .await;
    let own: Vec<_> = ran
        .into_iter()
        .map(|r| r.unwrap().rows[0][0].clone())
        .collect();
    run(&mut s, "USE NS elsewhere DB fresh", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(
        s.current_database().await.as_deref(),
        Some("elsewhere/fresh")
    );
    let [support::Cell::Text(ns), support::Cell::Text(dbn)] = &own[..] else {
        panic!("{own:?}")
    };
    run(&mut s, &format!("USE NS {ns} DB {dbn}"), Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(s.current_database().await, None, "back where it opened");
}
