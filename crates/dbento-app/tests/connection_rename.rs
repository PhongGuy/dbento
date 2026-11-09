use crate::support;
use dbento_app::explorer::connection_id;

use crate::support::{click_id, label_of, present, wait_real};
use dbento_app::app::start;
use dbento_app::connection_dialog::Field;
use dbento_core::{Secret, SecretError, SecretKey, SecretStore};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};
use std::sync::Arc;

/// The app on a fixture with no tab open.
fn idle_app(
    cx: &mut TestAppContext,
    store: Arc<dyn SecretStore>,
) -> (
    support::Fx,
    tempfile::TempDir,
    AnyWindowHandle,
    Entity<dbento_app::shell::Shell>,
) {
    support::init(cx);
    support::init_tokio(cx);
    let fx = support::fixture("", "");
    let files = tempfile::tempdir().unwrap();
    let mut services = support::services(files.path());
    services.store = store;
    let (win, shell) = cx
        .update(|cx| start(services, Some(fx.dir.path().into()), cx))
        .unwrap();
    present(cx, win, "connection-list");
    (fx, files, win, shell)
}

/// Opens the edit dialog through the row's edit button, as a user does.
fn edit_form(
    cx: &mut TestAppContext,
    win: AnyWindowHandle,
    shell: &Entity<dbento_app::shell::Shell>,
    name: &str,
) -> Entity<dbento_app::connection_dialog::ConnectionForm> {
    support::gear(cx, win, name);
    wait_real(cx, "the dialog", |cx| present(cx, win, "conn-name"));
    shell.read_with(cx, |s, _| s.connection_form().cloned().unwrap())
}

fn set(
    cx: &mut TestAppContext,
    win: AnyWindowHandle,
    f: &Entity<dbento_app::connection_dialog::ConnectionForm>,
    field: Field,
    text: &str,
) {
    let state = f.read_with(cx, |f, _| f.input(field).clone());
    cx.update_window(win, |_, w, cx| {
        state.update(cx, |s, cx| s.set_value(text.to_owned(), w, cx))
    })
    .unwrap();
    cx.run_until_parked();
}

struct FailingSet(dbento_core::MemoryStore);
impl SecretStore for FailingSet {
    fn get(&self, k: &SecretKey) -> Result<Option<Secret>, SecretError> {
        self.0.get(k)
    }
    fn set(&self, _: &SecretKey, _: &Secret) -> Result<(), SecretError> {
        Err(SecretError::Failed {
            message: "locked".into(),
        })
    }
    fn delete(&self, k: &SecretKey) -> Result<(), SecretError> {
        self.0.delete(k)
    }
}

#[gpui_kit::test]
fn a_rename_moves_the_secret_and_the_saved_queries(cx: &mut TestAppContext) {
    let store: Arc<dyn SecretStore> = Arc::new(dbento_core::MemoryStore::new());
    let (fx, _files, win, shell) = idle_app(cx, store.clone());
    let dir = fx.ws.dir().to_path_buf();
    store
        .set(&SecretKey::new(&dir, "nopg"), &Secret::new("pw"))
        .unwrap();
    std::fs::create_dir_all(dir.join("queries/nopg")).unwrap();
    std::fs::write(dir.join("queries/nopg/a.sql"), "SELECT 1").unwrap();
    let f = edit_form(cx, win, &shell, "nopg");
    set(cx, win, &f, Field::Name, "pg2");
    click_id(cx, win, "conn-save");
    let new = SecretKey::new(&dir, "pg2");
    wait_real(cx, "the moved secret", |_| {
        store.get(&new).unwrap().is_some()
    });
    assert!(store.get(&SecretKey::new(&dir, "nopg")).unwrap().is_none());
    wait_real(cx, "the moved folder", |_| {
        dir.join("queries/pg2/a.sql").exists()
    });
    assert!(!dir.join("queries/nopg").exists());
    let text = std::fs::read_to_string(dir.join("dbento.toml")).unwrap();
    assert!(
        text.contains("[connections.pg2]") && !text.contains("[connections.nopg]"),
        "{text}"
    );
    assert_eq!(
        shell.read_with(cx, |s, _| s.tabs().len()),
        0,
        "the edit button opened no tab"
    );
}

#[gpui_kit::test]
fn rename_and_delete_are_refused_while_a_tab_is_open(cx: &mut TestAppContext) {
    let o = support::opened(cx, "", "", 0);
    let before = std::fs::read_to_string(o.fx.dir.path().join("dbento.toml")).unwrap();
    support::gear(cx, o.win, "lite");
    support::fill(cx, &o, Field::Name, "lite2");
    click_id(cx, o.win, "conn-save");
    assert_eq!(
        label_of(cx, o.win, "conn-error").as_deref(),
        Some("Close its tabs first")
    );
    click_id(cx, o.win, "conn-delete");
    assert_eq!(
        label_of(cx, o.win, "conn-error").as_deref(),
        Some("Close its tabs first")
    );
    assert!(!present(cx, o.win, "delete-ok"));
    assert_eq!(
        std::fs::read_to_string(o.fx.dir.path().join("dbento.toml")).unwrap(),
        before
    );
}

#[gpui_kit::test]
fn a_failed_apply_moves_no_secret(cx: &mut TestAppContext) {
    let store: Arc<dyn SecretStore> = Arc::new(dbento_core::MemoryStore::new());
    let (fx, _files, win, shell) = idle_app(cx, store.clone());
    let dir = fx.ws.dir().to_path_buf();
    store
        .set(&SecretKey::new(&dir, "nopg"), &Secret::new("pw"))
        .unwrap();
    let f = edit_form(cx, win, &shell, "nopg");
    std::fs::write(
        dir.join("dbento.toml"),
        format!("{}# by hand\n", fx.ws.source_text()),
    )
    .unwrap();
    set(cx, win, &f, Field::Name, "pg2");
    click_id(cx, win, "conn-save");
    assert!(
        label_of(cx, win, "conn-error")
            .unwrap()
            .contains("changed since it was loaded")
    );
    cx.run_until_parked();
    assert!(store.get(&SecretKey::new(&dir, "pg2")).unwrap().is_none());
    assert!(store.get(&SecretKey::new(&dir, "nopg")).unwrap().is_some());
}

#[gpui_kit::test]
fn a_keychain_failure_after_apply_is_shown_and_keeps_the_rename(cx: &mut TestAppContext) {
    let inner = dbento_core::MemoryStore::new();
    let store: Arc<dyn SecretStore> = Arc::new(FailingSet(inner));
    let (fx, _files, win, shell) = idle_app(cx, store.clone());
    let dir = fx.ws.dir().to_path_buf();
    let f = edit_form(cx, win, &shell, "nopg");
    set(cx, win, &f, Field::Name, "pg2");
    set(cx, win, &f, Field::Password, "typed");
    click_id(cx, win, "conn-save");
    wait_real(cx, "the error", |cx| present(cx, win, "confirm-detail"));
    let detail = label_of(cx, win, "confirm-detail").unwrap();
    assert!(
        detail.contains("locked") && detail.contains("pg2"),
        "{detail}"
    );
    assert!(
        std::fs::read_to_string(dir.join("dbento.toml"))
            .unwrap()
            .contains("[connections.pg2]")
    );
}

#[gpui_kit::test]
fn a_folder_failure_after_apply_is_shown_and_keeps_the_rename(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt as _;
    let store: Arc<dyn SecretStore> = Arc::new(dbento_core::MemoryStore::new());
    let (fx, _files, win, shell) = idle_app(cx, store);
    let dir = fx.ws.dir().to_path_buf();
    std::fs::create_dir_all(dir.join("queries/nopg")).unwrap();
    // A read-only queries folder makes the rename inside it fail after apply.
    let mode = |m| std::fs::Permissions::from_mode(m);
    std::fs::set_permissions(dir.join("queries"), mode(0o555)).unwrap();
    let f = edit_form(cx, win, &shell, "nopg");
    set(cx, win, &f, Field::Name, "pg2");
    click_id(cx, win, "conn-save");
    let detail = label_of(cx, win, "confirm-detail");
    std::fs::set_permissions(dir.join("queries"), mode(0o755)).unwrap();
    assert_eq!(
        detail.as_deref(),
        Some("Move queries/nopg to queries/pg2 by hand.")
    );
    assert!(dir.join("queries/nopg").exists());
    assert!(
        std::fs::read_to_string(dir.join("dbento.toml"))
            .unwrap()
            .contains("[connections.pg2]")
    );
}

#[gpui_kit::test]
fn delete_asks_then_removes_the_entry_and_secret_but_keeps_queries(cx: &mut TestAppContext) {
    let store: Arc<dyn SecretStore> = Arc::new(dbento_core::MemoryStore::new());
    let (fx, _files, win, shell) = idle_app(cx, store.clone());
    let dir = fx.ws.dir().to_path_buf();
    let key = SecretKey::new(&dir, "nopg");
    store.set(&key, &Secret::new("pw")).unwrap();
    std::fs::create_dir_all(dir.join("queries/nopg")).unwrap();
    edit_form(cx, win, &shell, "nopg");
    click_id(cx, win, "conn-delete");
    wait_real(cx, "the question", |cx| present(cx, win, "delete-ok"));
    assert!(
        label_of(cx, win, "confirm-detail")
            .unwrap()
            .contains("queries/nopg/")
    );
    click_id(cx, win, "delete-ok");
    wait_real(cx, "the removed secret", |_| {
        store.get(&key).unwrap().is_none()
    });
    assert!(
        !std::fs::read_to_string(dir.join("dbento.toml"))
            .unwrap()
            .contains("nopg")
    );
    assert!(dir.join("queries/nopg").exists());
    assert!(!present(cx, win, connection_id("nopg")));
}

#[gpui_kit::test]
fn the_env_note_names_both_variables(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let fx = support::fixture("", "");
    let files = tempfile::tempdir().unwrap();
    let mut services = support::services(files.path());
    services.env = Arc::new(|v| (v == "DBENTO_SECRET_NOPG").then(|| "pw".to_owned()));
    let (win, shell) = cx
        .update(|cx| start(services, Some(fx.dir.path().into()), cx))
        .unwrap();
    present(cx, win, "connection-list");
    let f = edit_form(cx, win, &shell, "nopg");
    assert!(!present(cx, win, "conn-env-note"));
    set(cx, win, &f, Field::Name, "pg-2");
    assert_eq!(
        label_of(cx, win, "conn-env-note").as_deref(),
        Some("The environment fallback changes from DBENTO_SECRET_NOPG to DBENTO_SECRET_PG_2.")
    );
}

#[gpui_kit::test]
fn no_password_reaches_the_app_files(cx: &mut TestAppContext) {
    let store: Arc<dyn SecretStore> = Arc::new(dbento_core::MemoryStore::new());
    let (fx, files, win, shell) = idle_app(cx, store);
    let f = edit_form(cx, win, &shell, "nopg");
    set(cx, win, &f, Field::Password, "s3cr3t-Pw-9137");
    set(cx, win, &f, Field::Name, "pg2");
    click_id(cx, win, "conn-save");
    cx.run_until_parked();
    let ix = shell.read_with(cx, |s, _| {
        s.workspace()
            .unwrap()
            .connections()
            .iter()
            .position(|c| c.name() == "pg2")
            .unwrap()
    });
    cx.update_window(win, |_, w, cx| {
        shell.update(cx, |s, cx| s.open_connection(ix, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    for root in [fx.dir.path(), files.path()] {
        for entry in walk(root) {
            let bytes = std::fs::read(&entry).unwrap_or_default();
            assert!(
                !String::from_utf8_lossy(&bytes).contains("s3cr3t-Pw-9137"),
                "{}",
                entry.display()
            );
        }
    }
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[gpui_kit::test]
fn a_rename_to_sqlite_removes_the_old_secret(cx: &mut TestAppContext) {
    let store: Arc<dyn SecretStore> = Arc::new(dbento_core::MemoryStore::new());
    let (fx, _files, win, shell) = idle_app(cx, store.clone());
    let dir = fx.ws.dir().to_path_buf();
    let old = SecretKey::new(&dir, "nopg");
    store.set(&old, &Secret::new("pw")).unwrap();
    let f = edit_form(cx, win, &shell, "nopg");
    set(cx, win, &f, Field::Name, "lite2");
    cx.update_window(win, |_, w, cx| w.within("conn-engine").click(1usize, cx))
        .unwrap();
    cx.run_until_parked();
    set(cx, win, &f, Field::Path, "data/app.db");
    click_id(cx, win, "conn-save");
    wait_real(cx, "the removed secret", |_| {
        store.get(&old).unwrap().is_none()
    });
    cx.run_until_parked();
    assert!(store.get(&SecretKey::new(&dir, "lite2")).unwrap().is_none());
    assert!(
        std::fs::read_to_string(dir.join("dbento.toml"))
            .unwrap()
            .contains("[connections.lite2]")
    );
}

#[gpui_kit::test]
fn a_switch_away_from_the_keychain_removes_its_secret(cx: &mut TestAppContext) {
    let store: Arc<dyn SecretStore> = Arc::new(dbento_core::MemoryStore::new());
    let (fx, _files, win, shell) = idle_app(cx, store.clone());
    let key = SecretKey::new(fx.ws.dir(), "nopg");
    store.set(&key, &Secret::new("pw")).unwrap();
    edit_form(cx, win, &shell, "nopg");
    cx.update_window(win, |_, w, cx| w.within("conn-secret").click(2usize, cx))
        .unwrap();
    cx.run_until_parked();
    click_id(cx, win, "conn-save");
    wait_real(cx, "the removed secret", |_| {
        store.get(&key).unwrap().is_none()
    });
}

struct Counting(dbento_core::MemoryStore, std::sync::atomic::AtomicUsize);
impl Counting {
    fn calls(&self) -> usize {
        self.1.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn count(&self) {
        self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
impl SecretStore for Counting {
    fn get(&self, k: &SecretKey) -> Result<Option<Secret>, SecretError> {
        self.count();
        self.0.get(k)
    }
    fn set(&self, k: &SecretKey, s: &Secret) -> Result<(), SecretError> {
        self.count();
        self.0.set(k, s)
    }
    fn delete(&self, k: &SecretKey) -> Result<(), SecretError> {
        self.count();
        self.0.delete(k)
    }
}

#[gpui_kit::test]
fn an_env_source_rename_makes_no_keychain_calls(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let fx = support::fixture("", "");
    let path = fx.dir.path().join("dbento.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{text}secret = {{ env = \"NOPG_PW\" }}\n")).unwrap();
    let files = tempfile::tempdir().unwrap();
    let counting = Arc::new(Counting(
        dbento_core::MemoryStore::new(),
        Default::default(),
    ));
    let mut services = support::services(files.path());
    services.store = counting.clone();
    let (win, shell) = cx
        .update(|cx| start(services, Some(fx.dir.path().into()), cx))
        .unwrap();
    present(cx, win, "connection-list");
    let before = counting.calls();
    let f = edit_form(cx, win, &shell, "nopg");
    set(cx, win, &f, Field::Name, "pg2");
    click_id(cx, win, "conn-save");
    let until = std::time::Instant::now() + std::time::Duration::from_millis(300);
    while std::time::Instant::now() < until {
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains("[connections.pg2]")
    );
    assert_eq!(counting.calls(), before);
}

#[gpui_kit::test]
fn save_refuses_a_form_older_than_the_workspace(cx: &mut TestAppContext) {
    let store: Arc<dyn SecretStore> = Arc::new(dbento_core::MemoryStore::new());
    let (fx, _files, win, shell) = idle_app(cx, store);
    let path = fx.dir.path().join("dbento.toml");
    let f = edit_form(cx, win, &shell, "nopg");
    cx.update_window(win, |_, w, cx| {
        shell.update(cx, |s, cx| s.reload_workspace(w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    set(cx, win, &f, Field::Host, "127.0.0.2");
    let before = std::fs::read_to_string(&path).unwrap();
    click_id(cx, win, "conn-save");
    assert!(
        label_of(cx, win, "conn-error")
            .unwrap()
            .contains("changed since it was loaded")
    );
    assert!(present(cx, win, "conn-reload"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}
