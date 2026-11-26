//! A password saved before pinning moves to its pinned key on the first open after the
//! upgrade, and only on evidence a pre-upgrade app wrote.
use crate::support::{self, fixture, label_of, present, wait_real};
use dbento_app::app::{AppPaths, SavedState, WorkspaceSession, start};
use dbento_app::persist::{load_json, save_json};
use dbento_core::{MemoryStore, Secret, SecretError, SecretKey, SecretStore, seen_text};
use gpui_kit::{AnyWindowHandle, Entity, TestAppContext};
use serde_json::{Value, json};
use std::sync::{Arc, Condvar, Mutex};

struct Up {
    fx: support::Fx,
    files: tempfile::TempDir,
    win: AnyWindowHandle,
    shell: Entity<dbento_app::shell::Shell>,
    store: Arc<dyn SecretStore>,
}

impl Up {
    fn saved(&self) -> SavedState {
        load_json::<SavedState>(&AppPaths::in_dir(self.files.path()).state).value
    }
    fn disk(&self) -> Option<WorkspaceSession> {
        self.saved().session(self.fx.ws.dir()).cloned()
    }
    fn holds(&self, k: &SecretKey) -> bool {
        self.store.get(k).is_ok_and(|s| s.is_some())
    }
    fn legacy(&self, name: &str) -> SecretKey {
        SecretKey::legacy_password(self.fx.ws.dir(), name)
    }
    /// The pinned key for `name` as the file declares it now.
    fn pinned(&self, name: &str) -> SecretKey {
        let dir = self.fx.ws.dir();
        SecretKey::password(dir, name, &support::pin_of(dir, name))
    }
}

/// A memory keychain holding a legacy password for `name`.
fn with_legacy(fx: &support::Fx, name: &str) -> Arc<dyn SecretStore> {
    let store = MemoryStore::new();
    let legacy = SecretKey::legacy_password(fx.ws.dir(), name);
    store.set(&legacy, &Secret::new("x")).unwrap();
    Arc::new(store)
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
        shell,
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

#[gpui_kit::test]
fn an_upgraded_session_moves_the_legacy_password_and_clears_the_baseline(cx: &mut TestAppContext) {
    let fx = fixture("", "");
    let store = with_legacy(&fx, "nopg");
    let session = old_session(&fx, json!([]));
    let u = up(cx, fx, Some(session), store);
    wait_real(cx, "the pinned password", |_| u.holds(&u.pinned("nopg")));
    let moved = u.store.get(&u.pinned("nopg")).unwrap().unwrap();
    assert_eq!(moved.expose(), "x", "the legacy password moved unchanged");
    assert!(!u.holds(&u.legacy("nopg")), "the legacy entry is gone");
    // Epoch 1 is only ever written with the baseline until the pass clears it.
    wait_real(cx, "the cleared baseline", |_| {
        u.disk()
            .is_some_and(|s| s.pin_epoch == 1 && s.legacy_baseline.is_none())
    });
}

#[gpui_kit::test]
fn a_session_the_new_app_wrote_never_migrates(cx: &mut TestAppContext) {
    let fx = fixture("", "");
    let store = with_legacy(&fx, "nopg");
    let mut session = old_session(&fx, json!([]));
    session["pin_epoch"] = json!(1);
    let u = up(cx, fx, Some(session), store);
    wait_real(cx, "the open's state write", |_| {
        u.saved().last_workspace.is_some()
    });
    assert!(
        u.holds(&u.legacy("nopg")),
        "no pre-upgrade evidence, so the legacy entry stays"
    );
    assert!(!u.holds(&u.pinned("nopg")), "nothing was pinned");
}

#[gpui_kit::test]
fn an_unreadable_baseline_is_dropped_and_moves_nothing(cx: &mut TestAppContext) {
    let fx = fixture("", "");
    let store = with_legacy(&fx, "nopg");
    let mut session = old_session(&fx, json!([]));
    session["seen"] = json!("dbento = 1\n[connections.\n");
    let u = up(cx, fx, Some(session), store);
    wait_real(cx, "the dropped baseline", |_| {
        u.disk()
            .is_some_and(|s| s.pin_epoch == 1 && s.legacy_baseline.is_none())
    });
    assert!(
        u.holds(&u.legacy("nopg")),
        "nothing parsed, so nothing moved"
    );
    assert!(!u.holds(&u.pinned("nopg")), "nothing was pinned");
}

/// A store whose reads of `hold` wait until released, logging every key read.
struct Latch {
    inner: MemoryStore,
    hold: SecretKey,
    held: Mutex<bool>,
    open: Condvar,
    log: Mutex<Vec<SecretKey>>,
}
impl Latch {
    fn release(&self) {
        *self.held.lock().unwrap() = false;
        self.open.notify_all();
    }
    fn reads(&self) -> Vec<SecretKey> {
        self.log.lock().unwrap().clone()
    }
}
impl SecretStore for Latch {
    fn get(&self, k: &SecretKey) -> Result<Option<Secret>, SecretError> {
        self.log.lock().unwrap().push(k.clone());
        if *k == self.hold {
            let mut held = self.held.lock().unwrap();
            while *held {
                held = self.open.wait(held).unwrap();
            }
        }
        self.inner.get(k)
    }
    fn set(&self, k: &SecretKey, s: &Secret) -> Result<(), SecretError> {
        self.inner.set(k, s)
    }
    fn delete(&self, k: &SecretKey) -> Result<(), SecretError> {
        self.inner.delete(k)
    }
}
/// Opens the latch when the test ends, pass or fail, so no keychain thread stays blocked.
struct Release(Arc<Latch>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}
/// A latch holding `nopg`'s legacy read, over a store that has the legacy password `x`.
fn latched(fx: &support::Fx) -> Arc<Latch> {
    let inner = MemoryStore::new();
    let hold = SecretKey::legacy_password(fx.ws.dir(), "nopg");
    inner.set(&hold, &Secret::new("x")).unwrap();
    Arc::new(Latch {
        inner,
        hold,
        held: Mutex::new(true),
        open: Condvar::new(),
        log: Mutex::default(),
    })
}

#[gpui_kit::test]
fn a_restored_tab_waits_for_the_pass_and_connects_with_the_migrated_password(
    cx: &mut TestAppContext,
) {
    let fx = fixture("", "");
    let latch = latched(&fx);
    let _release = Release(Arc::clone(&latch));
    let session = old_session(&fx, json!([{ "connection": "nopg", "draft": "d0" }]));
    let u = up(
        cx,
        fx,
        Some(session),
        Arc::clone(&latch) as Arc<dyn SecretStore>,
    );
    wait_real(cx, "the pass at the latch", |_| {
        latch.reads().contains(&latch.hold)
    });
    cx.run_until_parked();
    let model = u
        .shell
        .read_with(cx, |s, cx| s.tabs()[0].read(cx).model().clone());
    assert!(
        model.read_with(cx, |m, _| m.events().is_none()),
        "the restored tab must not connect while the pass runs"
    );
    latch.release();
    wait_real(cx, "the restored tab's connect", |cx| {
        label_of(cx, u.win, "connect-error-message").is_some()
    });
    // The closed port refuses either way; only a missed password adds a hint.
    let message = label_of(cx, u.win, "connect-error-message").unwrap();
    for hint in [
        "saved password",
        "no secret was found",
        "no password is saved",
    ] {
        assert!(
            !message.contains(hint),
            "the connect had the password: {message}"
        );
    }
    assert!(
        u.holds(&u.pinned("nopg")),
        "the pass pinned the legacy entry"
    );
}

#[gpui_kit::test]
fn a_rename_from_before_the_upgrade_waits_for_the_pass_then_carries(cx: &mut TestAppContext) {
    let fx = fixture("", "");
    let latch = latched(&fx);
    let _release = Release(Arc::clone(&latch));
    let old_pin = fx.ws.secret_target("nopg").unwrap();
    let session = old_session(&fx, json!([]));
    let toml = fx.dir.path().join("dbento.toml");
    let renamed = std::fs::read_to_string(&toml)
        .unwrap()
        .replace("[connections.nopg]", "[connections.pg2]");
    std::fs::write(&toml, renamed).unwrap();
    let u = up(
        cx,
        fx,
        Some(session),
        Arc::clone(&latch) as Arc<dyn SecretStore>,
    );
    wait_real(cx, "the pass at the latch", |_| {
        latch.reads().contains(&latch.hold)
    });
    cx.run_until_parked();
    // The pass reads the pinned key, then the legacy one; an ungated probe adds its own reads.
    let early = latch.reads();
    assert_eq!(
        early,
        vec![old_pin.clone(), latch.hold.clone()],
        "only the pass has read the keychain"
    );
    latch.release();
    wait_real(cx, "the carried password", |_| u.holds(&u.pinned("pg2")));
    assert!(
        !u.holds(&u.legacy("nopg")),
        "the legacy entry was pinned first"
    );
    assert!(
        !u.holds(&old_pin),
        "the pinned entry moved on to the new name"
    );
}
