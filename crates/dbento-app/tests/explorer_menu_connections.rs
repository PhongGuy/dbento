use crate::support;
use crate::support::{
    TREE, click_id, label_of, name, present, right_click, right_click_empty, started,
    started_custom, started_with, wait_real,
};
use dbento_app::explorer::{connection_id, part_id};
use dbento_core::{Secret, SecretError, SecretKey, SecretStore};
use gpui_kit::{AppContext as _, TestAppContext};
use std::sync::Arc;

const PG: &str = "\n[connections.pg]\nengine = \"postgres\"\nhost = \"127.0.0.1\"\nport = 9\ndatabase = \"x\"\nuser = \"x\"\n";

#[gpui_kit::test]
fn edit_and_new_connection_open_the_dialog(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    right_click(cx, &s, connection_id("top"));
    click_id(cx, s.win, "menu-edit");
    wait_real(cx, "the dialog", |cx| present(cx, s.win, "conn-name"));
    let s = started(cx, TREE);
    right_click_empty(cx, &s);
    click_id(cx, s.win, "menu-new-connection");
    wait_real(cx, "the dialog", |cx| present(cx, s.win, "conn-name"));
}

#[gpui_kit::test]
fn a_menu_rename_moves_the_secret_and_the_saved_queries(cx: &mut TestAppContext) {
    let store: Arc<dyn SecretStore> = Arc::new(dbento_core::MemoryStore::new());
    let s = started_with(cx, &format!("{TREE}{PG}"), store.clone());
    let dir = s
        .shell
        .read_with(cx, |sh, _| sh.workspace().unwrap().dir().to_path_buf());
    store
        .set(&SecretKey::new(&dir, "pg"), &Secret::new("pw"))
        .unwrap();
    std::fs::create_dir_all(dir.join("queries/pg")).unwrap();
    std::fs::write(dir.join("queries/pg/a.sql"), "SELECT 1").unwrap();
    right_click(cx, &s, connection_id("pg"));
    click_id(cx, s.win, "menu-rename");
    name(cx, &s, "pg2");
    let new = SecretKey::new(&dir, "pg2");
    wait_real(cx, "the moved secret", |_| {
        store.get(&new).unwrap().is_some()
    });
    assert!(store.get(&SecretKey::new(&dir, "pg")).unwrap().is_none());
    assert!(dir.join("queries/pg2/a.sql").exists());
    assert!(s.toml().contains("[connections.pg2]"), "{}", s.toml());
    assert!(present(cx, s.win, connection_id("pg2")));
}

#[gpui_kit::test]
fn a_bad_connection_name_stays_in_the_dialog(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    right_click(cx, &s, connection_id("top"));
    click_id(cx, s.win, "menu-rename");
    name(cx, &s, "lite");
    assert!(
        label_of(cx, s.win, "save-name-error")
            .unwrap()
            .contains("already exists")
    );
    name(cx, &s, "a b");
    assert!(
        label_of(cx, s.win, "save-name-error")
            .unwrap()
            .contains("[A-Za-z0-9_-]")
    );
    assert_eq!(s.toml(), TREE);
}

#[gpui_kit::test]
fn rename_and_delete_are_refused_while_a_tab_is_open(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    support::open_connection_row(cx, s.win, "top");
    for item in ["menu-rename", "menu-delete"] {
        right_click(cx, &s, connection_id("top"));
        click_id(cx, s.win, item);
        wait_real(cx, "the refusal", |cx| present(cx, s.win, "save-error-ok"));
        assert_eq!(
            label_of(cx, s.win, "confirm-detail").as_deref(),
            Some("Close its tabs first")
        );
        assert!(!present(cx, s.win, "save-name") && !present(cx, s.win, "delete-ok"));
        click_id(cx, s.win, "save-error-ok");
    }
    assert_eq!(s.toml(), TREE);
}

#[gpui_kit::test]
fn a_swatch_recolours_the_connection(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    right_click(cx, &s, connection_id("top"));
    click_id(cx, s.win, ("menu-color", 1usize));
    assert!(
        s.toml().ends_with("path = \"t.db\"\ncolor = \"red\"\n"),
        "{}",
        s.toml()
    );
    assert_eq!(
        label_of(cx, s.win, part_id("connection-color", "top")).as_deref(),
        Some("red")
    );
}

#[gpui_kit::test]
fn a_menu_delete_asks_then_removes_the_entry_and_secret(cx: &mut TestAppContext) {
    let store: Arc<dyn SecretStore> = Arc::new(dbento_core::MemoryStore::new());
    let s = started_with(cx, &format!("{TREE}{PG}"), store.clone());
    let dir = s
        .shell
        .read_with(cx, |sh, _| sh.workspace().unwrap().dir().to_path_buf());
    let key = SecretKey::new(&dir, "pg");
    store.set(&key, &Secret::new("pw")).unwrap();
    right_click(cx, &s, connection_id("pg"));
    click_id(cx, s.win, "menu-delete");
    wait_real(cx, "the question", |cx| present(cx, s.win, "delete-ok"));
    click_id(cx, s.win, "delete-ok");
    assert_eq!(s.toml(), TREE);
    wait_real(cx, "the removed secret", |_| {
        store.get(&key).unwrap().is_none()
    });
}

// A connection's New folder lands in that connection's folder, never at the top level.
#[gpui_kit::test]
fn new_folder_from_a_connection_nests_in_its_folder(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    right_click(cx, &s, connection_id("lite"));
    click_id(cx, s.win, "menu-new-folder");
    name(cx, &s, "eu");
    assert!(
        s.toml().ends_with("\n[folders.\"team/sub/eu\"]\n"),
        "{}",
        s.toml()
    );
}

// The variable is derived from the name, so the hint must not suggest it carries over.
#[gpui_kit::test]
fn the_rename_hint_says_the_password_variable_changes(cx: &mut TestAppContext) {
    let s = started_custom(cx, TREE, |sv| {
        sv.env = Arc::new(|v| (v == "DBENTO_SECRET_TOP").then(|| "pw".to_owned()));
    });
    right_click(cx, &s, connection_id("top"));
    click_id(cx, s.win, "menu-rename");
    wait_real(cx, "the name dialog", |cx| present(cx, s.win, "save-name"));
    let hint = label_of(cx, s.win, "save-hint").unwrap();
    assert!(
        hint.contains("DBENTO_SECRET_TOP") && hint.contains("becomes DBENTO_SECRET_<NEW NAME>"),
        "{hint}"
    );
}

fn reload(cx: &mut TestAppContext, s: &support::Started) {
    let shell = s.shell.clone();
    cx.update_window(s.win, |_, w, cx| {
        shell.update(cx, |sh, cx| sh.reload_workspace(w, cx))
    })
    .unwrap();
    cx.run_until_parked();
}

// A reload while the dialog is up must not revert hand edits made to the connection.
#[gpui_kit::test]
fn a_menu_rename_writes_the_spec_installed_at_ok(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    right_click(cx, &s, connection_id("top"));
    click_id(cx, s.win, "menu-rename");
    wait_real(cx, "the name dialog", |cx| present(cx, s.win, "save-name"));
    let path = s.dir.path().join("dbento.toml");
    std::fs::write(&path, TREE.replace("\"t.db\"", "\"t2.db\"")).unwrap();
    reload(cx, &s);
    name(cx, &s, "top2");
    let text = s.toml();
    assert!(
        text.contains("[connections.top2]") && text.contains("path = \"t2.db\""),
        "{text}"
    );
}

#[gpui_kit::test]
fn a_menu_rename_of_a_connection_gone_by_ok_shows_an_error(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    right_click(cx, &s, connection_id("top"));
    click_id(cx, s.win, "menu-rename");
    wait_real(cx, "the name dialog", |cx| present(cx, s.win, "save-name"));
    let gone = TREE.split("\n[connections.top]").next().unwrap().to_owned();
    std::fs::write(s.dir.path().join("dbento.toml"), &gone).unwrap();
    reload(cx, &s);
    name(cx, &s, "top2");
    wait_real(cx, "the error", |cx| present(cx, s.win, "save-error-ok"));
    assert_eq!(s.toml(), gone);
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

// Neither a SQLite file nor an env-sourced secret lives in the keychain.
#[gpui_kit::test]
fn a_menu_rename_without_a_keychain_secret_makes_no_keychain_calls(cx: &mut TestAppContext) {
    let env_pg = format!("{PG}secret = {{ env = \"NOPG_PW\" }}\n");
    let counting = Arc::new(Counting(
        dbento_core::MemoryStore::new(),
        Default::default(),
    ));
    let kc = PG.replace("connections.pg", "connections.kc");
    let s = started_with(cx, &format!("{TREE}{env_pg}{kc}"), counting.clone());
    let before = counting.calls();
    for (old, new) in [("top", "top2"), ("pg", "pg2")] {
        right_click(cx, &s, connection_id(old));
        click_id(cx, s.win, "menu-rename");
        name(cx, &s, new);
        wait_real(cx, "the rename", |cx| {
            present(cx, s.win, connection_id(new))
        });
    }
    // Keychain calls run on the test executor, so parking drains every one a rename queued.
    cx.run_until_parked();
    assert_eq!(counting.calls(), before);
    // A keychain-backed rename shows those calls do land within the same wait.
    right_click(cx, &s, connection_id("kc"));
    click_id(cx, s.win, "menu-rename");
    name(cx, &s, "kc2");
    cx.run_until_parked();
    assert!(
        counting.calls() > before,
        "the keychain rename made no calls"
    );
}
