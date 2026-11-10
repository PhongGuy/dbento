use crate::support;

use crate::support::{click_id, fill, form, opened, present, wait_real};
use dbento_app::connection_dialog::Field;
use dbento_core::{SecretKey, SecretStore};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use std::sync::Arc;

fn store(cx: &mut TestAppContext, o: &support::Opened) -> Arc<dyn SecretStore> {
    o.shell
        .read_with(cx, |s, _| Arc::clone(&s.services().store))
}
fn key(o: &support::Opened, cx: &mut TestAppContext, name: &str) -> SecretKey {
    let dir = o
        .shell
        .read_with(cx, |s, _| s.workspace().unwrap().dir().to_path_buf());
    SecretKey::new(dir, name)
}
fn new_pg(cx: &mut TestAppContext, o: &support::Opened, name: &str, password: &str) {
    click_id(cx, o.win, "connection-new");
    form(cx, o);
    fill(cx, o, Field::Name, name);
    fill(cx, o, Field::Host, "127.0.0.1");
    fill(cx, o, Field::Port, "9");
    fill(cx, o, Field::Database, "d");
    fill(cx, o, Field::User, "u");
    fill(cx, o, Field::Password, password);
}

#[gpui_kit::test]
fn a_typed_password_goes_to_the_keychain_only(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    new_pg(cx, &o, "pg", "s3cr3t-Pw-9137");
    click_id(cx, o.win, "conn-save");
    let (st, k) = (store(cx, &o), key(&o, cx, "pg"));
    wait_real(cx, "the stored secret", |_| st.get(&k).unwrap().is_some());
    assert_eq!(st.get(&k).unwrap().unwrap().expose(), "s3cr3t-Pw-9137");
    let text = std::fs::read_to_string(o.fx.dir.path().join("dbento.toml")).unwrap();
    assert!(!text.contains("s3cr3t"), "{text}");
    assert!(
        !text.contains("secret ="),
        "keychain is the default and stays unwritten: {text}"
    );
}

#[gpui_kit::test]
fn an_empty_password_on_edit_keeps_the_stored_one(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let (st, k) = (store(cx, &o), key(&o, cx, "nopg"));
    st.set(&k, &dbento_core::Secret::new("old")).unwrap();
    support::gear(cx, o.win, "nopg");
    form(cx, &o);
    fill(cx, &o, Field::Database, "y");
    click_id(cx, o.win, "conn-save");
    cx.run_until_parked();
    assert_eq!(st.get(&k).unwrap().unwrap().expose(), "old");
}

#[gpui_kit::test]
fn an_environment_source_stores_nothing(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    new_pg(cx, &o, "pg", "typed");
    cx.update_window(o.win, |_, w, cx| w.within("conn-secret").click(1usize, cx))
        .unwrap();
    cx.run_until_parked();
    fill(cx, &o, Field::EnvVar, "PG_PW");
    click_id(cx, o.win, "conn-save");
    cx.run_until_parked();
    assert!(store(cx, &o).get(&key(&o, cx, "pg")).unwrap().is_none());
    let text = std::fs::read_to_string(o.fx.dir.path().join("dbento.toml")).unwrap();
    assert!(text.contains("secret = { env = \"PG_PW\" }"), "{text}");
}

#[gpui_kit::test]
fn no_keychain_note_with_a_keychain(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click_id(cx, o.win, "connection-new");
    form(cx, &o);
    assert!(!present(cx, o.win, "conn-keychain-note"));
}

#[gpui_kit::test]
fn the_keychain_note_names_the_environment_fallback(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let fx = support::fixture("", "");
    let files = tempfile::tempdir().unwrap();
    let mut services = support::services(files.path());
    services.secrets_in_memory = true;
    let (win, shell) = cx
        .update(|cx| dbento_app::app::start(services, Some(fx.dir.path().into()), cx))
        .unwrap();
    present(cx, win, "connection-list");
    click_id(cx, win, "connection-new");
    wait_real(cx, "the dialog", |cx| present(cx, win, "conn-name"));
    let f = shell.read_with(cx, |s, _| s.connection_form().cloned().unwrap());
    let name = f.read_with(cx, |f, _| f.input(Field::Name).clone());
    cx.update_window(win, |_, w, cx| {
        name.update(cx, |s, cx| s.set_value("my-db".to_owned(), w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    let note = support::label_of(cx, win, "conn-keychain-note").unwrap();
    assert!(
        note.contains("until Dbento quits") && note.contains("DBENTO_SECRET_MY_DB"),
        "{note}"
    );
}

#[gpui_kit::test]
fn test_connection_reports_an_unreachable_host_and_writes_nothing(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let before = std::fs::read_to_string(o.fx.dir.path().join("dbento.toml")).unwrap();
    new_pg(cx, &o, "pg", "pw");
    click_id(cx, o.win, "conn-test");
    wait_real(cx, "the test result", |cx| {
        support::label_of(cx, o.win, "conn-test-result").is_some_and(|l| l.starts_with("Failed: "))
    });
    assert_eq!(
        std::fs::read_to_string(o.fx.dir.path().join("dbento.toml")).unwrap(),
        before
    );
    assert!(
        present(cx, o.win, "conn-name"),
        "testing keeps the dialog open"
    );
}

#[gpui_kit::test]
fn the_typed_password_is_cleared_when_the_dialog_closes(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    new_pg(cx, &o, "pg", "cancelled-pw");
    let cancelled = form(cx, &o);
    click_id(cx, o.win, "conn-cancel");
    assert!(cancelled.read_with(cx, |f, cx| f.password(cx)).is_none());

    new_pg(cx, &o, "pg", "saved-pw");
    let saved = form(cx, &o);
    click_id(cx, o.win, "conn-save");
    assert!(saved.read_with(cx, |f, cx| f.password(cx)).is_none());
}

#[gpui_kit::test]
fn test_connection_during_a_rename_finds_the_passphrase_under_the_old_name(
    cx: &mut TestAppContext,
) {
    let s = support::started(
        cx,
        "dbento = 1\n[connections.old]\nengine = \"postgres\"\nhost = \"127.0.0.1\"\nport = 9\n\
         database = \"d\"\nuser = \"u\"\nsecret = \"none\"\ntls = \"verify\"\n\
         tls_cert = \"me.crt\"\ntls_key = \"me.key\"\n",
    );
    // Stub PEMs: the key only has to look encrypted for the passphrase lookup to run.
    let pem = |label: &str| format!("-----BEGIN {label}-----\nAAAA\n-----END {label}-----\n");
    std::fs::write(s.dir.path().join("me.crt"), pem("CERTIFICATE")).unwrap();
    std::fs::write(s.dir.path().join("me.key"), pem("ENCRYPTED PRIVATE KEY")).unwrap();
    let st = s
        .shell
        .read_with(cx, |s, _| Arc::clone(&s.services().store));
    st.set(
        &SecretKey::key_passphrase(s.dir.path(), "old"),
        &dbento_core::Secret::new("stored-under-old"),
    )
    .unwrap();
    support::gear(cx, s.win, "old");
    wait_real(cx, "the dialog", |cx| present(cx, s.win, "conn-name"));
    let f = s
        .shell
        .read_with(cx, |s, _| s.connection_form().cloned().unwrap());
    let name = f.read_with(cx, |f, _| f.input(Field::Name).clone());
    cx.update_window(s.win, |_, w, cx| {
        name.update(cx, |s, cx| s.set_value("new".to_owned(), w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    click_id(cx, s.win, "conn-test");
    wait_real(cx, "the test result", |cx| {
        support::label_of(cx, s.win, "conn-test-result").is_some_and(|l| l.starts_with("Failed: "))
    });
    let failed = support::label_of(cx, s.win, "conn-test-result").unwrap();
    // A found passphrase gets as far as decoding the stub key.
    assert!(!failed.contains("no passphrase was found"), "{failed}");
    assert!(failed.contains("not an encrypted PKCS#8 key"), "{failed}");
}
