//! A tab whose connection fails shows the failure, a fix where one is known, Retry and
//! Edit connection in place of the empty result area.
use crate::support::{
    click_id, init, init_tokio, label_of, open_connection_row, present, services, wait_real,
};
use dbento_app::link::{ConnectCause, fix_hint};
use dbento_core::{ConnectError, SecretError};
use gpui_kit::TestAppContext;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[test]
fn a_missing_variable_asks_for_it_or_the_keychain() {
    let e = SecretError::Missing {
        variable: "PG_PW".into(),
    };
    assert_eq!(
        fix_hint(ConnectCause::Secret(&e)).as_deref(),
        Some("Set PG_PW or store the secret in the keychain from Edit connection."),
        "the variable to set"
    );
}

#[test]
fn a_refused_connection_points_at_the_server() {
    let e = ConnectError::Refused {
        address: "db:5432".into(),
        message: "u@db:5432/d: error connecting to server".into(),
    };
    assert_eq!(
        fix_hint(ConnectCause::Connect(&e)).as_deref(),
        Some("Check that the server is running at db:5432."),
        "the address to check"
    );
}

#[test]
fn other_failures_have_no_hint() {
    let open = ConnectError::Open {
        message: "password authentication failed".into(),
    };
    assert_eq!(
        fix_hint(ConnectCause::Connect(&open)),
        None,
        "a server refusal"
    );
    let locked = SecretError::Unavailable {
        message: "locked".into(),
    };
    assert_eq!(
        fix_hint(ConnectCause::Secret(&locked)),
        None,
        "a locked keychain"
    );
}

const TOML: &str = "dbento = 1\n[connections.envpg]\nengine = \"postgres\"\nhost = \"127.0.0.1\"\nport = 9\ndatabase = \"x\"\nuser = \"x\"\nsecret = { env = \"DBENTO_TEST_RETRY_PW\" }\n";

// A workspace whose one connection reads its secret from a variable the test sets later,
// opened on that connection's tab.
fn opened_env(
    cx: &mut TestAppContext,
) -> (
    gpui_kit::AnyWindowHandle,
    tempfile::TempDir,
    tempfile::TempDir,
    Arc<AtomicBool>,
) {
    init(cx);
    init_tokio(cx);
    let (dir, files) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(dir.path().join("dbento.toml"), TOML).unwrap();
    let set = Arc::new(AtomicBool::new(false));
    let mut svc = services(files.path());
    let flag = Arc::clone(&set);
    svc.env = Arc::new(move |v| {
        (v == "DBENTO_TEST_RETRY_PW" && flag.load(Ordering::SeqCst)).then(|| "pw".to_owned())
    });
    let (win, _shell) = cx
        .update(|cx| dbento_app::app::start(svc, Some(dir.path().into()), cx))
        .unwrap();
    present(cx, win, "connection-list");
    open_connection_row(cx, win, "envpg");
    (win, dir, files, set)
}

#[gpui_kit::test]
fn a_failed_connect_fills_the_result_area_with_the_error_and_its_fix(cx: &mut TestAppContext) {
    let (win, _dir, _files, _set) = opened_env(cx);
    wait_real(cx, "the failure", |cx| present(cx, win, "connect-error"));
    assert_eq!(
        label_of(cx, win, "connect-error").as_deref(),
        Some("Can't connect to envpg"),
        "the title"
    );
    assert_eq!(
        label_of(cx, win, "connect-error-message").as_deref(),
        Some("the environment variable DBENTO_TEST_RETRY_PW is not set"),
        "the error"
    );
    assert_eq!(
        label_of(cx, win, "connect-error-hint").as_deref(),
        Some("Set DBENTO_TEST_RETRY_PW or store the secret in the keychain from Edit connection."),
        "the fix"
    );
    assert!(present(cx, win, "connect-error-retry"), "Retry shows");
    assert!(
        present(cx, win, "connect-error-edit"),
        "Edit connection shows"
    );
    assert!(!present(cx, win, "result-empty"), "the run prompt hides");
    assert!(!present(cx, win, "result-message"), "no gutter line");
}

#[gpui_kit::test]
fn retry_connects_again_with_what_changed_since(cx: &mut TestAppContext) {
    let (win, _dir, _files, set) = opened_env(cx);
    wait_real(cx, "the failure", |cx| present(cx, win, "connect-error"));
    set.store(true, Ordering::SeqCst);
    click_id(cx, win, "connect-error-retry");
    // With the variable set, the next attempt reaches the network and nothing listens.
    wait_real(cx, "the refusal", |cx| {
        label_of(cx, win, "connect-error-hint").as_deref()
            == Some("Check that the server is running at 127.0.0.1:9.")
    });
}

#[gpui_kit::test]
fn edit_connection_opens_the_dialog_on_the_tabs_connection(cx: &mut TestAppContext) {
    let (win, _dir, _files, _set) = opened_env(cx);
    wait_real(cx, "the failure", |cx| present(cx, win, "connect-error"));
    click_id(cx, win, "connect-error-edit");
    wait_real(cx, "the dialog", |cx| present(cx, win, "conn-name"));
}
