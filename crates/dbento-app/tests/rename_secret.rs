//! An outside rename carries a connection's saved keychain secrets to its new name.
use crate::support::{self, label_of, present, wait_real};
use dbento_app::app::{EnvLookup, start};
use dbento_app::rename_secret::{Held, adopt_held, probe, rename_keys};
use dbento_core::{
    Edit, MemoryStore, Secret, SecretError, SecretKey, SecretStore, Workspace, secret_renames,
};
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};
use std::path::PathBuf;
use std::sync::Arc;

const LITE: &str = "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"data/app.db\"\n";
const KEY: &str = "tls = \"verify\"\ntls_cert = \"c.crt\"\ntls_key = \"c.key\"\n";
const SECRET: &str = "Qz7vK9xW";

/// Postgres `name` as `user` on the closed port 127.0.0.1:9 reading `db`, with `extra` lines.
fn pg(name: &str, user: &str, db: &str, extra: &str) -> String {
    format!(
        "\n[connections.{name}]\nengine = \"postgres\"\nhost = \"127.0.0.1\"\nport = 9\ndatabase = \"{db}\"\nuser = \"{user}\"\n{extra}"
    )
}

/// A workspace at /w holding `lite` and `body`, parsed without touching the disk.
fn at_w(body: &str) -> Workspace {
    let dir = PathBuf::from("/w");
    Workspace::from_text(
        dir.clone(),
        &dir.join("dbento.toml"),
        &format!("{LITE}{body}"),
    )
    .unwrap()
}

/// A store whose reads fail with `get` when set and whose writes fail when `set_fails`.
struct Broken {
    inner: MemoryStore,
    get: Option<SecretError>,
    set_fails: bool,
}
impl SecretStore for Broken {
    fn get(&self, k: &SecretKey) -> Result<Option<Secret>, SecretError> {
        match &self.get {
            Some(e) => Err(e.clone()),
            None => self.inner.get(k),
        }
    }
    fn set(&self, k: &SecretKey, s: &Secret) -> Result<(), SecretError> {
        if self.set_fails {
            return Err(locked());
        }
        self.inner.set(k, s)
    }
    fn delete(&self, k: &SecretKey) -> Result<(), SecretError> {
        self.inner.delete(k)
    }
}
fn locked() -> SecretError {
    SecretError::Failed {
        message: "locked".into(),
    }
}
fn unavailable() -> SecretError {
    SecretError::Unavailable {
        message: "no keychain".into(),
    }
}

fn keys_ab(extra: &str) -> dbento_app::rename_secret::RenameKeys {
    let (old, new) = (
        at_w(&pg("a", "x", "x", extra)),
        at_w(&pg("b", "x", "x", extra)),
    );
    rename_keys(&old, &new, &secret_renames(&old, &new)[0])
}

#[test]
fn rename_keys_name_each_slot_both_sides_use() {
    let keys = keys_ab(KEY);
    assert_eq!(
        keys.password,
        Some((SecretKey::new("/w", "a"), SecretKey::new("/w", "b")))
    );
    assert_eq!(
        keys.passphrase,
        Some((
            SecretKey::key_passphrase("/w", "a"),
            SecretKey::key_passphrase("/w", "b")
        ))
    );
    assert_eq!(keys_ab("").passphrase, None);
}

#[test]
fn probe_sees_which_slots_hold_and_adopt_moves_only_those() {
    let keys = keys_ab(KEY);
    let store = MemoryStore::new();
    store
        .set(&SecretKey::new("/w", "a"), &Secret::new(SECRET))
        .unwrap();
    let held = probe(&store, &keys).unwrap();
    assert_eq!(
        held,
        Held {
            password: true,
            ..Held::default()
        }
    );
    assert!(held.movable());
    adopt_held(&store, &keys, held).unwrap();
    assert!(store.get(&SecretKey::new("/w", "b")).unwrap().is_some());
    assert!(store.get(&SecretKey::new("/w", "a")).unwrap().is_none());
    assert!(
        store
            .get(&SecretKey::key_passphrase("/w", "b"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_slot_already_filled_under_the_new_name_is_not_movable() {
    let keys = keys_ab("");
    let store = MemoryStore::new();
    store
        .set(&SecretKey::new("/w", "a"), &Secret::new(SECRET))
        .unwrap();
    store
        .set(&SecretKey::new("/w", "b"), &Secret::new("typed"))
        .unwrap();
    let held = probe(&store, &keys).unwrap();
    assert_eq!(
        held,
        Held {
            password: true,
            password_taken: true,
            ..Held::default()
        }
    );
    assert!(!held.movable());
}

#[test]
fn an_unavailable_keychain_probes_as_empty_and_a_failing_one_is_an_error() {
    let keys = keys_ab(KEY);
    let gone = Broken {
        inner: MemoryStore::new(),
        get: Some(unavailable()),
        set_fails: false,
    };
    assert_eq!(probe(&gone, &keys).unwrap(), Held::default());
    let failing = Broken {
        inner: MemoryStore::new(),
        get: Some(locked()),
        set_fails: false,
    };
    assert!(probe(&failing, &keys).is_err());
}

fn memory() -> Arc<dyn SecretStore> {
    Arc::new(MemoryStore::new())
}

fn no_env() -> EnvLookup {
    Arc::new(|_| None)
}

/// An exact rename of `zw` (sorted after every name under test) rides along in an edit, so
/// its move is the positive sign that the reload's renames were all handled.
fn zw(name: &str) -> String {
    pg(name, "w", "x", "")
}

struct App {
    dir: PathBuf,
    _fx: support::Fx,
    _files: tempfile::TempDir,
    win: AnyWindowHandle,
    shell: Entity<dbento_app::shell::Shell>,
    store: Arc<dyn SecretStore>,
}

impl App {
    fn password(&self, name: &str) -> SecretKey {
        SecretKey::new(&self.dir, name)
    }
    fn passphrase(&self, name: &str) -> SecretKey {
        SecretKey::key_passphrase(&self.dir, name)
    }
    fn holds(&self, k: &SecretKey) -> bool {
        self.store.get(k).unwrap().is_some()
    }
    fn save(&self, k: &SecretKey) {
        self.store.set(k, &Secret::new(SECRET)).unwrap();
    }
    /// Runs the reload a watcher event would, and lets it finish.
    fn reload(&self, cx: &mut TestAppContext) {
        let shell = self.shell.clone();
        cx.update_window(self.win, |_, w, cx| {
            shell.update(cx, |s, cx| s.workspace_files_changed(w, cx))
        })
        .unwrap();
        support::settle_reload(cx, &self.shell);
    }
    /// Rewrites dbento.toml as `lite` then `body` from outside the app, then reloads.
    fn outside(&self, cx: &mut TestAppContext, body: &str) {
        std::fs::write(self.dir.join("dbento.toml"), format!("{LITE}{body}")).unwrap();
        self.reload(cx);
    }
    /// Waits for the ride-along `zw` -> `zw2` move.
    fn witnessed(&self, cx: &mut TestAppContext) {
        wait_real(cx, "the ride-along move", |_| {
            self.holds(&self.password("zw2"))
        });
    }
}

/// The app on a workspace of `lite` and `body` over `store` and `env`, with no tab open; a
/// `zw` in `body` gets a saved password.
fn app(cx: &mut TestAppContext, body: &str, store: Arc<dyn SecretStore>, env: EnvLookup) -> App {
    support::init(cx);
    support::init_tokio(cx);
    let fx = support::fixture("", "");
    let dir = fx.ws.dir().to_path_buf();
    std::fs::write(dir.join("dbento.toml"), format!("{LITE}{body}")).unwrap();
    let files = tempfile::tempdir().unwrap();
    let mut services = support::services(files.path());
    services.store = Arc::clone(&store);
    services.env = env;
    let (win, shell) = cx
        .update(|cx| start(services, Some(dir.clone()), cx))
        .unwrap();
    present(cx, win, "connection-list");
    let a = App {
        dir,
        _fx: fx,
        _files: files,
        win,
        shell,
        store,
    };
    if body.contains("[connections.zw]") {
        a.save(&a.password("zw"));
    }
    a
}

#[gpui_kit::test]
fn an_exact_outside_rename_moves_both_slots_without_asking(cx: &mut TestAppContext) {
    let a = app(cx, &pg("nopg", "x", "x", KEY), memory(), no_env());
    a.save(&a.password("nopg"));
    a.save(&a.passphrase("nopg"));
    a.outside(cx, &pg("pg2", "x", "x", KEY));
    wait_real(cx, "the moved secrets", |_| {
        a.holds(&a.password("pg2")) && a.holds(&a.passphrase("pg2"))
    });
    assert!(!a.holds(&a.password("nopg")) && !a.holds(&a.passphrase("nopg")));
    assert!(
        !present(cx, a.win, "confirm-dialog"),
        "an exact rename asks nothing"
    );
}

#[gpui_kit::test]
fn a_secret_already_under_the_new_name_wins(cx: &mut TestAppContext) {
    let a = app(
        cx,
        &format!("{}{}", pg("nopg", "x", "x", ""), zw("zw")),
        memory(),
        no_env(),
    );
    a.save(&a.password("nopg"));
    a.store
        .set(&a.password("pg2"), &Secret::new("typed"))
        .unwrap();
    a.outside(cx, &format!("{}{}", pg("pg2", "x", "x", ""), zw("zw2")));
    a.witnessed(cx);
    assert_eq!(
        a.store.get(&a.password("pg2")).unwrap().unwrap().expose(),
        "typed"
    );
    assert!(a.holds(&a.password("nopg")), "the old entry stays");
    assert!(
        !present(cx, a.win, "confirm-dialog"),
        "a taken name is silent"
    );
}

#[gpui_kit::test]
fn another_login_moves_nothing_and_asks_nothing(cx: &mut TestAppContext) {
    let a = app(
        cx,
        &format!("{}{}", pg("nopg", "x", "x", ""), zw("zw")),
        memory(),
        no_env(),
    );
    a.save(&a.password("nopg"));
    a.outside(cx, &format!("{}{}", pg("pg2", "y", "x", ""), zw("zw2")));
    a.witnessed(cx);
    assert!(!present(cx, a.win, "confirm-dialog"));
    assert!(a.holds(&a.password("nopg")) && !a.holds(&a.password("pg2")));
}

#[gpui_kit::test]
fn the_apps_own_rename_is_not_taken_for_an_outside_one(cx: &mut TestAppContext) {
    let a = app(
        cx,
        &format!("{}{}", pg("nopg", "x", "x", ""), zw("zw")),
        memory(),
        no_env(),
    );
    a.save(&a.password("nopg"));
    let spec = a.shell.read_with(cx, |s, _| {
        s.workspace().unwrap().connection_spec("nopg").unwrap()
    });
    let edit = Edit::PutConnection {
        previous: Some("nopg".into()),
        name: "pg2".into(),
        spec,
    };
    let shell = a.shell.clone();
    cx.update_window(a.win, |_, w, cx| {
        shell.update(cx, |s, cx| assert!(s.apply_edit(edit, w, cx), "written"))
    })
    .unwrap();
    // The outside edit keeps the app's pg2 and renames only zw.
    a.outside(cx, &format!("{}{}", pg("pg2", "x", "x", ""), zw("zw2")));
    a.witnessed(cx);
    assert!(!present(cx, a.win, "confirm-dialog"));
    assert!(
        a.holds(&a.password("nopg")),
        "the app's own rename was not reread as outside"
    );
}

fn says_not_moved(cx: &mut TestAppContext, a: &App) {
    wait_real(cx, "the error", |cx| present(cx, a.win, "save-error-ok"));
    let detail = label_of(cx, a.win, "confirm-detail").unwrap();
    assert!(
        detail.ends_with("Enter the password again for pg2."),
        "{detail}"
    );
    assert!(
        !detail.contains(SECRET),
        "the dialog never shows the secret"
    );
}

#[gpui_kit::test]
fn a_failed_move_says_so_and_keeps_the_old_entry(cx: &mut TestAppContext) {
    let store = Arc::new(Broken {
        inner: MemoryStore::new(),
        get: None,
        set_fails: true,
    });
    let a = app(
        cx,
        &pg("nopg", "x", "x", ""),
        Arc::clone(&store) as Arc<dyn SecretStore>,
        no_env(),
    );
    store
        .inner
        .set(&a.password("nopg"), &Secret::new(SECRET))
        .unwrap();
    a.outside(cx, &pg("pg2", "x", "x", ""));
    says_not_moved(cx, &a);
    assert!(store.inner.get(&a.password("nopg")).unwrap().is_some());
}

#[gpui_kit::test]
fn a_failed_probe_says_so(cx: &mut TestAppContext) {
    let store = Arc::new(Broken {
        inner: MemoryStore::new(),
        get: Some(locked()),
        set_fails: false,
    });
    let a = app(cx, &pg("nopg", "x", "x", ""), store, no_env());
    a.outside(cx, &pg("pg2", "x", "x", ""));
    says_not_moved(cx, &a);
}
