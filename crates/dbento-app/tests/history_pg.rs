//! No secret reaches history, drafts or the state file, on Postgres with a distinctive password.
mod support;
use dbento_app::app::{AppPaths, Services, start};
use dbento_app::drafts::DRAFT_DEBOUNCE;
use dbento_core::{History, HistoryFilter};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext, VisualTestContext};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use support::{present, wait_real};

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(files_under(&p))
        } else {
            out.push(p)
        }
    }
    out
}

#[gpui_kit::test]
fn no_secret_reaches_the_app_files(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let rt = cx.update(|cx| dbento_app::tokio_bridge::handle(cx));
    let _enter = rt.enter();
    let db = rt.block_on(dbento_testkit::pg_db());
    let role = format!("dbento_probe_{}", std::process::id());
    let secret = format!("Zq9-{}-probe-s3cret", std::process::id());
    rt.block_on(async {
        db.admin().await.batch_execute(&format!(
            "DROP ROLE IF EXISTS {role}; CREATE ROLE {role} LOGIN PASSWORD '{secret}'; GRANT ALL ON DATABASE {} TO {role}",
            db.database()
        )).await.unwrap()
    });
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("dbento.toml"), format!(
        "dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"{h}\"\nport = {p}\ndatabase = \"{d}\"\nuser = \"{role}\"\nsecret = {{ env = \"{v}\" }}\n\
         [connections.zbad]\nengine = \"postgres\"\nhost = \"{h}\"\nport = {p}\ndatabase = \"{d}_missing\"\nuser = \"{role}\"\nsecret = {{ env = \"{v}\" }}\n",
        h = db.host(), p = db.port(), d = db.database(), v = dbento_testkit::SECRET_VAR
    )).unwrap();
    let files = tempfile::tempdir().unwrap();
    let pw = secret.clone();
    let services = Services {
        paths: AppPaths::in_dir(files.path()),
        store: Arc::new(dbento_core::MemoryStore::new()),
        env: Arc::new(move |v| (v == dbento_testkit::SECRET_VAR).then(|| pw.clone())),
        secrets_in_memory: false,
    };
    let (win, shell) = cx
        .update(|cx| start(services, Some(ws.path().into()), cx))
        .unwrap();
    assert!(present(cx, win, "connection-list"));
    support::open_connection_row(cx, win, "pg");
    let tab = shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    cx.update_window(win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql("SELECT 1; SELECT 1/0", w, cx));
        w.click("run-all", cx);
    })
    .unwrap();
    let history = AppPaths::in_dir(files.path()).history;
    let count = |p: &Path| {
        History::open(p)
            .map(|h| h.recent(&HistoryFilter::default(), 10).len())
            .unwrap_or(0)
    };
    wait_real(cx, "two entries", |_| count(&history) == 2);
    cx.update_window(win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql("SELECT 'unsaved'", w, cx))
    })
    .unwrap();
    cx.executor().advance_clock(DRAFT_DEBOUNCE);
    let drafts = AppPaths::in_dir(files.path()).drafts;
    wait_real(cx, "the draft", |_| {
        drafts.is_dir() && std::fs::read_dir(&drafts).unwrap().count() == 1
    });
    // A connect error must not reach the files either: `zbad` names a missing database.
    support::open_connection_row(cx, win, "zbad");
    let bad = shell.read_with(cx, |s, cx| s.tab().unwrap().read(cx).model().clone());
    wait_real(cx, "the connect error", |cx| {
        bad.read_with(cx, |m, _| m.error().is_some())
    });
    let mut v = VisualTestContext::from_window(win, cx);
    assert!(!v.simulate_close());
    wait_real(cx, "the window to close", |cx| {
        cx.update(|cx| cx.windows().is_empty())
    });
    let all = files_under(files.path());
    assert!(all.iter().any(|p| p.ends_with("state.json")), "{all:?}");
    for p in all {
        let bytes = std::fs::read(&p).unwrap();
        assert!(
            !bytes.windows(secret.len()).any(|w| w == secret.as_bytes()),
            "{} holds the secret",
            p.display()
        );
    }
    rt.block_on(async {
        let admin = db.admin().await;
        admin
            .batch_execute(&format!(
                "REVOKE ALL ON DATABASE {} FROM {role}",
                db.database()
            ))
            .await
            .unwrap();
        admin
            .batch_execute(&format!("DROP ROLE {role}"))
            .await
            .unwrap();
    });
}
