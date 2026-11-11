use crate::support;

use dbento_app::app::{AppPaths, Services, start};
use dbento_app::connection_dialog::Field;
use gpui_kit::{AppContext as _, TestAppContext};
use std::sync::Arc;

#[gpui_kit::test]
fn test_connection_against_postgres_with_the_typed_password(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let rt = cx.update(|cx| dbento_app::tokio_bridge::handle(cx));
    // The container's drop needs a runtime, and it outlives `db`.
    let _enter = rt.enter();
    let db = rt.block_on(dbento_testkit::pg_db());
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dbento.toml"), "dbento = 1\n").unwrap();
    let files = tempfile::tempdir().unwrap();
    let services = Services {
        paths: AppPaths::in_dir(files.path()),
        store: Arc::new(dbento_core::MemoryStore::new()),
        env: Arc::new(|_| None),
        secrets_in_memory: false,
    };
    let (win, shell) = cx
        .update(|cx| start(services, Some(dir.path().into()), cx))
        .unwrap();
    support::click_id(cx, win, "connection-new");
    support::wait_real(cx, "the dialog", |cx| {
        support::present(cx, win, "conn-name")
    });
    let f = shell.read_with(cx, |s, _| s.connection_form().cloned().unwrap());
    let set = |cx: &mut TestAppContext, field: Field, text: String| {
        let state = f.read_with(cx, |f, _| f.input(field).clone());
        cx.update_window(win, |_, w, cx| {
            state.update(cx, |s, cx| s.set_value(text, w, cx))
        })
        .unwrap();
        cx.run_until_parked();
    };
    set(cx, Field::Name, "pg".into());
    set(cx, Field::Host, db.host().to_owned());
    set(cx, Field::Port, db.port().to_string());
    set(cx, Field::Database, db.database().to_owned());
    set(cx, Field::User, db.user().to_owned());
    set(cx, Field::Password, db.password().to_owned());
    support::click_id(cx, win, "conn-test");
    support::wait_real(cx, "connected", |cx| {
        support::label_of(cx, win, "conn-test-result").as_deref() == Some("Connected")
    });

    set(cx, Field::Password, "wrong-secret-4471".into());
    support::click_id(cx, win, "conn-test");
    support::wait_real(cx, "the failure", |cx| {
        support::label_of(cx, win, "conn-test-result").is_some_and(|l| l.starts_with("Failed: "))
    });
    let failed = support::label_of(cx, win, "conn-test-result").unwrap();
    assert!(
        failed.contains("password authentication failed"),
        "{failed}"
    );
    assert!(!failed.contains("wrong-secret-4471"), "{failed}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("dbento.toml")).unwrap(),
        "dbento = 1\n"
    );
}

#[gpui_kit::test]
fn a_passed_test_offers_the_servers_databases_under_database(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let rt = cx.update(|cx| dbento_app::tokio_bridge::handle(cx));
    // The container's drop needs a runtime, and it outlives `db`.
    let _enter = rt.enter();
    let db = rt.block_on(dbento_testkit::pg_db());
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dbento.toml"), "dbento = 1\n").unwrap();
    let files = tempfile::tempdir().unwrap();
    let services = Services {
        paths: AppPaths::in_dir(files.path()),
        store: Arc::new(dbento_core::MemoryStore::new()),
        env: Arc::new(|_| None),
        secrets_in_memory: false,
    };
    let (win, shell) = cx
        .update(|cx| start(services, Some(dir.path().into()), cx))
        .unwrap();
    support::click_id(cx, win, "connection-new");
    support::wait_real(cx, "the dialog", |cx| {
        support::present(cx, win, "conn-name")
    });
    let f = shell.read_with(cx, |s, _| s.connection_form().cloned().unwrap());
    let set = |cx: &mut TestAppContext, field: Field, text: String| {
        let state = f.read_with(cx, |f, _| f.input(field).clone());
        cx.update_window(win, |_, w, cx| {
            state.update(cx, |s, cx| s.set_value(text, w, cx))
        })
        .unwrap();
        cx.run_until_parked();
    };
    // Postgres needs a database to test with; the server's own is enough to list the others.
    set(cx, Field::Name, "pg".into());
    set(cx, Field::Host, db.host().to_owned());
    set(cx, Field::Port, db.port().to_string());
    set(cx, Field::Database, "postgres".into());
    set(cx, Field::User, db.user().to_owned());
    set(cx, Field::Password, db.password().to_owned());
    support::click_id(cx, win, "conn-test");
    support::wait_real(cx, "connected", |cx| {
        support::label_of(cx, win, "conn-test-result").as_deref() == Some("Connected")
    });
    let offered = f.read_with(cx, |f, _| f.database_suggestions().to_vec());
    assert!(offered.iter().any(|d| d == db.database()), "{offered:?}");
    assert!(
        !offered.iter().any(|d| d == "postgres"),
        "the database Test used is not offered again: {offered:?}"
    );
    assert!(
        !offered.iter().any(|d| d.starts_with("template")),
        "{offered:?}"
    );
    assert_eq!(
        support::label_of(cx, win, ("conn-database-suggestion", 0usize)).as_deref(),
        offered.first().map(String::as_str)
    );
}

#[gpui_kit::test]
fn test_a_renamed_connection_with_the_secret_stored_under_its_old_name(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let rt = cx.update(|cx| dbento_app::tokio_bridge::handle(cx));
    let _enter = rt.enter();
    let db = rt.block_on(dbento_testkit::pg_db());
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!(
            "dbento = 1\n[connections.old]\nengine = \"postgres\"\nhost = \"{}\"\nport = {}\ndatabase = \"{}\"\nuser = \"{}\"\n",
            db.host(),
            db.port(),
            db.database(),
            db.user()
        ),
    )
    .unwrap();
    let store = Arc::new(dbento_core::MemoryStore::new());
    dbento_core::SecretStore::set(
        &*store,
        &dbento_core::SecretKey::new(dir.path(), "old"),
        &dbento_core::Secret::new(db.password()),
    )
    .unwrap();
    let files = tempfile::tempdir().unwrap();
    let services = Services {
        paths: AppPaths::in_dir(files.path()),
        store,
        env: Arc::new(|_| None),
        secrets_in_memory: false,
    };
    let (win, shell) = cx
        .update(|cx| start(services, Some(dir.path().into()), cx))
        .unwrap();
    support::present(cx, win, "connection-list");
    support::gear(cx, win, "old");
    support::wait_real(cx, "the dialog", |cx| {
        support::present(cx, win, "conn-name")
    });
    let f = shell.read_with(cx, |s, _| s.connection_form().cloned().unwrap());
    let name = f.read_with(cx, |f, _| f.input(Field::Name).clone());
    cx.update_window(win, |_, w, cx| {
        name.update(cx, |s, cx| s.set_value("new".to_owned(), w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    support::click_id(cx, win, "conn-test");
    support::wait_real(cx, "a test result", |cx| {
        support::label_of(cx, win, "conn-test-result").is_some_and(|l| l != "Testing\u{2026}")
    });
    assert_eq!(
        support::label_of(cx, win, "conn-test-result").as_deref(),
        Some("Connected")
    );
}
