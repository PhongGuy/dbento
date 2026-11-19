//! An outside rename carries a connection's saved keychain secrets to its new name.
use crate::support::{self, click_id, label_of, present, wait_real};
use dbento_app::app::{EnvLookup, start};
use dbento_app::rename_secret::{
    Held, adopt_held, env_lines, env_notice, move_ask, probe, rename_keys, still_renamed,
};
use dbento_core::{
    Edit, MemoryStore, Secret, SecretError, SecretKey, SecretStore, Workspace, secret_renames,
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
            "These passwords come from environment variables, which Dbento cannot rename. Rename them and restart Dbento.\nL1\nL2"
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
