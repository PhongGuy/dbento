//! SurrealDB through the app's runner: the session's read-only level reaches the tab, a fenced
//! result says its changes were rolled back, and the catalog lists no schema objects.
mod support;
use std::sync::Arc;

use dbento_app::catalog::{CatalogData, CatalogQuery, spawn_catalog};
use dbento_app::results_pane::outcome_note;
use dbento_app::runner::{Event, RunnerSpec, StatementEnd, spawn_on};
use dbento_core::{Outcome, ReadOnlyEnforcement, Workspace};
use dbento_testkit::{SrDb, surreal_db};
use support::{here, next_event, run_cmd, until_event};

const SR_SECRET_VAR: &str = "DBENTO_TEST_SR_SECRET";

/// A runner spec on `db` as connection `sr` for `login`, its password from the variable.
fn sr_spec(db: &SrDb, login: (&str, &str), extra: &str) -> (tempfile::TempDir, RunnerSpec) {
    let dir = tempfile::tempdir().unwrap();
    let extra = format!("secret = {{ env = \"{SR_SECRET_VAR}\" }}\n{extra}");
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!(
            "dbento = 1\n{}",
            db.connection_toml("sr", Some(login), &extra)
        ),
    )
    .unwrap();
    let password = db.password().to_owned();
    let spec = RunnerSpec {
        workspace: Arc::new(Workspace::open(dir.path()).unwrap()),
        connection: "sr".into(),
        store: Arc::new(dbento_core::MemoryStore::new()),
        env: Arc::new(move |v| (v == SR_SECRET_VAR).then(|| password.clone())),
    };
    (dir, spec)
}

async fn connected(spec: RunnerSpec) -> (bool, Option<ReadOnlyEnforcement>) {
    let r = spawn_on(&here(), spec);
    let ev = r.events();
    r.send(run_cmd(1, "RETURN 1", Some(10)));
    match next_event(&ev).await {
        Event::Connected {
            read_only, level, ..
        } => (read_only, level),
        other => panic!("{other:?}"),
    }
}

/// Runs `sql` to its end and returns the first completed statement's outcome and the rows.
async fn outcome_of(spec: RunnerSpec, sql: &str) -> (Outcome, usize) {
    let r = spawn_on(&here(), spec);
    let ev = r.events();
    r.send(run_cmd(1, sql, Some(10)));
    let (seen, rows) = until_event(&ev, |e| matches!(e, Event::RunDone { .. })).await;
    let outcome = seen
        .iter()
        .find_map(|e| match e {
            Event::StatementDone {
                end: StatementEnd::Complete(o),
                ..
            } => Some(o.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no completed statement in {seen:?}"));
    (outcome, rows)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_session_s_read_only_level_reaches_the_tab() {
    let db = surreal_db().await;
    let (_a, viewer) = sr_spec(&db, ("rv", "root"), "");
    assert_eq!(
        connected(viewer).await,
        (true, Some(ReadOnlyEnforcement::Database)),
        "a VIEWER login is read-only without read_only"
    );
    let (_b, owner) = sr_spec(&db, ("ro", "root"), "read_only = true");
    assert_eq!(
        connected(owner).await,
        (true, Some(ReadOnlyEnforcement::BestEffort))
    );
    let (_c, writer) = sr_spec(&db, ("ro", "root"), "");
    assert_eq!(connected(writer).await, (false, None));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fenced_result_notes_that_its_changes_were_rolled_back() {
    let db = surreal_db().await;
    db.admin("DEFINE FUNCTION fn::w() { CREATE fnt:1 SET x = 1; RETURN 1; };")
        .await;
    let (_a, viewer) = sr_spec(&db, ("dv", "database"), "");
    let (o, rows) = outcome_of(viewer, "RETURN fn::w()").await;
    assert_eq!(rows, 1);
    assert!(o.rolled_back);
    assert_eq!(
        outcome_note(&o).as_deref(),
        Some("any changes were rolled back")
    );
    assert_eq!(db.count("fnt").await, 0, "the fence undid the write");

    let (_b, writer) = sr_spec(&db, ("ro", "root"), "");
    let (o, _) = outcome_of(writer, "CREATE w:1 SET x = 1").await;
    assert!(!o.rolled_back);
    assert_eq!(outcome_note(&o), None);
    assert_eq!(db.count("w").await, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_engine_without_schema_objects_lists_none_instead_of_failing() {
    let db = surreal_db().await;
    let (_a, spec) = sr_spec(&db, ("ro", "root"), "");
    let r = spawn_catalog(&here(), spec);
    let rx = r.replies();
    r.load(
        1,
        CatalogQuery::SchemaObjects {
            schema: db.database().into(),
        },
    );
    let got = tokio::time::timeout(std::time::Duration::from_secs(20), rx.recv())
        .await
        .expect("a reply within 20 s")
        .expect("the runner is alive");
    match got.result {
        Ok(CatalogData::SchemaObjects(s)) => {
            assert!(s.types().is_empty() && s.routines().is_empty() && !s.truncated())
        }
        other => panic!("{other:?}"),
    }
}
