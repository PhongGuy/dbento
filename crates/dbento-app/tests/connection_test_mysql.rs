//! A MySQL connection made in the dialog tests, saves, and keeps its password in the keychain.
mod support;

use dbento_app::app::{AppPaths, Services, start};
use dbento_app::connection_dialog::Field;
use dbento_core::{SecretKey, SecretStore};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use std::sync::Arc;

#[gpui_kit::test]
fn a_new_mysql_connection_tests_saves_and_keeps_its_password_in_the_keychain(
    cx: &mut TestAppContext,
) {
    support::init(cx);
    support::init_tokio(cx);
    let rt = cx.update(|cx| dbento_app::tokio_bridge::handle(cx));
    let _enter = rt.enter();
    let db = rt.block_on(dbento_testkit::mysql_db());
    // Its own login, because the gate password can hide inside the file's other text and a
    // container's is empty; a distinctive password makes "not in the file" mean something.
    let (user, password) = (
        format!("t04_{}", std::process::id()),
        format!("pw-{}", std::time::UNIX_EPOCH.elapsed().unwrap().as_nanos()),
    );
    let host = "'%'";
    // A recycled pid may meet a login a crashed run left behind; the guard drops ours either way.
    rt.block_on(db.scalar(&format!("DROP USER IF EXISTS '{user}'@{host}")));
    let _login = DropLogin {
        db: &db,
        sql: format!("DROP USER IF EXISTS '{user}'@{host}"),
    };
    rt.block_on(db.scalar(&format!(
        "CREATE USER '{user}'@{host} IDENTIFIED BY '{password}'"
    )));
    rt.block_on(db.scalar(&format!(
        "GRANT ALL ON `{}`.* TO '{user}'@{host}",
        db.database()
    )));
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dbento.toml"), "dbento = 1\n").unwrap();
    let files = tempfile::tempdir().unwrap();
    let store: Arc<dyn SecretStore> = Arc::new(dbento_core::MemoryStore::new());
    let services = Services {
        paths: AppPaths::in_dir(files.path()),
        store: store.clone(),
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
    cx.update_window(win, |_, w, cx| w.within("conn-engine").click(2usize, cx))
        .unwrap();
    cx.run_until_parked();
    let f = shell.read_with(cx, |s, _| s.connection_form().cloned().unwrap());
    let set = |cx: &mut TestAppContext, field: Field, text: String| {
        let state = f.read_with(cx, |f, _| f.input(field).clone());
        cx.update_window(win, |_, w, cx| {
            state.update(cx, |s, cx| s.set_value(text, w, cx))
        })
        .unwrap();
        cx.run_until_parked();
    };
    set(cx, Field::Name, "my".into());
    set(cx, Field::Host, db.host().to_owned());
    set(cx, Field::Port, db.port().to_string());
    set(cx, Field::Database, db.database().to_owned());
    set(cx, Field::User, user.clone());
    set(cx, Field::Password, password.clone());
    support::click_id(cx, win, "conn-test");
    support::wait_real(cx, "connected", |cx| {
        support::label_of(cx, win, "conn-test-result").as_deref() == Some("Connected")
    });
    support::click_id(cx, win, "conn-save");
    let key = SecretKey::new(
        shell.read_with(cx, |s, _| s.workspace().unwrap().dir().to_path_buf()),
        "my",
    );
    support::wait_real(cx, "the stored secret", |_| {
        store.get(&key).unwrap().is_some()
    });
    assert_eq!(store.get(&key).unwrap().unwrap().expose(), password);
    let text = std::fs::read_to_string(dir.path().join("dbento.toml")).unwrap();
    assert!(text.contains("engine = \"mysql\""), "{text}");
    assert!(!text.contains(&password), "{text}");
}

/// Drops the test login when the test ends, pass or fail. It runs on its own thread and
/// runtime, as `MyDb::drop` does, because the test's runtime is entered, and never panics.
struct DropLogin<'a> {
    db: &'a dbento_testkit::MyDb,
    sql: String,
}

impl Drop for DropLogin<'_> {
    fn drop(&mut self) {
        let dropped = std::thread::scope(|s| {
            s.spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map(|rt| rt.block_on(self.db.scalar(&self.sql)))
                    .map_err(|e| e.to_string())
            })
            .join()
        });
        match dropped {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => eprintln!("cannot drop the test login: {e}"),
            Err(_) => eprintln!("dropping the test login panicked"),
        }
    }
}
