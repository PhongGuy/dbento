//! Write mechanics of `plan` and `apply`: the append, the re-parse and the guards.
use super::*;
use crate::import::model::{
    ConnectionDraft, ImportSet, ImportedConnection, PgDraft, SecretSetting,
};
use crate::{Color, OpenError, TlsMode, Workspace};
use std::collections::BTreeMap;
use std::path::Path;

pub(super) fn pg_fields(host: &str) -> PgDraft {
    PgDraft {
        host: host.to_owned(),
        port: None,
        database: "orders".to_owned(),
        user: "app".to_owned(),
        secret: SecretSetting::Keychain,
        tls: Some(TlsMode::Verify),
        tls_ca: None,
        tls_cert: None,
        tls_key: None,
        settings: BTreeMap::new(),
    }
}

/// A writable Postgres draft `name` for `app@host/orders`, with source id `id-<name>-<host>`.
pub(super) fn pg(name: &str, host: &str) -> ImportedConnection {
    let draft = ConnectionDraft::postgres(pg_fields(host), false);
    ImportedConnection::new(
        format!("id-{name}-{host}"),
        name.to_owned(),
        name.to_owned(),
        draft,
    )
}

pub(super) fn set_of(connections: Vec<ImportedConnection>) -> ImportSet {
    let mut set = ImportSet::default();
    for c in connections {
        set.add_connection(c);
    }
    set
}

/// A temporary workspace folder, holding `dbento.toml` when `toml` is given.
pub(super) fn ws_with(toml: Option<&str>) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    if let Some(text) = toml {
        std::fs::write(dir.path().join("dbento.toml"), text).unwrap();
    }
    dir
}

/// `(name, action code)` for every connection action, in order.
pub(super) fn conn_actions(p: &ImportPlan) -> Vec<(String, &'static str)> {
    p.actions()
        .iter()
        .filter_map(|a| match a {
            Action::AddConnection { name, .. }
            | Action::ConnectionUnchanged { name, .. }
            | Action::ConnectionConflict { name, .. } => Some((name.clone(), a.code())),
            Action::CreateQuery { .. }
            | Action::QueryUnchanged { .. }
            | Action::SkipQuery { .. } => None,
        })
        .collect()
}

pub(super) fn toml_text(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("dbento.toml")).unwrap()
}

const OLD: &str =
    "dbento = 1\n# keep me\n[connections.old]\nengine = \"sqlite\"\npath = \"old.db\"\n";

#[test]
fn new_tables_follow_the_old_text_byte_for_byte() {
    let ws = ws_with(Some(OLD));
    let mut fields = pg_fields("db.example");
    fields.port = Some(6543);
    fields
        .settings
        .insert("search_path".to_owned(), "sales".to_owned());
    let draft = ConnectionDraft::postgres(fields, true);
    let set = set_of(vec![ImportedConnection::new(
        "u1".to_owned(),
        "Orders DB".to_owned(),
        "orders-db".to_owned(),
        draft,
    )]);
    let p = plan(ws.path(), &set, OnConflict::Skip).unwrap();
    assert_eq!(conn_actions(&p), [("orders-db".to_owned(), "add")]);
    let applied = p.apply().unwrap();
    let dir = std::fs::canonicalize(ws.path()).unwrap();
    assert_eq!(applied.written(), [dir.join("dbento.toml")]);
    let expected = format!(
        "{OLD}\n[connections.orders-db]\nengine = \"postgres\"\nhost = \"db.example\"\nport = 6543\n\
         database = \"orders\"\nuser = \"app\"\ntls = \"verify\"\nread_only = true\n\n\
         [connections.orders-db.settings]\nsearch_path = \"sales\"\n"
    );
    assert_eq!(toml_text(ws.path()), expected);
    let opened = Workspace::open(ws.path()).unwrap();
    assert!(opened.declared("orders-db").unwrap().read_only());
}

#[test]
fn a_missing_file_and_folder_are_created_from_the_init_template() {
    let parent = tempfile::TempDir::new().unwrap();
    let dir = parent.path().join("new-ws");
    let lite = ConnectionDraft::sqlite("/elsewhere/app.db".to_owned(), false);
    let fields = PgDraft {
        secret: SecretSetting::None,
        tls: Some(TlsMode::Off),
        ..pg_fields("localhost")
    };
    let set = set_of(vec![
        ImportedConnection::new(
            "u1".to_owned(),
            "app.db".to_owned(),
            "lite".to_owned(),
            lite,
        ),
        ImportedConnection::new(
            "u2".to_owned(),
            "Local".to_owned(),
            "local-pg".to_owned(),
            ConnectionDraft::postgres(fields, false),
        ),
    ]);
    plan(&dir, &set, OnConflict::Skip).unwrap().apply().unwrap();
    let text = toml_text(&dir);
    let tables = text
        .strip_prefix(crate::workspace::TEMPLATE)
        .expect("starts with the template");
    assert_eq!(
        tables,
        "\n[connections.lite]\nengine = \"sqlite\"\npath = \"/elsewhere/app.db\"\n\n\
         [connections.local-pg]\nengine = \"postgres\"\nhost = \"localhost\"\ndatabase = \"orders\"\n\
         user = \"app\"\nsecret = \"none\"\ntls = \"off\"\n"
    );
    assert_eq!(Workspace::open(&dir).unwrap().connections().len(), 2);
}

#[test]
fn an_old_text_without_a_final_newline_still_gets_a_blank_line() {
    let ws = ws_with(Some("dbento = 1"));
    let set = set_of(vec![pg("a", "h.example")]);
    plan(ws.path(), &set, OnConflict::Skip)
        .unwrap()
        .apply()
        .unwrap();
    assert!(toml_text(ws.path()).starts_with("dbento = 1\n\n[connections.a]\n"));
}

#[test]
fn an_inline_connections_table_is_unmergeable_and_nothing_is_written() {
    for old in [
        "dbento = 1\nconnections = {}\n",
        "dbento = 1\nconnections = { a = { engine = \"sqlite\", path = \"a.db\" } }\n",
    ] {
        let ws = ws_with(Some(old));
        let err = plan(
            ws.path(),
            &set_of(vec![pg("b", "h.example")]),
            OnConflict::Skip,
        )
        .unwrap_err();
        assert!(
            matches!(err, PlanError::Unmergeable { .. }),
            "{old}: {err:?}"
        );
        assert_eq!(toml_text(ws.path()), old);
    }
}

#[test]
fn an_invalid_existing_file_is_a_workspace_error() {
    let ws = ws_with(Some("dbento = 2\n"));
    let err = plan(ws.path(), &set_of(vec![]), OnConflict::Skip).unwrap_err();
    assert!(
        matches!(
            err,
            PlanError::Workspace(OpenError::UnsupportedVersion { found: 2 })
        ),
        "{err:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_dbento_toml_is_refused() {
    let ws = ws_with(None);
    std::fs::write(ws.path().join("real.toml"), "dbento = 1\n").unwrap();
    std::os::unix::fs::symlink("real.toml", ws.path().join("dbento.toml")).unwrap();
    let err = plan(
        ws.path(),
        &set_of(vec![pg("a", "h.example")]),
        OnConflict::Skip,
    )
    .unwrap_err();
    assert!(matches!(err, PlanError::Symlink { .. }), "{err:?}");
}

#[test]
fn a_file_edited_between_plan_and_apply_aborts_with_changed() {
    let ws = ws_with(Some(OLD));
    let p = plan(
        ws.path(),
        &set_of(vec![pg("a", "h.example")]),
        OnConflict::Skip,
    )
    .unwrap();
    let edited = format!("{OLD}# edited\n");
    std::fs::write(ws.path().join("dbento.toml"), &edited).unwrap();
    match p.apply() {
        Err(e @ ApplyError::Changed { .. }) => assert!(e.written().is_empty()),
        other => panic!("{other:?}"),
    }
    assert_eq!(toml_text(ws.path()), edited);
}

#[test]
fn a_taken_name_is_a_conflict_and_nothing_is_written() {
    let ws = ws_with(Some(OLD));
    let p = plan(
        ws.path(),
        &set_of(vec![pg("old", "h.example")]),
        OnConflict::Skip,
    )
    .unwrap();
    assert_eq!(conn_actions(&p), [("old".to_owned(), "conflict")]);
    assert!(p.apply().unwrap().written().is_empty());
    assert_eq!(toml_text(ws.path()), OLD);
}

#[test]
fn two_drafts_with_one_name_under_skip_add_the_first_and_conflict_the_second() {
    let ws = ws_with(None);
    let p = plan(
        ws.path(),
        &set_of(vec![pg("a", "h1.example"), pg("a", "h2.example")]),
        OnConflict::Skip,
    )
    .unwrap();
    assert_eq!(
        conn_actions(&p),
        [("a".to_owned(), "add"), ("a".to_owned(), "conflict")]
    );
    p.apply().unwrap();
    assert!(toml_text(ws.path()).contains("host = \"h1.example\""));
}

#[test]
fn a_stale_temporary_file_is_replaced_and_gone_after_apply() {
    let ws = ws_with(Some(OLD));
    let tmp = ws.path().join(".dbento.toml.import.tmp");
    std::fs::write(&tmp, "left by a crashed run").unwrap();
    let p = plan(
        ws.path(),
        &set_of(vec![pg("a", "h.example")]),
        OnConflict::Skip,
    )
    .unwrap();
    p.apply().unwrap();
    assert!(!tmp.exists());
    assert!(toml_text(ws.path()).starts_with(OLD));
}

#[cfg(unix)]
#[test]
fn apply_keeps_the_permissions_of_the_existing_file() {
    use std::os::unix::fs::PermissionsExt;
    let ws = ws_with(Some(OLD));
    let file = ws.path().join("dbento.toml");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let p = plan(
        ws.path(),
        &set_of(vec![pg("a", "h.example")]),
        OnConflict::Skip,
    )
    .unwrap();
    p.apply().unwrap();
    let mode = std::fs::metadata(&file).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn a_crlf_file_stays_crlf_after_the_append() {
    let old = OLD.replace('\n', "\r\n");
    let ws = ws_with(Some(&old));
    let p = plan(
        ws.path(),
        &set_of(vec![pg("a", "h.example")]),
        OnConflict::Skip,
    )
    .unwrap();
    p.apply().unwrap();
    let text = toml_text(ws.path());
    assert!(text.starts_with(&old), "{text:?}");
    assert!(text.contains("[connections.a]\r\n"), "{text:?}");
    assert!(!text.replace("\r\n", "").contains('\n'), "{text:?}");
}

#[test]
fn a_bad_draft_is_reported_as_itself_not_as_an_inline_table() {
    let ws = ws_with(Some(OLD));
    let mut fields = pg_fields("h.example");
    fields
        .settings
        .insert("password".to_owned(), "x".to_owned());
    let draft = ConnectionDraft::postgres(fields, false);
    let set = set_of(vec![ImportedConnection::new(
        "u1".to_owned(),
        "A".to_owned(),
        "a".to_owned(),
        draft,
    )]);
    let err = plan(ws.path(), &set, OnConflict::Skip).unwrap_err();
    assert!(matches!(err, PlanError::Workspace(_)), "{err:?}");
    assert_eq!(toml_text(ws.path()), OLD);
}

#[test]
fn a_draft_folder_is_written_and_reopens_as_that_folder() {
    let ws = ws_with(Some(OLD));
    let draft = ConnectionDraft::postgres(pg_fields("db.example"), true)
        .with_folder(Some("Prod/EU".parse().unwrap()));
    let set = set_of(vec![ImportedConnection::new(
        "u1".to_owned(),
        "Orders DB".to_owned(),
        "orders-db".to_owned(),
        draft,
    )]);
    plan(ws.path(), &set, OnConflict::Skip)
        .unwrap()
        .apply()
        .unwrap();
    let expected = format!(
        "{OLD}\n[connections.orders-db]\nengine = \"postgres\"\nhost = \"db.example\"\n\
         database = \"orders\"\nuser = \"app\"\ntls = \"verify\"\nread_only = true\nfolder = \"Prod/EU\"\n"
    );
    assert_eq!(toml_text(ws.path()), expected);
    let opened = Workspace::open(ws.path()).unwrap();
    let conn = opened.declared("orders-db").unwrap();
    assert_eq!(conn.folder().map(|f| f.as_str()), Some("Prod/EU"));
}

#[test]
fn odd_folder_names_survive_toml_escaping_through_apply() {
    let odd = [
        "Q \"x\"\\y.z",
        "it's \"both\"",
        "#hash = [x]",
        "a.b/c'd",
        "小城镇/EU west",
        "tab\u{2003}em/\u{1F600}",
    ];
    for (n, folder) in odd.into_iter().enumerate() {
        let ws = ws_with(Some(OLD));
        let draft = ConnectionDraft::postgres(pg_fields("db.example"), true)
            .with_folder(Some(folder.parse().unwrap()));
        let set = set_of(vec![ImportedConnection::new(
            "u1".to_owned(),
            "Odd".to_owned(),
            "odd".to_owned(),
            draft,
        )]);
        plan(ws.path(), &set, OnConflict::Skip)
            .unwrap()
            .apply()
            .unwrap();
        let opened = Workspace::open(ws.path()).unwrap();
        let got = opened.declared("odd").unwrap().folder().map(|f| f.as_str());
        assert_eq!(got, Some(folder), "case {n}: {}", toml_text(ws.path()));
    }
}

#[cfg(unix)]
#[test]
fn apply_refuses_a_dbento_toml_turned_into_a_symlink_after_planning() {
    let ws = ws_with(Some(OLD));
    let p = plan(
        ws.path(),
        &set_of(vec![pg("a", "h.example")]),
        OnConflict::Skip,
    )
    .unwrap();
    let (file, real) = (ws.path().join("dbento.toml"), ws.path().join("real.toml"));
    std::fs::rename(&file, &real).unwrap();
    std::os::unix::fs::symlink(&real, &file).unwrap();
    match p.apply() {
        Err(ApplyError::Io { message, .. }) => {
            assert!(message.contains("symbolic link"), "{message}")
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read_to_string(&real).unwrap(), OLD);
}

#[test]
fn a_draft_colour_is_written_and_reopens_as_that_colour() {
    let ws = ws_with(None);
    let draft =
        ConnectionDraft::postgres(pg_fields("db.example"), false).with_color(Some(Color::Red));
    let set = set_of(vec![ImportedConnection::new(
        "u1".to_owned(),
        "X".to_owned(),
        "x".to_owned(),
        draft,
    )]);
    plan(ws.path(), &set, OnConflict::Skip)
        .unwrap()
        .apply()
        .unwrap();
    assert!(toml_text(ws.path()).contains("[connections.x]\nengine = \"postgres\"\nhost = \"db.example\"\ndatabase = \"orders\"\nuser = \"app\"\ntls = \"verify\"\ncolor = \"red\"\n"));
    let opened = Workspace::open(ws.path()).unwrap();
    assert_eq!(opened.declared("x").unwrap().color(), Some(Color::Red));
}

#[test]
fn a_draft_without_a_colour_writes_no_color_key() {
    let ws = ws_with(None);
    plan(
        ws.path(),
        &set_of(vec![pg("x", "db.example")]),
        OnConflict::Skip,
    )
    .unwrap()
    .apply()
    .unwrap();
    // The init template mentions `# color` in a comment, so look for a key line only.
    assert!(!toml_text(ws.path()).lines().any(|l| l.starts_with("color")));
}

#[test]
fn a_red_draft_of_an_uncoloured_existing_server_is_unchanged_and_not_retagged() {
    let old = "dbento = 1\n[connections.x]\nengine = \"postgres\"\nhost = \"db.example\"\n\
               database = \"orders\"\nuser = \"app\"\n";
    let ws = ws_with(Some(old));
    let draft =
        ConnectionDraft::postgres(pg_fields("db.example"), false).with_color(Some(Color::Red));
    let set = set_of(vec![ImportedConnection::new(
        "u1".to_owned(),
        "X".to_owned(),
        "x".to_owned(),
        draft,
    )]);
    let p = plan(ws.path(), &set, OnConflict::Skip).unwrap();
    assert_eq!(conn_actions(&p), [("x".to_owned(), "unchanged")]);
    p.apply().unwrap();
    assert_eq!(toml_text(ws.path()), old);
}

#[test]
fn a_skipped_name_clash_keeps_the_existing_colour() {
    let old =
        "dbento = 1\n[connections.x]\nengine = \"sqlite\"\npath = \"old.db\"\ncolor = \"blue\"\n";
    let ws = ws_with(Some(old));
    let draft =
        ConnectionDraft::postgres(pg_fields("db.example"), false).with_color(Some(Color::Red));
    let set = set_of(vec![ImportedConnection::new(
        "u1".to_owned(),
        "X".to_owned(),
        "x".to_owned(),
        draft,
    )]);
    let p = plan(ws.path(), &set, OnConflict::Skip).unwrap();
    assert_eq!(conn_actions(&p), [("x".to_owned(), "conflict")]);
    p.apply().unwrap();
    let text = toml_text(ws.path());
    assert_eq!(text, old);
    assert!(!text.contains("red"));
}
