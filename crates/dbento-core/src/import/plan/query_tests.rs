//! Placement of saved queries by `plan`, and their writing by `apply`.
use super::tests::{pg, set_of, ws_with};
use super::*;
use crate::Workspace;
use crate::import::model::{ImportedQuery, WarningKind};
use std::path::PathBuf;

const ORDERS: &str = "dbento = 1\n[connections.orders]\nengine = \"postgres\"\nhost = \"db.example\"\n\
                      database = \"orders\"\nuser = \"app\"\n";

fn q(conn: Option<&str>, relative: &str, origin: impl Into<PathBuf>, body: &str) -> ImportedQuery {
    ImportedQuery::new(
        conn.map(str::to_owned),
        PathBuf::from(relative),
        origin.into(),
        body.as_bytes().to_vec(),
    )
}

/// One line per query action: `create <path>`, `unchanged <path>` or `skip <origin> <reason>`.
fn query_actions(p: &ImportPlan) -> Vec<String> {
    p.actions()
        .iter()
        .filter_map(|a| match a {
            Action::CreateQuery { path, .. } | Action::QueryUnchanged { path, .. } => {
                Some(format!("{} {}", a.code(), path.display()))
            }
            Action::SkipQuery { origin, reason } => {
                Some(format!("skip {} {}", origin.display(), reason.code()))
            }
            Action::AddConnection { .. }
            | Action::ConnectionUnchanged { .. }
            | Action::ConnectionConflict { .. } => None,
        })
        .collect()
}

#[test]
fn bound_queries_go_to_their_connection_folder_and_the_rest_to_queries() {
    let ws = ws_with(None);
    let mut set = set_of(vec![pg("orders", "db.example")]);
    set.add_query(q(
        Some("id-orders-db.example"),
        "reports/top.sql",
        "/src/queries/reports/top.sql",
        "select 1;\n",
    ));
    set.add_query(q(
        None,
        "notes.sql",
        "/src/queries/notes.sql",
        "select 2;\n",
    ));
    set.add_query(q(
        Some("id-not-in-set"),
        "lost.sql",
        "/src/lost.sql",
        "select 3;\n",
    ));
    let p = plan(ws.path(), &set, OnConflict::Skip).unwrap();
    assert_eq!(
        query_actions(&p),
        [
            "create queries/orders/reports/top.sql",
            "create queries/notes.sql",
            "create queries/lost.sql"
        ]
    );
    p.apply().unwrap();
    assert_eq!(
        std::fs::read(ws.path().join("queries/orders/reports/top.sql")).unwrap(),
        b"select 1;\n"
    );
    let saved = Workspace::open(ws.path()).unwrap().saved_queries().unwrap();
    let bound: Vec<_> = saved.iter().filter_map(|s| s.connection()).collect();
    assert_eq!(bound, ["orders"]);
}

#[test]
fn identical_bytes_are_unchanged_and_different_bytes_take_the_next_free_sibling() {
    let ws = ws_with(None);
    std::fs::create_dir_all(ws.path().join("queries")).unwrap();
    std::fs::write(ws.path().join("queries/notes.sql"), "a").unwrap();
    std::fs::write(ws.path().join("queries/notes-2.sql"), "b").unwrap();
    let mut set = set_of(vec![]);
    for (body, from) in [
        ("b", "/s/1.sql"),
        ("c", "/s/2.sql"),
        ("a", "/s/3.sql"),
        ("c", "/s/4.sql"),
    ] {
        set.add_query(q(None, "notes.sql", from, body));
    }
    let p = plan(ws.path(), &set, OnConflict::Skip).unwrap();
    assert_eq!(
        query_actions(&p),
        [
            "unchanged queries/notes-2.sql",
            "create queries/notes-3.sql",
            "unchanged queries/notes.sql",
            "unchanged queries/notes-3.sql"
        ]
    );
    let applied = p.apply().unwrap();
    let dir = std::fs::canonicalize(ws.path()).unwrap();
    assert_eq!(
        applied.written(),
        [dir.join("queries/notes-3.sql"), dir.join("dbento.toml")]
    );
}

#[test]
fn queries_follow_their_connection_into_a_conflict_or_a_rename() {
    let ws = ws_with(Some(ORDERS));
    let mut set = set_of(vec![pg("orders", "other.example")]);
    set.add_query(q(
        Some("id-orders-other.example"),
        "c.sql",
        "/src/c.sql",
        "x",
    ));
    let skip = plan(ws.path(), &set, OnConflict::Skip).unwrap();
    assert_eq!(
        query_actions(&skip),
        ["skip /src/c.sql connection_conflict"]
    );
    let rename = plan(ws.path(), &set, OnConflict::Rename).unwrap();
    assert_eq!(query_actions(&rename), ["create queries/orders-2/c.sql"]);
}

#[test]
fn a_file_already_under_the_workspace_queries_is_unchanged_in_place() {
    let ws = ws_with(Some(ORDERS));
    std::fs::create_dir_all(ws.path().join("queries/orders")).unwrap();
    let inside = ws.path().join("queries/orders/in.sql");
    std::fs::write(&inside, "x").unwrap();
    let loose = ws.path().join("queries/loose.sql");
    std::fs::write(&loose, "y").unwrap();
    let mut set = set_of(vec![pg("orders", "db.example")]);
    set.add_query(q(
        Some("id-orders-db.example"),
        "orders/in.sql",
        &inside,
        "x",
    ));
    set.add_query(q(None, "loose.sql", &loose, "y"));
    let p = plan(ws.path(), &set, OnConflict::Skip).unwrap();
    assert_eq!(
        query_actions(&p),
        [
            "unchanged queries/orders/in.sql",
            "unchanged queries/loose.sql"
        ]
    );
    let notes: Vec<_> = p
        .warnings()
        .iter()
        .map(|w| (w.subject(), w.kind().clone()))
        .collect();
    assert_eq!(
        notes,
        [("queries/orders/in.sql", WarningKind::BoundByFolder)]
    );
}

#[test]
fn a_relative_path_leaving_its_folder_is_outside_root() {
    let ws = ws_with(None);
    let mut set = set_of(vec![]);
    set.add_query(q(None, "../x.sql", "/s/x.sql", "x"));
    set.add_query(q(None, "/etc/y.sql", "/s/y.sql", "y"));
    let p = plan(ws.path(), &set, OnConflict::Skip).unwrap();
    assert_eq!(
        query_actions(&p),
        ["skip /s/x.sql outside_root", "skip /s/y.sql outside_root"]
    );
}

#[cfg(unix)]
#[test]
fn symlinked_target_folders_and_files_are_never_written_through() {
    let ws = ws_with(Some(ORDERS));
    let elsewhere = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(ws.path().join("queries")).unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), ws.path().join("queries/orders")).unwrap();
    std::os::unix::fs::symlink(
        elsewhere.path().join("n.sql"),
        ws.path().join("queries/notes.sql"),
    )
    .unwrap();
    let mut set = set_of(vec![pg("orders", "db.example")]);
    set.add_query(q(Some("id-orders-db.example"), "c.sql", "/s/c.sql", "x"));
    set.add_query(q(None, "notes.sql", "/s/notes.sql", "y"));
    let p = plan(ws.path(), &set, OnConflict::Skip).unwrap();
    assert_eq!(
        query_actions(&p),
        ["skip /s/c.sql symlink", "skip /s/notes.sql symlink"]
    );
    p.apply().unwrap();
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
}

#[test]
fn queries_are_written_before_dbento_toml_and_a_rerun_converges() {
    let ws = ws_with(Some(ORDERS));
    let mut set = set_of(vec![pg("shop", "db.example")]);
    set.add_query(q(Some("id-shop-db.example"), "a.sql", "/s/a.sql", "x"));
    let p = plan(ws.path(), &set, OnConflict::Skip).unwrap();
    std::fs::write(ws.path().join("dbento.toml"), format!("{ORDERS}# edited\n")).unwrap();
    let dir = std::fs::canonicalize(ws.path()).unwrap();
    match p.apply() {
        Err(ApplyError::Changed { written }) => {
            assert_eq!(written, [dir.join("queries/shop/a.sql")])
        }
        other => panic!("{other:?}"),
    }
    let again = plan(ws.path(), &set, OnConflict::Skip).unwrap();
    assert_eq!(query_actions(&again), ["unchanged queries/shop/a.sql"]);
    again.apply().unwrap();
    assert!(
        Workspace::open(ws.path())
            .unwrap()
            .connection("shop")
            .is_some()
    );
}

#[test]
fn a_disk_error_part_way_reports_what_was_written() {
    let ws = ws_with(Some(ORDERS));
    std::fs::create_dir_all(ws.path().join("queries")).unwrap();
    std::fs::write(
        ws.path().join("queries/orders"),
        "a file where a folder belongs",
    )
    .unwrap();
    let mut set = set_of(vec![pg("orders", "db.example")]);
    set.add_query(q(None, "a.sql", "/s/a.sql", "x"));
    set.add_query(q(Some("id-orders-db.example"), "b.sql", "/s/b.sql", "y"));
    let p = plan(ws.path(), &set, OnConflict::Skip).unwrap();
    assert_eq!(
        query_actions(&p),
        ["create queries/a.sql", "create queries/orders/b.sql"]
    );
    let dir = std::fs::canonicalize(ws.path()).unwrap();
    match p.apply() {
        Err(ApplyError::Io { path, written, .. }) => {
            assert_eq!(written, [dir.join("queries/a.sql")]);
            assert!(
                path.starts_with(dir.join("queries/orders")),
                "{}",
                path.display()
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_file_created_at_the_target_after_plan_is_never_overwritten() {
    let ws = ws_with(Some(ORDERS));
    let mut set = set_of(vec![]);
    set.add_query(q(None, "a.sql", "/s/a.sql", "new"));
    let p = plan(ws.path(), &set, OnConflict::Skip).unwrap();
    assert_eq!(query_actions(&p), ["create queries/a.sql"]);
    let target = ws.path().join("queries/a.sql");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, "someone else's").unwrap();
    match p.apply() {
        Err(ApplyError::Io { written, .. }) => assert!(written.is_empty()),
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "someone else's");
}

#[test]
fn a_third_and_fourth_sibling_take_the_next_numbers_and_a_rerun_converges() {
    let ws = ws_with(None);
    let mut set = set_of(vec![]);
    for (body, from) in [
        ("a", "/s/1.sql"),
        ("b", "/s/2.sql"),
        ("c", "/s/3.sql"),
        ("d", "/s/4.sql"),
    ] {
        set.add_query(q(None, "notes.sql", from, body));
    }
    for (body, from) in [
        ("x", "/s/a/README"),
        ("y", "/s/b/README"),
        ("z", "/s/c/README"),
    ] {
        set.add_query(q(None, "README", from, body));
    }
    let p = plan(ws.path(), &set, OnConflict::Skip).unwrap();
    let created = [
        "queries/notes.sql",
        "queries/notes-2.sql",
        "queries/notes-3.sql",
        "queries/notes-4.sql",
        "queries/README",
        "queries/README-2",
        "queries/README-3",
    ];
    let expect = |code: &str| {
        created
            .iter()
            .map(|p| format!("{code} {p}"))
            .collect::<Vec<_>>()
    };
    assert_eq!(query_actions(&p), expect("create"));
    p.apply().unwrap();
    assert_eq!(
        std::fs::read(ws.path().join("queries/notes-4.sql")).unwrap(),
        b"d"
    );
    let again = plan(ws.path(), &set, OnConflict::Skip).unwrap();
    assert_eq!(query_actions(&again), expect("unchanged"));
}
