//! Derived child connections in the schema tree model.
use std::sync::Arc;

use dbento_app::catalog::{CatalogData, CatalogQuery, CatalogReply};
use dbento_app::link::LinkState;
use dbento_app::schema_tree::{CatalogTree, LoadRequest, NodeKey, RevealStep, RowKind};
use dbento_core::{RelationRef, SchemaInfo, Workspace};

fn answer(t: &mut CatalogTree, req: LoadRequest, result: Result<CatalogData, String>) {
    t.apply(
        &req.conn,
        CatalogReply {
            generation: req.generation,
            query: req.query,
            result,
        },
    );
}

#[test]
fn a_databases_reply_is_cached_and_ensure_asks_once() {
    let mut t = CatalogTree::new();
    let req = t
        .ensure("srv", &CatalogQuery::Databases)
        .expect("absent, so it loads");
    assert!(t.ensure("srv", &CatalogQuery::Databases).is_none());
    assert_eq!(t.databases("srv"), None);
    answer(&mut t, req, Ok(CatalogData::Databases(vec!["app".into()])));
    assert_eq!(t.databases("srv"), Some(&["app".to_owned()][..]));
    let req = t.refresh_connection("srv");
    assert!(req.is_empty(), "nothing is expanded");
    assert_eq!(t.databases("srv"), None, "a refresh drops the list");
}

const PG: &str =
    "engine = \"postgres\"\nhost = \"127.0.0.1\"\nport = 9\ndatabase = \"x\"\nuser = \"x\"\n";

fn write(dir: &tempfile::TempDir, srv_extra: &str) {
    let toml = format!(
        "dbento = 1\n[connections.srv]\n{PG}{srv_extra}\n[connections.listed]\n{PG}databases = [\"app\", \"booking\"]\n"
    );
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
}
fn reopen(dir: &tempfile::TempDir, srv_extra: &str) -> Arc<Workspace> {
    write(dir, srv_extra);
    Arc::new(Workspace::open(dir.path()).unwrap())
}
fn tree(srv_extra: &str) -> (tempfile::TempDir, CatalogTree) {
    let dir = tempfile::tempdir().unwrap();
    let mut t = CatalogTree::new();
    t.set_workspace(reopen(&dir, srv_extra));
    (dir, t)
}
fn conn(name: &str) -> NodeKey {
    NodeKey::Connection(name.into())
}
fn dbs(names: &[&str]) -> CatalogData {
    CatalogData::Databases(names.iter().map(|n| (*n).to_owned()).collect())
}
fn shape(t: &CatalogTree, parent: &str, filter: &str) -> Vec<String> {
    t.rows(parent, 1, filter)
        .iter()
        .map(|r| format!("{}{}", "  ".repeat(r.depth - 1), r.aria))
        .collect()
}
/// `srv` is `"all"`, expanded and answered with `names`.
fn srv_listing(names: &[&str]) -> (tempfile::TempDir, CatalogTree) {
    let (dir, mut t) = tree("databases = \"all\"");
    let req = t.toggle(&conn("srv")).expect("a databases load");
    answer(&mut t, req, Ok(dbs(names)));
    (dir, t)
}
fn schemas(names: &[&str]) -> CatalogData {
    CatalogData::Schemas(names.iter().map(|n| SchemaInfo::new(*n)).collect())
}

#[test]
fn an_all_parent_lists_its_databases_as_child_rows() {
    let (_d, mut t) = tree("databases = \"all\"");
    let req = t.toggle(&conn("srv")).expect("a databases load");
    assert_eq!(
        (req.conn.as_str(), &req.query),
        ("srv", &CatalogQuery::Databases)
    );
    assert_eq!(shape(&t, "srv", ""), ["srv:loading"]);
    assert_eq!(t.link("srv"), LinkState::Connecting);
    answer(&mut t, req, Ok(dbs(&["app", "my.db"])));
    assert_eq!(shape(&t, "srv", ""), ["srv/app", "srv/my.db:invalid"]);
    assert_eq!(t.link("srv"), LinkState::Connected);
    let rows = t.rows("srv", 1, "");
    assert_eq!(rows[0].kind, RowKind::Database { invalid: None });
    assert_eq!(
        (rows[0].key.clone(), rows[0].label.as_str()),
        (conn("srv/app"), "app")
    );
    let RowKind::Database { invalid: Some(why) } = &rows[1].kind else {
        panic!("{:?}", rows[1].kind)
    };
    assert!(why.contains("[A-Za-z0-9_-]+"), "{why}");
    assert_eq!(rows[1].label, "my.db");
}

#[test]
fn a_child_nests_its_own_schemas_under_its_row() {
    let (_d, mut t) = srv_listing(&["app", "my.db"]);
    let req = t
        .toggle(&conn("srv/app"))
        .expect("the child's schemas load");
    assert_eq!(
        (req.conn.as_str(), &req.query),
        ("srv/app", &CatalogQuery::Schemas)
    );
    assert_eq!(
        shape(&t, "srv", ""),
        ["srv/app", "  srv/app:loading", "srv/my.db:invalid"]
    );
    answer(&mut t, req, Ok(schemas(&["public"])));
    assert_eq!(
        shape(&t, "srv", ""),
        ["srv/app", "  srv/app.public", "srv/my.db:invalid"]
    );
    assert!(t.rows("srv", 1, "")[0].expanded);
    assert_eq!(t.link("srv/app"), LinkState::Connected);
}

#[test]
fn an_invalid_database_never_expands_or_loads() {
    let (_d, mut t) = srv_listing(&["my.db"]);
    assert!(t.toggle(&conn("srv/my.db")).is_none());
    assert!(!t.is_expanded(&conn("srv/my.db")));
    assert_eq!(shape(&t, "srv", ""), ["srv/my.db:invalid"]);
}

#[test]
fn a_listed_parent_shows_its_list_without_a_load() {
    let (_d, mut t) = tree("");
    assert!(t.toggle(&conn("listed")).is_none());
    assert_eq!(shape(&t, "listed", ""), ["listed/app", "listed/booking"]);
    assert_eq!(t.link("listed"), LinkState::Idle);
    assert!(
        t.refresh().is_empty(),
        "a list in dbento.toml needs no server"
    );
}

#[test]
fn a_failed_or_empty_listing_shows_a_status_row() {
    let (_d, mut t) = tree("databases = \"all\"");
    let req = t.toggle(&conn("srv")).unwrap();
    answer(&mut t, req, Err("no answer within 30s".into()));
    let rows = t.rows("srv", 1, "");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].aria, "srv:error");
    assert!(matches!(&rows[0].kind, RowKind::Failed { message } if message.contains("30s")));
    assert!(matches!(t.link("srv"), LinkState::Failed(_)));
    assert!(t.toggle(&conn("srv")).is_none(), "collapsing loads nothing");
    let again = t
        .toggle(&conn("srv"))
        .expect("re-expanding a failed listing retries");
    assert_eq!(again.query, CatalogQuery::Databases);
    answer(&mut t, again, Ok(dbs(&[])));
    let rows = t.rows("srv", 1, "");
    assert_eq!(
        (rows[0].aria.as_str(), rows[0].label.as_str()),
        ("srv:empty", "no databases")
    );
    assert_eq!(rows[0].kind, RowKind::Empty);
}

#[test]
fn refresh_relists_and_reload_keeps_the_list() {
    let (_d, mut t) = srv_listing(&["app"]);
    let loads = t.reload();
    assert!(
        loads.iter().all(|r| r.query != CatalogQuery::Databases),
        "{loads:?}"
    );
    assert_eq!(shape(&t, "srv", ""), ["srv/app"]);
    let loads = t.refresh_connection("srv");
    assert_eq!(loads.len(), 1, "{loads:?}");
    assert_eq!(loads[0].query, CatalogQuery::Databases);
    answer(&mut t, loads.into_iter().next().unwrap(), Ok(dbs(&["app"])));
    let loads = t.refresh();
    assert!(
        loads
            .iter()
            .any(|r| r.conn == "srv" && r.query == CatalogQuery::Databases)
    );
    assert_eq!(shape(&t, "srv", ""), ["srv:loading"]);
}

#[test]
fn a_new_workspace_keeps_the_list_only_while_the_parent_is_unchanged() {
    let (dir, mut t) = srv_listing(&["app"]);
    let req = t.toggle(&conn("srv/app")).unwrap();
    answer(&mut t, req, Ok(schemas(&["public"])));
    t.set_workspace(reopen(&dir, "databases = \"all\""));
    assert_eq!(shape(&t, "srv", ""), ["srv/app", "  srv/app.public"]);
    t.set_workspace(reopen(&dir, "databases = \"all\"\nread_only = true"));
    assert_eq!(t.databases("srv"), None, "a changed session drops the list");
    t.set_workspace(reopen(&dir, ""));
    assert!(
        !t.is_expanded(&conn("srv/app")),
        "children of a former parent are forgotten"
    );
    assert!(t.schemas("srv/app").is_none());
}

#[test]
fn the_filter_keeps_a_database_whose_name_or_levels_match() {
    let (_d, mut t) = srv_listing(&["app", "booking", "my.db"]);
    let req = t.toggle(&conn("srv/booking")).unwrap();
    answer(&mut t, req, Ok(schemas(&["public", "audit"])));
    assert_eq!(
        shape(&t, "srv", "aud"),
        ["srv/booking", "  srv/booking.audit"]
    );
    assert_eq!(
        shape(&t, "srv", "book"),
        ["srv/booking", "  srv/booking.public", "  srv/booking.audit"]
    );
    assert_eq!(shape(&t, "srv", "my"), ["srv/my.db:invalid"]);
}

#[test]
fn retry_on_a_failed_listing_lists_again() {
    let (_d, mut t) = tree("databases = \"all\"");
    let req = t.toggle(&conn("srv")).unwrap();
    answer(&mut t, req, Err("refused".into()));
    let again = t.retry(&conn("srv")).expect("a failed listing retries");
    assert_eq!(again.query, CatalogQuery::Databases);
    assert_eq!(shape(&t, "srv", ""), ["srv:loading"]);
}

#[test]
fn a_parent_has_no_relations_to_reveal() {
    let (_d, mut t) = tree("databases = \"all\"");
    let rel = RelationRef::new("public", "t");
    assert_eq!(t.reveal("srv", &rel), RevealStep::Missing, "an all parent");
    assert_eq!(
        t.reveal("listed", &rel),
        RevealStep::Missing,
        "a listed parent"
    );
}

/// The queries of a reveal still waiting on its loads.
fn waiting_on(step: RevealStep) -> Vec<CatalogQuery> {
    match step {
        RevealStep::Waiting(loads) => loads.into_iter().map(|l| l.query).collect(),
        other @ (RevealStep::Shown | RevealStep::Missing | _) => {
            panic!("expected Waiting, got {other:?}")
        }
    }
}

#[test]
fn revealing_on_a_child_opens_its_collapsed_parent_first() {
    let (_d, mut t) = srv_listing(&["app"]);
    assert!(t.toggle(&conn("srv")).is_none(), "collapsing loads nothing");
    let rel = RelationRef::new("public", "t");
    assert_eq!(
        waiting_on(t.reveal("srv/app", &rel)),
        [CatalogQuery::Schemas]
    );
    assert!(t.is_expanded(&conn("srv")), "the parent opens");
    assert!(t.is_expanded(&conn("srv/app")), "then the child");
}

#[test]
fn a_child_reveal_waits_on_its_parents_listing_and_misses_an_unlisted_name() {
    let (_d, mut t) = tree("databases = \"all\"");
    let rel = RelationRef::new("public", "t");
    let req = crate::schema_tree::waiting(t.reveal("srv/app", &rel));
    assert_eq!(req.query, CatalogQuery::Databases);
    assert!(t.is_expanded(&conn("srv")));
    assert!(!t.is_expanded(&conn("srv/app")), "no hidden child session");
    answer(&mut t, req, Ok(dbs(&["other"])));
    assert_eq!(t.reveal("srv/app", &rel), RevealStep::Missing);
    assert!(
        !t.is_expanded(&conn("srv/app")),
        "an unlisted child stays shut"
    );
}

#[test]
fn a_child_reveal_under_a_failed_listing_is_missing() {
    let (_d, mut t) = tree("databases = \"all\"");
    let req = t.toggle(&conn("srv")).unwrap();
    answer(&mut t, req, Err("refused".into()));
    let rel = RelationRef::new("public", "t");
    assert_eq!(t.reveal("srv/app", &rel), RevealStep::Missing);
    assert!(!t.is_expanded(&conn("srv/app")));
}

#[test]
fn revealing_on_a_listed_child_opens_the_parent_without_a_load() {
    let (_d, mut t) = tree("");
    let rel = RelationRef::new("public", "t");
    assert_eq!(
        waiting_on(t.reveal("listed/app", &rel)),
        [CatalogQuery::Schemas]
    );
    assert!(t.is_expanded(&conn("listed")));
    assert_eq!(
        t.reveal("listed/nope", &rel),
        RevealStep::Missing,
        "not listed"
    );
}
