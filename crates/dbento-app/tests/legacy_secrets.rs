//! A password saved before pinning moves to its pinned key on the first open after the
//! upgrade, and only on evidence a pre-upgrade app wrote.
use crate::support::{self, fixture, present, wait_real};
use dbento_app::app::{AppPaths, SavedState, WorkspaceSession, start};
use dbento_app::persist::{load_json, save_json};
use dbento_core::{Secret, SecretError, SecretKey, SecretStore, seen_text};
use gpui_kit::{AnyWindowHandle, Entity, TestAppContext};
use serde_json::{Value, json};
use std::sync::Arc;

struct Up {
    fx: support::Fx,
    files: tempfile::TempDir,
    win: AnyWindowHandle,
    _shell: Entity<dbento_app::shell::Shell>,
    store: Arc<dyn SecretStore>,
}

impl Up {
    fn saved(&self) -> SavedState {
        load_json::<SavedState>(&AppPaths::in_dir(self.files.path()).state).value
    }
    fn disk(&self) -> Option<WorkspaceSession> {
        self.saved().session(self.fx.ws.dir()).cloned()
    }
}

/// A session as a pre-upgrade app wrote it: no `pin_epoch`, `seen` from the fixture's file.
fn old_session(fx: &support::Fx, tabs: Value) -> Value {
    let active = if tabs.as_array().is_some_and(|t| !t.is_empty()) {
        json!(0)
    } else {
        Value::Null
    };
    json!({
        "workspace": fx.ws.dir(),
        "tabs": tabs,
        "active": active,
        "collapsed": [],
        "seen": seen_text(&fx.ws).unwrap(),
    })
}

/// Every call fails as a missing keychain does.
struct NoKeychain;
fn gone() -> SecretError {
    SecretError::Unavailable {
        message: "no keychain".into(),
    }
}
impl SecretStore for NoKeychain {
    fn get(&self, _: &SecretKey) -> Result<Option<Secret>, SecretError> {
        Err(gone())
    }
    fn set(&self, _: &SecretKey, _: &Secret) -> Result<(), SecretError> {
        Err(gone())
    }
    fn delete(&self, _: &SecretKey) -> Result<(), SecretError> {
        Err(gone())
    }
}

/// The app started on `fx` over `store`, with `session` as the state file's only session.
fn up(
    cx: &mut TestAppContext,
    fx: support::Fx,
    session: Option<Value>,
    store: Arc<dyn SecretStore>,
) -> Up {
    support::init(cx);
    support::init_tokio(cx);
    let files = tempfile::tempdir().unwrap();
    if let Some(session) = session {
        let state = json!({ "sessions": [session] });
        save_json(&AppPaths::in_dir(files.path()).state, &state).unwrap();
    }
    let mut services = support::services(files.path());
    services.store = Arc::clone(&store);
    let (win, shell) = cx
        .update(|cx| start(services, Some(fx.dir.path().into()), cx))
        .unwrap();
    present(cx, win, "connection-list");
    Up {
        fx,
        files,
        win,
        _shell: shell,
        store,
    }
}

#[test]
fn a_session_from_before_pinning_reads_as_epoch_zero_and_a_baseline_round_trips() {
    let old = r#"{"sessions":[{"workspace":"/w","seen":"dbento = 1\n"}]}"#;
    let saved: SavedState = serde_json::from_str(old).unwrap();
    let s = saved.session(std::path::Path::new("/w")).unwrap().clone();
    assert_eq!(s.pin_epoch, 0, "a pre-upgrade session reads as epoch 0");
    assert_eq!(s.legacy_baseline, None, "and holds no baseline");
    let kept = WorkspaceSession {
        pin_epoch: 1,
        legacy_baseline: Some("dbento = 1\n".into()),
        ..s
    };
    let back: WorkspaceSession =
        serde_json::from_str(&serde_json::to_string(&kept).unwrap()).unwrap();
    assert_eq!(back, kept, "both fields round-trip");
    let bare = serde_json::to_string(&WorkspaceSession::default()).unwrap();
    assert!(
        !bare.contains("legacy_baseline"),
        "an absent baseline is not written: {bare}"
    );
}

#[gpui_kit::test]
fn a_failed_pass_keeps_the_frozen_snapshot_and_every_save_writes_it(cx: &mut TestAppContext) {
    let fx = fixture("", "");
    let old = seen_text(&fx.ws).unwrap();
    let session = old_session(&fx, json!([]));
    // The file moved on after the old app's snapshot, so `seen` and the baseline differ.
    let toml = fx.dir.path().join("dbento.toml");
    let text = std::fs::read_to_string(&toml).unwrap();
    std::fs::write(&toml, text.replace("user = \"x\"", "user = \"y\"")).unwrap();
    let u = up(cx, fx, Some(session), Arc::new(NoKeychain));
    let legacy = SecretKey::legacy_password(u.fx.ws.dir(), "nopg");
    assert!(
        u.store.get(&legacy).is_err(),
        "the keychain is unavailable throughout"
    );
    wait_real(cx, "the frozen baseline", |_| {
        u.disk().is_some_and(|s| s.pin_epoch == 1)
    });
    let frozen = u.disk().unwrap();
    assert_eq!(
        frozen.legacy_baseline.as_deref(),
        Some(old.as_str()),
        "the pre-upgrade snapshot is frozen and kept after a failed pass"
    );
    assert_ne!(
        frozen.seen.as_deref(),
        Some(old.as_str()),
        "seen moved on to the installed list"
    );
    // A later save rebuilds the whole session and must still carry both fields.
    support::open_connection_row(cx, u.win, "lite");
    wait_real(cx, "a later session save", |_| {
        u.disk().is_some_and(|s| !s.tabs.is_empty())
    });
    let later = u.disk().unwrap();
    assert_eq!(later.pin_epoch, 1, "save_session writes the epoch");
    assert_eq!(
        later.legacy_baseline.as_deref(),
        Some(old.as_str()),
        "save_session writes the in-memory baseline"
    );
}
