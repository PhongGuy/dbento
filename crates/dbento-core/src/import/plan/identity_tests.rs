//! Identity, Rename and the SQLite path rules of `plan`.
use super::tests::{conn_actions, pg, pg_fields, set_of, toml_text, ws_with};
use super::*;
use crate::Workspace;
use crate::import::model::{ConnectionDraft, ImportedConnection, MysqlDraft, WarningKind};

const ORDERS: &str = "dbento = 1\n[connections.orders]\nengine = \"postgres\"\nhost = \"db.example\"\n\
                      database = \"orders\"\nuser = \"app\"\n";

fn read_only_pg(name: &str, host: &str) -> ImportedConnection {
    let draft = ConnectionDraft::postgres(pg_fields(host), true);
    ImportedConnection::new(format!("ro-{name}"), name, name, draft)
}

fn sqlite(name: &str, path: &str) -> ImportedConnection {
    ImportedConnection::new(
        format!("id-{name}"),
        format!("{name}.db"),
        name,
        ConnectionDraft::sqlite(path.to_owned(), false),
    )
}

#[test]
fn later_edits_to_a_matching_connection_keep_it_unchanged() {
    let old = "dbento = 1\n[connections.orders]\nengine = \"postgres\"\nhost = \"DB.example\"\nport = 5432\n\
               database = \"orders\"\nuser = \"app\"\ncolor = \"red\"\nread_only = true\ntls = \"off\"\n\
               secret = \"none\"\nstatement_timeout = \"5s\"\n\n[connections.orders.settings]\nsearch_path = \"x\"\n";
    let ws = ws_with(Some(old));
    let p = plan(
        ws.path(),
        &set_of(vec![pg("orders", "db.example")]),
        OnConflict::Skip,
    )
    .unwrap();
    assert_eq!(conn_actions(&p), [("orders".to_owned(), "unchanged")]);
    assert!(p.apply().unwrap().written().is_empty());
    assert_eq!(toml_text(ws.path()), old);
}

#[test]
fn a_different_host_under_skip_is_a_conflict() {
    let ws = ws_with(Some(ORDERS));
    let p = plan(
        ws.path(),
        &set_of(vec![pg("orders", "other.example")]),
        OnConflict::Skip,
    )
    .unwrap();
    assert_eq!(conn_actions(&p), [("orders".to_owned(), "conflict")]);
    assert!(p.apply().unwrap().written().is_empty());
}

#[test]
fn rename_adds_one_sibling_and_a_rerun_adds_none() {
    let ws = ws_with(Some(ORDERS));
    let set = set_of(vec![pg("orders", "other.example")]);
    let p = plan(ws.path(), &set, OnConflict::Rename).unwrap();
    assert_eq!(conn_actions(&p), [("orders-2".to_owned(), "add")]);
    p.apply().unwrap();
    let again = plan(ws.path(), &set, OnConflict::Rename).unwrap();
    assert_eq!(conn_actions(&again), [("orders-2".to_owned(), "unchanged")]);
    assert_eq!(Workspace::open(ws.path()).unwrap().connections().len(), 2);
}

#[test]
fn rename_never_takes_the_name_of_another_draft_in_the_set() {
    // The set's clash rule named the second draft `orders-2`; the first conflicts with `orders`.
    let ws = ws_with(Some(ORDERS));
    let set = set_of(vec![pg("orders", "b.example"), pg("orders-2", "c.example")]);
    let p = plan(ws.path(), &set, OnConflict::Rename).unwrap();
    assert_eq!(
        conn_actions(&p),
        [
            ("orders-3".to_owned(), "add"),
            ("orders-2".to_owned(), "add")
        ]
    );
    p.apply().unwrap();
    let again = plan(ws.path(), &set, OnConflict::Rename).unwrap();
    assert_eq!(
        conn_actions(&again),
        [
            ("orders-3".to_owned(), "unchanged"),
            ("orders-2".to_owned(), "unchanged")
        ]
    );
}

#[test]
fn names_claimed_earlier_in_the_plan_count_as_taken() {
    let ws = ws_with(Some(ORDERS));
    let set = set_of(vec![pg("orders", "b.example"), pg("orders", "c.example")]);
    let p = plan(ws.path(), &set, OnConflict::Rename).unwrap();
    assert_eq!(
        conn_actions(&p),
        [
            ("orders-2".to_owned(), "add"),
            ("orders-3".to_owned(), "add")
        ]
    );
    p.apply().unwrap();
    assert_eq!(Workspace::open(ws.path()).unwrap().connections().len(), 3);
}

#[test]
fn a_read_only_source_never_matches_a_writable_connection() {
    let ws = ws_with(Some(ORDERS));
    let set = set_of(vec![read_only_pg("orders", "db.example")]);
    let p = plan(ws.path(), &set, OnConflict::Skip).unwrap();
    assert_eq!(conn_actions(&p), [("orders".to_owned(), "conflict")]);
    let p = plan(ws.path(), &set, OnConflict::Rename).unwrap();
    assert_eq!(conn_actions(&p), [("orders-2".to_owned(), "add")]);
    p.apply().unwrap();
    let opened = Workspace::open(ws.path()).unwrap();
    assert!(opened.connection("orders-2").unwrap().read_only());
    assert!(!opened.connection("orders").unwrap().read_only());
}

#[test]
fn port_5432_and_no_port_are_the_same_server() {
    let ws = ws_with(Some(ORDERS));
    let mut fields = pg_fields("db.example");
    fields.port = Some(5432);
    let draft = ConnectionDraft::postgres(fields, false);
    let set = set_of(vec![ImportedConnection::new(
        "p", "orders", "orders", draft,
    )]);
    let p = plan(ws.path(), &set, OnConflict::Skip).unwrap();
    assert_eq!(conn_actions(&p), [("orders".to_owned(), "unchanged")]);
}

#[test]
fn sqlite_matches_the_resolved_file_and_is_written_relative_inside_the_workspace() {
    let ws = ws_with(Some(
        "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"data/app.db\"\n",
    ));
    let dir = std::fs::canonicalize(ws.path()).unwrap();
    let at = |rel: &str| dir.join(rel).to_str().unwrap().to_owned();
    let set = set_of(vec![
        sqlite("lite", &at("data/app.db")),
        sqlite("other", &at("data/other.db")),
    ]);
    let p = plan(ws.path(), &set, OnConflict::Skip).unwrap();
    assert_eq!(
        conn_actions(&p),
        [
            ("lite".to_owned(), "unchanged"),
            ("other".to_owned(), "add")
        ]
    );
    assert!(p.warnings().is_empty());
    p.apply().unwrap();
    assert!(
        toml_text(ws.path())
            .contains("[connections.other]\nengine = \"sqlite\"\npath = \"data/other.db\"\n")
    );
}

#[test]
fn sqlite_outside_the_workspace_stays_absolute_with_a_warning() {
    let ws = ws_with(None);
    let p = plan(
        ws.path(),
        &set_of(vec![sqlite("far", "/elsewhere/far.db")]),
        OnConflict::Skip,
    )
    .unwrap();
    let warnings: Vec<_> = p
        .warnings()
        .iter()
        .map(|w| (w.subject(), w.kind().clone()))
        .collect();
    assert_eq!(warnings, [("far.db", WarningKind::MachineSpecificPath)]);
    p.apply().unwrap();
    assert!(toml_text(ws.path()).contains("path = \"/elsewhere/far.db\"\n"));
}

#[test]
fn a_windows_sqlite_path_matches_its_own_connection_and_reruns_add_nothing() {
    let ws = ws_with(Some(
        "dbento = 1\n[connections.win]\nengine = \"sqlite\"\npath = 'C:\\data\\app.db'\n",
    ));
    let set = set_of(vec![sqlite("win", "C:\\data\\app.db")]);
    for mode in [OnConflict::Skip, OnConflict::Rename] {
        let p = plan(ws.path(), &set, mode).unwrap();
        assert_eq!(conn_actions(&p), [("win".to_owned(), "unchanged")]);
    }
    let other = set_of(vec![sqlite("win", "C:\\data\\other.db")]);
    let p = plan(ws.path(), &other, OnConflict::Rename).unwrap();
    assert_eq!(conn_actions(&p), [("win-2".to_owned(), "add")]);
}

#[test]
fn only_n_2_and_up_count_as_siblings() {
    let ws = ws_with(Some(
        "dbento = 1\n[connections.orders-1]\nengine = \"postgres\"\nhost = \"db.example\"\n\
         database = \"orders\"\nuser = \"app\"\n",
    ));
    let set = set_of(vec![pg("orders", "db.example")]);
    let p = plan(ws.path(), &set, OnConflict::Rename).unwrap();
    assert_eq!(conn_actions(&p), [("orders".to_owned(), "add")]);
}

#[test]
fn a_connection_filed_in_another_folder_is_still_unchanged() {
    let old = format!("{ORDERS}folder = \"Mine\"\n");
    let ws = ws_with(Some(&old));
    let draft = ConnectionDraft::postgres(pg_fields("db.example"), false)
        .with_folder(Some("Theirs".parse().unwrap()));
    let set = set_of(vec![ImportedConnection::new(
        "u1", "orders", "orders", draft,
    )]);
    let p = plan(ws.path(), &set, OnConflict::Skip).unwrap();
    assert_eq!(conn_actions(&p), [("orders".to_owned(), "unchanged")]);
    assert_eq!(toml_text(ws.path()), old);
}

fn my_fields(port: Option<u16>, database: Option<&str>) -> MysqlDraft {
    MysqlDraft {
        host: "db.example".into(),
        port,
        database: database.map(str::to_owned),
        user: "app".into(),
        secret: SecretSetting::Keychain,
        tls: Some(TlsMode::Verify),
        tls_ca: None,
        tls_cert: None,
        tls_key: None,
    }
}

fn mysql(name: &str, port: Option<u16>) -> ImportedConnection {
    let draft = ConnectionDraft::mysql(my_fields(port, Some("orders")), false);
    ImportedConnection::new(format!("my-{name}"), name, name, draft)
}

#[test]
fn mysql_matches_on_port_3306_and_never_matches_a_postgres_twin() {
    let my = "dbento = 1\n[connections.shop]\nengine = \"mysql\"\nhost = \"db.example\"\n\
              database = \"orders\"\nuser = \"app\"\n";
    let ws = ws_with(Some(my));
    for port in [None, Some(3306)] {
        let p = plan(
            ws.path(),
            &set_of(vec![mysql("shop", port)]),
            OnConflict::Skip,
        )
        .unwrap();
        assert_eq!(
            conn_actions(&p),
            [("shop".to_owned(), "unchanged")],
            "{port:?}"
        );
    }
    let p = plan(
        ws.path(),
        &set_of(vec![mysql("shop", Some(5432))]),
        OnConflict::Skip,
    )
    .unwrap();
    assert_eq!(conn_actions(&p), [("shop".to_owned(), "conflict")]);
    let twin = "dbento = 1\n[connections.shop]\nengine = \"postgres\"\nhost = \"db.example\"\n\
                port = 3306\ndatabase = \"orders\"\nuser = \"app\"\n";
    let ws = ws_with(Some(twin));
    let p = plan(
        ws.path(),
        &set_of(vec![mysql("shop", None)]),
        OnConflict::Skip,
    )
    .unwrap();
    assert_eq!(conn_actions(&p), [("shop".to_owned(), "conflict")]);
}

#[test]
fn a_mysql_draft_is_written_as_engine_mysql_and_reopens() {
    let ws = ws_with(None);
    let draft = ConnectionDraft::mysql(my_fields(Some(3306), None), false);
    let set = set_of(vec![ImportedConnection::new("m", "Shop", "shop", draft)]);
    plan(ws.path(), &set, OnConflict::Skip)
        .unwrap()
        .apply()
        .unwrap();
    let text = toml_text(ws.path());
    assert!(
        text.contains(
            "[connections.shop]\nengine = \"mysql\"\nhost = \"db.example\"\nuser = \"app\"\ntls = \"verify\"\n"
        ),
        "{text}"
    );
    let opened = Workspace::open(ws.path()).unwrap();
    let c = opened.connection("shop").unwrap();
    assert_eq!(
        (c.engine(), c.port(), c.database()),
        (Engine::Mysql, Some(3306), None)
    );
}
