//! An outside rename carries a connection's saved keychain secrets to its new name.
use crate::support::{self, click_id, label_of, present, wait_real};
use dbento_app::app::{AppPaths, EnvLookup, SavedState, start};
use dbento_app::persist::{load_json, save_json};
use dbento_app::rename_secret::not_moved_text;
use dbento_app::rename_secret::{
    Held, RenameKeys, adopt_held, env_lines, env_notice, move_ask, probe, rename_keys, stays_line,
    still_renamed,
};
use dbento_core::{
    Edit, MemoryStore, Secret, SecretError, SecretKey, SecretRename, SecretStore, SshHome,
    Workspace, secret_renames,
};
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};
use std::path::{Path, PathBuf};
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
    rename_keys(&old, &new, &secret_renames(&old, &new)[0], None)
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

#[test]
fn the_question_names_both_targets_and_the_changed_keys() {
    let (old, new) = (at_w(&pg("a", "x", "x", KEY)), at_w(&pg("b", "x", "y", KEY)));
    let r = secret_renames(&old, &new).remove(0);
    let ask = move_ask(
        &r,
        Held {
            password: true,
            ..Held::default()
        },
    );
    assert_eq!(ask.title.to_string(), "Move the saved secret to b?");
    assert_eq!(
        ask.detail.as_ref().map(|d| d.to_string()).as_deref(),
        Some(
            "a was renamed to b in dbento.toml, and these settings changed too: database. Saved password for: x@127.0.0.1:9/x. Now: x@127.0.0.1:9/y. Move it only if this is the same login."
        )
    );
    let ids: Vec<_> = ask.choices.iter().map(|c| c.id).collect();
    assert_eq!(ids, vec!["rename-secret-keep", "rename-secret-move"]);
    assert_eq!(ask.choices[0].label.to_string(), "Keep under a");
    assert_eq!((ask.default, ask.cancel), (0, 0));
    let both = move_ask(
        &r,
        Held {
            password: true,
            passphrase: true,
            ..Held::default()
        },
    );
    assert!(
        both.detail
            .unwrap()
            .contains("Saved password and key passphrase for:")
    );
    let phrase = move_ask(
        &r,
        Held {
            password: true,
            passphrase: true,
            password_taken: true,
            ..Held::default()
        },
    );
    assert!(phrase.detail.unwrap().contains("Saved key passphrase for:"));
}

#[test]
fn a_move_holds_only_while_the_file_still_says_what_was_asked() {
    let (old, new) = (at_w(&pg("a", "x", "x", "")), at_w(&pg("b", "x", "y", "")));
    let r = secret_renames(&old, &new).remove(0);
    let asked = new.declared("b").unwrap();
    let w = Path::new("/w");
    assert!(still_renamed(&new, w, &r, asked));
    assert!(
        !still_renamed(&at_w(&pg("b", "x", "z", "")), w, &r, asked),
        "changed again"
    );
    let back = format!("{}{}", pg("a", "x", "x", ""), pg("b", "x", "y", ""));
    assert!(
        !still_renamed(&at_w(&back), w, &r, asked),
        "the old name is back"
    );
    assert!(
        !still_renamed(&at_w(""), w, &r, asked),
        "the new name is gone"
    );
    assert!(
        !still_renamed(&new, Path::new("/v"), &r, asked),
        "another workspace"
    );
}

fn asked(cx: &mut TestAppContext, a: &App, from: &str) {
    let lead = format!("{from} was renamed");
    wait_real(cx, "the question", |cx| {
        label_of(cx, a.win, "confirm-detail").is_some_and(|d| d.starts_with(&lead))
    });
    assert!(
        !label_of(cx, a.win, "confirm-detail")
            .unwrap()
            .contains(SECRET),
        "the question never shows the secret"
    );
}

fn renamed_to_y(a: &App, cx: &mut TestAppContext) {
    a.outside(cx, &format!("{}{}", pg("pg2", "x", "y", ""), zw("zw2")));
}

fn with_zw(body: &str) -> String {
    format!("{body}{}", zw("zw"))
}

#[gpui_kit::test]
fn keep_leaves_the_secret_under_the_old_name(cx: &mut TestAppContext) {
    let a = app(cx, &with_zw(&pg("nopg", "x", "x", "")), memory(), no_env());
    a.save(&a.password("nopg"));
    renamed_to_y(&a, cx);
    asked(cx, &a, "nopg");
    click_id(cx, a.win, "rename-secret-keep");
    a.witnessed(cx);
    assert!(a.holds(&a.password("nopg")) && !a.holds(&a.password("pg2")));
}

#[gpui_kit::test]
fn move_moves_it(cx: &mut TestAppContext) {
    let a = app(cx, &pg("nopg", "x", "x", ""), memory(), no_env());
    a.save(&a.password("nopg"));
    a.outside(cx, &pg("pg2", "x", "y", ""));
    asked(cx, &a, "nopg");
    click_id(cx, a.win, "rename-secret-move");
    wait_real(cx, "the moved secret", |_| a.holds(&a.password("pg2")));
    assert!(!a.holds(&a.password("nopg")));
}

#[gpui_kit::test]
fn a_file_change_while_asking_drops_the_move(cx: &mut TestAppContext) {
    let a = app(cx, &with_zw(&pg("nopg", "x", "x", "")), memory(), no_env());
    a.save(&a.password("nopg"));
    renamed_to_y(&a, cx);
    asked(cx, &a, "nopg");
    a.outside(cx, &format!("{}{}", pg("pg2", "x", "z", ""), zw("zw2")));
    click_id(cx, a.win, "rename-secret-move");
    a.witnessed(cx);
    assert!(a.holds(&a.password("nopg")) && !a.holds(&a.password("pg2")));
}

#[gpui_kit::test]
fn a_new_name_that_already_has_every_secret_asks_nothing(cx: &mut TestAppContext) {
    let a = app(cx, &with_zw(&pg("nopg", "x", "x", "")), memory(), no_env());
    a.save(&a.password("nopg"));
    a.store
        .set(&a.password("pg2"), &Secret::new("typed"))
        .unwrap();
    renamed_to_y(&a, cx);
    a.witnessed(cx);
    assert!(!present(cx, a.win, "confirm-dialog"));
    assert_eq!(
        a.store.get(&a.password("pg2")).unwrap().unwrap().expose(),
        "typed"
    );
}

#[gpui_kit::test]
fn two_questions_come_one_at_a_time_in_name_order(cx: &mut TestAppContext) {
    let before = format!("{}{}", pg("pa", "x", "x", ""), pg("pb", "z", "x", ""));
    let a = app(cx, &before, memory(), no_env());
    a.save(&a.password("pa"));
    a.save(&a.password("pb"));
    a.outside(
        cx,
        &format!("{}{}", pg("pa2", "x", "y", ""), pg("pb2", "z", "y", "")),
    );
    asked(cx, &a, "pa");
    click_id(cx, a.win, "rename-secret-keep");
    asked(cx, &a, "pb");
    click_id(cx, a.win, "rename-secret-move");
    wait_real(cx, "the second move", |_| a.holds(&a.password("pb2")));
    assert!(a.holds(&a.password("pa")) && !a.holds(&a.password("pa2")));
}

fn env_with(vars: &'static [&'static str]) -> EnvLookup {
    Arc::new(move |v: &str| vars.contains(&v).then(|| SECRET.to_owned()))
}

#[test]
fn env_lines_name_the_variables_to_rename() {
    let (old, new) = (at_w(&pg("a", "x", "x", KEY)), at_w(&pg("b", "x", "x", KEY)));
    let r = secret_renames(&old, &new).remove(0);
    let none = Held::default();
    let a = env_with(&["DBENTO_SECRET_A"]);
    assert_eq!(
        env_lines(&r, none, &a),
        vec!["DBENTO_SECRET_A -> DBENTO_SECRET_B".to_owned()]
    );
    assert!(
        env_lines(
            &r,
            Held {
                password: true,
                ..none
            },
            &a
        )
        .is_empty(),
        "the keychain has it"
    );
    assert!(
        env_lines(
            &r,
            Held {
                password_taken: true,
                ..none
            },
            &a
        )
        .is_empty(),
        "the new name has one"
    );
    assert!(env_lines(&r, none, &env_with(&["DBENTO_SECRET_A", "DBENTO_SECRET_B"])).is_empty());
    assert_eq!(
        env_lines(&r, none, &env_with(&["DBENTO_KEY_PASSPHRASE_A"])),
        vec!["DBENTO_KEY_PASSPHRASE_A -> DBENTO_KEY_PASSPHRASE_B".to_owned()]
    );
    let (old, new) = (at_w(&pg("a", "x", "x", "")), at_w(&pg("b", "x", "y", "")));
    let r = secret_renames(&old, &new).remove(0);
    assert_eq!(
        env_lines(&r, none, &a),
        vec!["DBENTO_SECRET_A -> DBENTO_SECRET_B: saved for x@127.0.0.1:9/x, now x@127.0.0.1:9/y (database changed); rename it only if this is the same login".to_owned()]
    );
}

#[test]
fn the_notice_lists_every_line_after_the_reason() {
    let ask = env_notice(&["L1".to_owned(), "L2".to_owned()]);
    assert_eq!(ask.title.to_string(), "Rename the environment variables");
    assert_eq!(
        ask.detail.map(|d| d.to_string()).as_deref(),
        Some(
            "These secrets come from environment variables, which Dbento cannot rename. Rename them and restart Dbento.\nL1\nL2"
        )
    );
    let ids: Vec<_> = ask.choices.iter().map(|c| c.id).collect();
    assert_eq!(ids, vec!["rename-env-ok"]);
}

fn shows_env_notice(cx: &mut TestAppContext, a: &App) {
    wait_real(cx, "the notice", |cx| present(cx, a.win, "rename-env-ok"));
    let detail = label_of(cx, a.win, "confirm-detail").unwrap();
    assert!(
        detail.contains("DBENTO_SECRET_NOPG -> DBENTO_SECRET_PG2"),
        "{detail}"
    );
    assert!(
        !detail.contains(SECRET),
        "a variable's value is never shown"
    );
}

#[gpui_kit::test]
fn an_env_only_secret_shows_the_notice(cx: &mut TestAppContext) {
    let a = app(
        cx,
        &pg("nopg", "x", "x", ""),
        memory(),
        env_with(&["DBENTO_SECRET_NOPG"]),
    );
    a.outside(cx, &pg("pg2", "x", "x", ""));
    shows_env_notice(cx, &a);
    click_id(cx, a.win, "rename-env-ok");
    assert!(!a.holds(&a.password("pg2")));
}

#[gpui_kit::test]
fn without_a_keychain_the_notice_shows_and_no_error(cx: &mut TestAppContext) {
    let store = Arc::new(Broken {
        inner: MemoryStore::new(),
        get: Some(unavailable()),
        set_fails: false,
    });
    let a = app(
        cx,
        &pg("nopg", "x", "x", ""),
        store,
        env_with(&["DBENTO_SECRET_NOPG"]),
    );
    a.outside(cx, &pg("pg2", "x", "x", ""));
    shows_env_notice(cx, &a);
    assert!(
        !present(cx, a.win, "save-error-ok"),
        "an absent keychain is no error"
    );
}

#[gpui_kit::test]
fn the_notice_comes_after_the_questions(cx: &mut TestAppContext) {
    let before = format!("{}{}", pg("pa", "x", "x", ""), pg("pb", "z", "x", ""));
    let a = app(cx, &before, memory(), env_with(&["DBENTO_SECRET_PB"]));
    a.save(&a.password("pa"));
    a.outside(
        cx,
        &format!("{}{}", pg("pa2", "x", "y", ""), pg("pb2", "z", "x", "")),
    );
    asked(cx, &a, "pa");
    assert!(!present(cx, a.win, "rename-env-ok"));
    click_id(cx, a.win, "rename-secret-keep");
    wait_real(cx, "the notice", |cx| present(cx, a.win, "rename-env-ok"));
}

#[gpui_kit::test]
fn an_exact_rename_waiting_behind_a_question_rechecks_the_file(cx: &mut TestAppContext) {
    let before = format!("{}{}", pg("a", "x", "x", ""), pg("b", "v", "x", ""));
    let a = app(cx, &with_zw(&before), memory(), no_env());
    a.save(&a.password("a"));
    a.save(&a.password("b"));
    let y = pg("y", "v", "x", "");
    a.outside(cx, &format!("{}{y}{}", pg("a2", "x", "y", ""), zw("zw2")));
    asked(cx, &a, "a");
    // While the question is open, y moves to another host.
    let elsewhere = y.replace("127.0.0.1", "127.0.0.2");
    a.outside(
        cx,
        &format!("{}{elsewhere}{}", pg("a2", "x", "y", ""), zw("zw2")),
    );
    click_id(cx, a.win, "rename-secret-keep");
    a.witnessed(cx);
    assert!(
        !a.holds(&a.password("y")),
        "b's password never reaches the new host"
    );
    assert!(a.holds(&a.password("b")), "b's entry stays");
}

/// A dbento.toml that does not parse.
const BROKEN: &str = "dbento = 1\n[connections.\n";

impl App {
    /// Opens `dir` in this window through the path every start, switch and recent takes.
    fn open(&self, cx: &mut TestAppContext, dir: &Path) {
        let (shell, dir) = (self.shell.clone(), dir.to_path_buf());
        cx.update_window(self.win, |_, w, cx| {
            shell.update(cx, |s, cx| s.open_workspace(dir, w, cx))
        })
        .unwrap();
        cx.run_until_parked();
    }
    /// Opens another workspace, so this one's file changes with nothing watching it, and
    /// waits until state.json holds the other workspace's session first and this one's
    /// snapshot: every write the switch queued has landed and this shell has nothing pending.
    fn away(&self, cx: &mut TestAppContext) -> support::Fx {
        let other = support::fixture("", "");
        self.open(cx, other.dir.path());
        let there = other.ws.dir().to_path_buf();
        wait_real(cx, "the switch's state write", |_| {
            let saved = load_json::<SavedState>(&self.state()).value;
            saved.sessions.first().is_some_and(|s| s.workspace == there)
                && saved.session(&self.dir).is_some_and(|s| s.seen.is_some())
        });
        other
    }
    /// Rewrites dbento.toml as `lite` then `body` while another workspace is open, then
    /// opens this one again.
    fn while_closed(&self, cx: &mut TestAppContext, body: &str) {
        let _other = self.away(cx);
        std::fs::write(self.dir.join("dbento.toml"), format!("{LITE}{body}")).unwrap();
        self.open(cx, &self.dir);
    }
    fn state(&self) -> PathBuf {
        AppPaths::in_dir(self._files.path()).state
    }
    /// The snapshot state.json holds for this workspace.
    fn seen_on_disk(&self) -> Option<String> {
        load_json::<SavedState>(&self.state())
            .value
            .session(&self.dir)
            .and_then(|s| s.seen.clone())
    }
    /// Replaces this workspace's snapshot in state.json, as an older app or damage would.
    fn put_seen_on_disk(&self, seen: Option<&str>) {
        let mut saved = load_json::<SavedState>(&self.state()).value;
        saved
            .sessions
            .iter_mut()
            .find(|s| s.workspace == self.dir)
            .expect("the first open saved a session")
            .seen = seen.map(str::to_owned);
        save_json(&self.state(), &saved).unwrap();
    }
    /// A second start on the same files, keychain and workspace: the next launch.
    fn restart(&self, cx: &mut TestAppContext) -> AnyWindowHandle {
        let mut services = support::services(self._files.path());
        services.store = Arc::clone(&self.store);
        let (win, _shell) = cx
            .update(|cx| start(services, Some(self.dir.clone()), cx))
            .unwrap();
        present(cx, win, "connection-list");
        win
    }
}

#[gpui_kit::test]
fn closed_an_exact_rename_moves_both_slots_on_the_next_open(cx: &mut TestAppContext) {
    let a = app(cx, &pg("nopg", "x", "x", KEY), memory(), no_env());
    a.save(&a.password("nopg"));
    a.save(&a.passphrase("nopg"));
    a.while_closed(cx, &pg("pg2", "x", "x", KEY));
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
fn closed_a_same_login_rename_asks_and_a_reopen_asks_nothing(cx: &mut TestAppContext) {
    let a = app(cx, &pg("nopg", "x", "x", ""), memory(), no_env());
    a.save(&a.password("nopg"));
    a.while_closed(cx, &pg("pg2", "x", "y", ""));
    asked(cx, &a, "nopg");
    click_id(cx, a.win, "rename-secret-move");
    wait_real(cx, "the moved secret", |_| a.holds(&a.password("pg2")));
    assert!(!a.holds(&a.password("nopg")));
    a.while_closed(cx, &pg("pg2", "x", "y", ""));
    assert!(!present(cx, a.win, "confirm-dialog"));
    assert!(a.holds(&a.password("pg2")));
}

#[gpui_kit::test]
fn closed_a_kept_secret_is_not_asked_about_again(cx: &mut TestAppContext) {
    let a = app(cx, &with_zw(&pg("nopg", "x", "x", "")), memory(), no_env());
    a.save(&a.password("nopg"));
    a.while_closed(cx, &with_zw(&pg("pg2", "x", "y", "")));
    asked(cx, &a, "nopg");
    click_id(cx, a.win, "rename-secret-keep");
    // The open already remembered pg2, so only the zw rename is found this time.
    a.while_closed(cx, &format!("{}{}", pg("pg2", "x", "y", ""), zw("zw2")));
    a.witnessed(cx);
    assert!(!present(cx, a.win, "confirm-dialog"));
    assert!(a.holds(&a.password("nopg")) && !a.holds(&a.password("pg2")));
}

#[gpui_kit::test]
fn closed_another_host_moves_nothing(cx: &mut TestAppContext) {
    let a = app(cx, &with_zw(&pg("nopg", "x", "x", "")), memory(), no_env());
    a.save(&a.password("nopg"));
    let elsewhere = pg("pg2", "x", "x", "").replace("127.0.0.1", "127.0.0.2");
    a.while_closed(cx, &format!("{elsewhere}{}", zw("zw2")));
    a.witnessed(cx);
    assert!(!present(cx, a.win, "confirm-dialog"));
    assert!(a.holds(&a.password("nopg")) && !a.holds(&a.password("pg2")));
}

/// Leaves this workspace, sets its stored snapshot to `seen`, renames nopg to pg2 and zw to
/// zw2 exactly, and starts the app again on it; answers the new window and the workspace
/// the first window moved to, which must outlive the test. The first window stays open but
/// quiescent: `away` waited for its last state write, and nothing after it changes its state,
/// so no queued change can replay over the edited snapshot.
fn restarted_with(
    cx: &mut TestAppContext,
    a: &App,
    seen: Option<&str>,
) -> (AnyWindowHandle, support::Fx) {
    let other = a.away(cx);
    a.put_seen_on_disk(seen);
    let body = format!("{LITE}{}{}", pg("pg2", "x", "x", ""), zw("zw2"));
    std::fs::write(a.dir.join("dbento.toml"), body).unwrap();
    (a.restart(cx), other)
}

fn nothing_moved_and_snapshot_rewritten(cx: &mut TestAppContext, a: &App, win: AnyWindowHandle) {
    wait_real(cx, "the new snapshot", |_| {
        a.seen_on_disk()
            .is_some_and(|s| s.contains("[connections.pg2]"))
    });
    assert!(!present(cx, win, "confirm-dialog"), "nothing is asked");
    assert!(
        a.holds(&a.password("nopg")) && !a.holds(&a.password("pg2")),
        "nopg's password stays"
    );
    assert!(
        a.holds(&a.password("zw")) && !a.holds(&a.password("zw2")),
        "no compare ran"
    );
}

#[gpui_kit::test]
fn closed_without_a_snapshot_nothing_moves_and_one_is_written(cx: &mut TestAppContext) {
    let a = app(cx, &with_zw(&pg("nopg", "x", "x", "")), memory(), no_env());
    a.save(&a.password("nopg"));
    let (win, _other) = restarted_with(cx, &a, None);
    nothing_moved_and_snapshot_rewritten(cx, &a, win);
}

#[gpui_kit::test]
fn closed_a_corrupt_snapshot_moves_nothing_and_is_rewritten(cx: &mut TestAppContext) {
    let a = app(cx, &with_zw(&pg("nopg", "x", "x", "")), memory(), no_env());
    a.save(&a.password("nopg"));
    let (win, _other) = restarted_with(cx, &a, Some(BROKEN));
    nothing_moved_and_snapshot_rewritten(cx, &a, win);
}

#[gpui_kit::test]
fn closed_an_invalid_file_at_open_keeps_the_old_snapshot(cx: &mut TestAppContext) {
    let a = app(cx, &with_zw(&pg("nopg", "x", "x", "")), memory(), no_env());
    a.save(&a.password("nopg"));
    let _other = a.away(cx);
    let before = a.seen_on_disk().expect("away waited for the snapshot");
    std::fs::write(a.dir.join("dbento.toml"), BROKEN).unwrap();
    a.open(cx, &a.dir);
    assert!(
        a.shell.read_with(cx, |s, _| s.workspace().is_none()),
        "the open failed"
    );
    // A failed open saves no session; any last_workspace write it queues replays onto the
    // file and leaves the sessions as they are, and writes are atomic renames.
    cx.run_until_parked();
    assert_eq!(a.seen_on_disk(), Some(before));
    a.while_closed(cx, &format!("{}{}", pg("pg2", "x", "x", ""), zw("zw2")));
    wait_real(cx, "the moved secret", |_| a.holds(&a.password("pg2")));
    assert!(!a.holds(&a.password("nopg")));
}

#[gpui_kit::test]
fn closed_a_folder_opened_by_another_path_still_finds_its_snapshot(cx: &mut TestAppContext) {
    let a = app(cx, &pg("nopg", "x", "x", ""), memory(), no_env());
    a.save(&a.password("nopg"));
    let _other = a.away(cx);
    std::fs::write(
        a.dir.join("dbento.toml"),
        format!("{LITE}{}", pg("pg2", "x", "x", "")),
    )
    .unwrap();
    // The fixture's data folder exists, so this path canonicalizes to the workspace.
    a.open(cx, &a.dir.join("data").join(".."));
    wait_real(cx, "the moved secret", |_| a.holds(&a.password("pg2")));
}

fn tun(name: &str, ssh_user: &str) -> String {
    format!(
        "{}[connections.{name}.ssh]\nhost = \"jump\"\nuser = \"{ssh_user}\"\n",
        pg(name, "x", "x", "")
    )
}

fn ssh_keys(a_user: &str, b_user: &str, home: Option<&SshHome>) -> (SecretRename, RenameKeys) {
    let (old, new) = (at_w(&tun("a", a_user)), at_w(&tun("b", b_user)));
    let r = secret_renames(&old, &new).remove(0);
    let keys = rename_keys(&old, &new, &r, home);
    (r, keys)
}

fn temp_home() -> (tempfile::TempDir, SshHome) {
    let dir = tempfile::tempdir().unwrap();
    let home = SshHome::new(dir.path(), "tester");
    (dir, home)
}

#[test]
fn rename_keys_pair_the_ssh_slots_on_one_login() {
    let (_d, home) = temp_home();
    let (_, keys) = ssh_keys("ops", "ops", Some(&home));
    assert_eq!(
        keys.ssh_password,
        Some((
            SecretKey::ssh_password("/w", "a", "ops@jump:22"),
            SecretKey::ssh_password("/w", "b", "ops@jump:22")
        ))
    );
    assert_eq!(
        keys.ssh_passphrase,
        Some((
            SecretKey::ssh_key_passphrase("/w", "a"),
            SecretKey::ssh_key_passphrase("/w", "b")
        ))
    );
    assert_eq!(keys.ssh_password_stays, None);
}

#[test]
fn another_ssh_user_leaves_the_password_with_the_reason() {
    let (_d, home) = temp_home();
    let (_, keys) = ssh_keys("ops", "dev", Some(&home));
    assert_eq!(keys.ssh_password, None);
    let (key, why) = keys.ssh_password_stays.clone().unwrap();
    assert_eq!(key, SecretKey::ssh_password("/w", "a", "ops@jump:22"));
    assert!(
        why.contains("ops@jump:22") && why.contains("dev@jump:22"),
        "{why}"
    );
    assert!(
        keys.ssh_passphrase.is_some(),
        "the passphrase is not pinned"
    );
}

#[test]
fn without_a_home_only_the_passphrase_pairs() {
    let (_, keys) = ssh_keys("ops", "ops", None);
    assert_eq!((keys.ssh_password, keys.ssh_password_stays), (None, None));
    assert!(keys.ssh_passphrase.is_some(), "passphrase");
}

#[test]
fn probe_and_adopt_carry_the_ssh_slots() {
    let (_d, home) = temp_home();
    let (_, keys) = ssh_keys("ops", "ops", Some(&home));
    let store = MemoryStore::new();
    let (pw_a, pw_b) = keys.ssh_password.clone().unwrap();
    let (pp_a, pp_b) = keys.ssh_passphrase.clone().unwrap();
    store.set(&pw_a, &Secret::new(SECRET)).unwrap();
    store.set(&pp_a, &Secret::new(SECRET)).unwrap();
    let held = probe(&store, &keys).unwrap();
    assert!(
        held.ssh_password && held.ssh_passphrase && held.movable(),
        "{held:?}"
    );
    adopt_held(&store, &keys, held).unwrap();
    assert!(store.get(&pw_b).unwrap().is_some() && store.get(&pp_b).unwrap().is_some());
    assert!(store.get(&pw_a).unwrap().is_none() && store.get(&pp_a).unwrap().is_none());
}

#[test]
fn a_password_left_behind_is_said_once() {
    let (_d, home) = temp_home();
    let (r, keys) = ssh_keys("ops", "dev", Some(&home));
    let store = MemoryStore::new();
    let (old, _) = keys.ssh_password_stays.clone().unwrap();
    store.set(&old, &Secret::new(SECRET)).unwrap();
    let held = probe(&store, &keys).unwrap();
    assert!(held.ssh_password_left, "{held:?}");
    let line = stays_line(&r, &keys, held).unwrap();
    assert!(
        line.starts_with("The SSH password saved for a stays under its old entry")
            && line.ends_with("Enter it again for b in Edit connection."),
        "{line}"
    );
    assert!(!line.contains(SECRET), "never the secret");
    assert!(
        stays_line(&r, &keys, Held::default()).is_none(),
        "nothing saved, nothing said"
    );
}

#[test]
fn the_question_names_the_ssh_slots_it_moves() {
    let (old, new) = (at_w(&pg("a", "x", "x", KEY)), at_w(&pg("b", "x", "y", KEY)));
    let r = secret_renames(&old, &new).remove(0);
    let says = |held: Held| move_ask(&r, held).detail.unwrap().to_string();
    assert!(
        says(Held {
            ssh_password: true,
            ..Held::default()
        })
        .contains("Saved SSH password for:")
    );
    assert!(
        says(Held {
            password: true,
            ssh_passphrase: true,
            ..Held::default()
        })
        .contains("Saved password and SSH key passphrase for:")
    );
    assert!(
        says(Held {
            password: true,
            passphrase: true,
            ssh_password: true,
            ..Held::default()
        })
        .contains("Saved password, key passphrase and SSH password for:")
    );
}

#[test]
fn env_lines_name_the_ssh_variables() {
    let (r, _) = ssh_keys("ops", "ops", None);
    let env = env_with(&["DBENTO_SSH_PASSWORD_A", "DBENTO_SSH_KEY_PASSPHRASE_A"]);
    assert_eq!(
        env_lines(&r, Held::default(), &env),
        vec![
            "DBENTO_SSH_PASSWORD_A -> DBENTO_SSH_PASSWORD_B".to_owned(),
            "DBENTO_SSH_KEY_PASSPHRASE_A -> DBENTO_SSH_KEY_PASSPHRASE_B".to_owned(),
        ]
    );
}

#[gpui_kit::test]
fn an_exact_outside_rename_moves_the_ssh_secrets_too(cx: &mut TestAppContext) {
    let a = app(cx, &tun("nopg", "ops"), memory(), no_env());
    let dir = a.dir.clone();
    let pw = |n: &str| SecretKey::ssh_password(&dir, n, "ops@jump:22");
    let pp = |n: &str| SecretKey::ssh_key_passphrase(&dir, n);
    a.save(&pw("nopg"));
    a.save(&pp("nopg"));
    a.outside(cx, &tun("pg2", "ops"));
    wait_real(cx, "the moved SSH secrets", |_| {
        a.holds(&pw("pg2")) && a.holds(&pp("pg2"))
    });
    assert!(
        !a.holds(&pw("nopg")) && !a.holds(&pp("nopg")),
        "the old entries are gone"
    );
    assert!(
        !present(cx, a.win, "confirm-dialog"),
        "an exact rename asks nothing"
    );
}

#[gpui_kit::test]
fn another_ssh_user_leaves_the_ssh_password_and_says_so(cx: &mut TestAppContext) {
    let a = app(cx, &tun("nopg", "ops"), memory(), no_env());
    let old = SecretKey::ssh_password(&a.dir, "nopg", "ops@jump:22");
    a.save(&old);
    a.outside(cx, &tun("pg2", "dev"));
    wait_real(cx, "the notice", |cx| present(cx, a.win, "save-error-ok"));
    let detail = label_of(cx, a.win, "confirm-detail").unwrap();
    assert!(
        detail.contains("SSH password") && detail.contains("pg2"),
        "{detail}"
    );
    assert!(
        !detail.contains(SECRET),
        "the dialog never shows the secret"
    );
    assert!(a.holds(&old), "the old entry stays");
}

#[test]
fn a_failed_move_names_the_ssh_secrets_it_carried() {
    let ssh = Held {
        ssh_password: true,
        ..Held::default()
    };
    let text = not_moved_text(&locked(), "pg2", Some(ssh));
    assert!(
        text.ends_with("Enter the SSH password for pg2 again in Edit connection."),
        "{text}"
    );
    let both = Held {
        password: true,
        ssh_passphrase: true,
        ..Held::default()
    };
    let text = not_moved_text(&locked(), "pg2", Some(both));
    assert!(
        text.ends_with(
            "Enter the password and SSH key passphrase for pg2 again in Edit connection."
        ),
        "{text}"
    );
    let plain = Held {
        password: true,
        ..Held::default()
    };
    let text = not_moved_text(&locked(), "pg2", Some(plain));
    assert!(
        text.ends_with("Enter the password again for pg2."),
        "{text}"
    );
    assert!(not_moved_text(&locked(), "pg2", None).ends_with("Enter the password again for pg2."));
}
