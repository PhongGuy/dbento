//! A tab opens its session before the first Run; a failed connect shows and is retried.
//! Opening a workspace connects only the focused tab, within `PRECONNECT_CAP`.
use crate::support::{
    MANY, Opened, fixture, here, init, init_tokio, label_of, next_event, opened, pg_spec, present,
    run_cmd, services, spec, until_event, wait_real,
};
use dbento_app::app::{AppPaths, SavedState, TabState, WorkspaceSession, start};
use dbento_app::persist::save_json;
use dbento_app::runner::{Command, Event, StatementEnd, StopReason, spawn_on};
use dbento_app::shell::PRECONNECT_CAP;
use dbento_app::tab_model::TabModel;
use dbento_core::CellRef;
use gpui_kit::{AppContext as _, Entity, TestAppContext};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const T: &str = "CREATE TABLE t(x); INSERT INTO t VALUES (1);";

#[tokio::test(flavor = "multi_thread")]
async fn connect_opens_the_session_and_the_run_reuses_it() {
    let fx = fixture(T, "");
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    // Sent back to back: the Run queues behind the connect, and a second Connect is a no-op.
    r.send(Command::Connect);
    r.send(Command::Connect);
    r.send(run_cmd(1, "SELECT x FROM t", Some(10)));
    assert!(matches!(
        next_event(&ev).await,
        Event::Connected {
            read_only: false,
            ..
        }
    ));
    let (seen, rows) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })).await;
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, Event::Connected { .. } | Event::ConnectFailed { .. })),
        "{seen:?}"
    );
    assert_eq!(rows, 1);
    assert!(seen.iter().any(|e| matches!(
        e,
        Event::StatementDone {
            end: StatementEnd::Complete(_),
            ..
        }
    )));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_only_connection_preconnects_read_only() {
    let fx = fixture(T, "read_only = true");
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(Command::Connect);
    assert!(matches!(
        next_event(&ev).await,
        Event::Connected {
            read_only: true,
            ..
        }
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_connect_is_reported_and_the_next_run_retries() {
    let fx = fixture("", "");
    let r = spawn_on(&here(), spec(&fx, "nopg"));
    let ev = r.events();
    r.send(Command::Connect);
    match next_event(&ev).await {
        Event::ConnectFailed { message } => {
            assert!(message.contains("DBENTO_SECRET_NOPG"), "{message}")
        }
        other => panic!("{other:?}"),
    }
    r.send(run_cmd(1, "SELECT 1", Some(10)));
    let (seen, _) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })).await;
    assert!(
        matches!(seen[0], Event::ConnectFailed { .. }),
        "the Run tried again: {seen:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn close_during_a_connect_ends_the_runner() {
    let fx = fixture(T, "");
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(Command::Connect);
    r.send(Command::Close);
    let ended = tokio::time::timeout(Duration::from_secs(10), async {
        while ev.recv().await.is_ok() {}
    })
    .await;
    assert!(ended.is_ok(), "the runner did not end");
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_preconnect_opens_a_backend_before_any_run() {
    let (db, _dir, spec) = pg_spec("").await;
    let r = spawn_on(&here(), spec);
    let ev = r.events();
    r.send(Command::Connect);
    assert!(matches!(next_event(&ev).await, Event::Connected { .. }));
    let ours = "SELECT count(*) FROM pg_stat_activity WHERE application_name = 'dbento' AND datname = current_database()";
    assert_eq!(db.scalar(ours).await.as_deref(), Some("1"));
    r.send(run_cmd(1, "SELECT 1", Some(10)));
    assert!(
        matches!(next_event(&ev).await, Event::Executing { run_id: 1, .. }),
        "the Run reconnected"
    );
}

#[gpui_kit::test]
fn a_failed_preconnect_shows_its_error_and_the_next_run_retries(cx: &mut TestAppContext) {
    init_tokio(cx);
    let fx = fixture(T, "");
    // Core opens existing SQLite files only, so moving the file aside fails the connect.
    let (db, aside) = (
        fx.dir.path().join("data/app.db"),
        fx.dir.path().join("data/aside.db"),
    );
    std::fs::rename(&db, &aside).unwrap();
    let m = cx.new(|_| TabModel::new(spec(&fx, "lite")));
    m.update(cx, |m, cx| m.connect(cx));
    wait_real(cx, "the failed connect", |cx| {
        m.read_with(cx, |m, _| m.error().is_some())
    });
    m.read_with(cx, |m, _| {
        assert_eq!(m.read_only(), None);
        assert!(!m.running());
    });
    std::fs::rename(&aside, &db).unwrap();
    m.update(cx, |m, cx| m.run("SELECT x FROM t".into(), cx));
    wait_real(cx, "the run", |cx| m.read_with(cx, |m, _| !m.running()));
    m.read_with(cx, |m, _| {
        assert_eq!(m.error(), None, "the reconnect cleared the error");
        assert_eq!(m.read_only(), Some(false));
        assert_eq!(m.results()[0].store.cell(0, 0), Some(CellRef::Text("1")));
    });
}

#[gpui_kit::test]
fn a_closed_model_does_not_connect(cx: &mut TestAppContext) {
    init_tokio(cx);
    let fx = fixture(T, "");
    let m = cx.new(|_| TabModel::new(spec(&fx, "lite")));
    m.update(cx, |m, cx| {
        let _ = m.close(cx);
        m.connect(cx);
    });
    assert!(
        m.read_with(cx, |m, _| m.events().is_none()),
        "a runner started"
    );
}

#[gpui_kit::test]
fn opening_a_connection_connects_before_the_first_run(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    let m = o.model(cx);
    wait_real(cx, "the pre-connect", |cx| {
        m.read_with(cx, |m, _| m.read_only().is_some())
    });
    m.read_with(cx, |m, _| {
        assert_eq!(m.run_id(), 0);
        assert!(m.results().is_empty() && !m.running());
    });
}

#[gpui_kit::test]
fn a_failed_preconnect_shows_on_the_tab(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 1);
    wait_real(cx, "the message", |cx| {
        label_of(cx, o.win, "result-message").is_some()
    });
    let message = label_of(cx, o.win, "result-message").unwrap();
    assert!(message.contains("DBENTO_SECRET_NOPG"), "{message}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connect_mid_script_is_dropped_and_the_script_runs_on() {
    let fx = fixture(T, "");
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    // The Connect waits in the inbox while the Run connects, so the script finds it between
    // statements; it must neither end the script nor open a second session.
    r.send(run_cmd(1, "SELECT 1; SELECT 2", Some(10)));
    r.send(Command::Connect);
    let (seen, _) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })).await;
    let connects = seen
        .iter()
        .filter(|e| matches!(e, Event::Connected { .. }))
        .count();
    assert_eq!(connects, 1, "{seen:?}");
    let complete = seen
        .iter()
        .filter(|e| {
            matches!(
                e,
                Event::StatementDone {
                    end: StatementEnd::Complete(_),
                    ..
                }
            )
        })
        .count();
    assert_eq!(complete, 2, "{seen:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connect_while_parked_leaves_the_stream_to_its_pulls() {
    let fx = fixture(T, "");
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(10_000)));
    until_event(&ev, |e| matches!(e, Event::Paused { run_id: 1, .. })).await;
    // Behind the Pull, the Connect is read between batches, where a cut would end the stream.
    r.send(Command::Pull { run_id: 1 });
    r.send(Command::Connect);
    assert!(
        matches!(next_event(&ev).await, Event::Batch { run_id: 1, .. }),
        "the Connect cut the parked stream"
    );
    r.send(Command::Stop { run_id: 1 });
    let (seen, _) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })).await;
    assert!(
        seen.iter().any(|e| matches!(
            e,
            Event::StatementDone {
                end: StatementEnd::Stopped {
                    reason: StopReason::User,
                    ..
                },
                ..
            }
        )),
        "{seen:?}"
    );
    assert!(!seen.iter().any(|e| matches!(e, Event::Connected { .. })));
}

#[gpui_kit::test]
// No cancel handle exists until the session connects, so the Cancel must reach the run itself.
fn a_cancel_before_the_session_connects_stops_the_run(cx: &mut TestAppContext) {
    init_tokio(cx);
    let rt = cx.update(|cx| dbento_app::tokio_bridge::handle(cx));
    let _enter = rt.enter();
    let (_db, _dir, mut spec) = rt.block_on(pg_spec(""));
    // The secret lookup waits on this gate, so the connect is still open when Cancel comes.
    let released = Arc::new(AtomicBool::new(false));
    let (gate, lookup) = (released.clone(), spec.env.clone());
    spec.env = Arc::new(move |v| {
        while !gate.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        lookup(v)
    });
    let m = cx.new(|_| TabModel::new(spec));
    m.update(cx, |m, cx| {
        m.run("SELECT 1".into(), cx);
        m.cancel();
    });
    released.store(true, Ordering::SeqCst);
    wait_real(cx, "the run", |cx| m.read_with(cx, |m, _| !m.running()));
    m.read_with(cx, |m, _| {
        assert_eq!(m.read_only(), Some(false), "the session connected");
        assert!(m.results().is_empty(), "the statement ran after the Cancel");
    });
}

// Starts the app on a workspace whose saved session holds `n` lite tabs with tab `active` focused.
fn restored_with(cx: &mut TestAppContext, n: usize, active: usize) -> Opened {
    init(cx);
    init_tokio(cx);
    let (fx, files) = (fixture(T, ""), tempfile::tempdir().unwrap());
    let mut state = SavedState::default();
    state.put_session(WorkspaceSession {
        workspace: fx.ws.dir().to_path_buf(),
        tabs: (0..n)
            .map(|i| TabState {
                connection: "lite".into(),
                draft: format!("d{i}"),
                ..Default::default()
            })
            .collect(),
        active: Some(active),
        ..Default::default()
    });
    save_json(&AppPaths::in_dir(files.path()).state, &state).unwrap();
    let (win, shell) = cx
        .update(|cx| start(services(files.path()), Some(fx.dir.path().into()), cx))
        .unwrap();
    present(cx, win, "connection-list");
    Opened {
        fx,
        files,
        win,
        shell,
    }
}
fn models(cx: &mut TestAppContext, o: &Opened) -> Vec<Entity<TabModel>> {
    o.shell.read_with(cx, |s, cx| {
        s.tabs()
            .iter()
            .map(|t| t.read(cx).model().clone())
            .collect()
    })
}
fn connected(cx: &mut TestAppContext, models: &[Entity<TabModel>]) -> Vec<usize> {
    (0..models.len())
        .filter(|&i| models[i].read_with(cx, |m, _| m.events().is_some()))
        .collect()
}

#[gpui_kit::test]
fn a_large_restore_connects_only_the_focused_tab_within_the_cap(cx: &mut TestAppContext) {
    let o = restored_with(cx, PRECONNECT_CAP + 2, 3);
    let models = models(cx, &o);
    assert_eq!(models.len(), PRECONNECT_CAP + 2);
    assert_eq!(o.shell.read_with(cx, |s, _| s.active()), Some(3));
    let open = connected(cx, &models);
    assert!(open.len() <= PRECONNECT_CAP, "{open:?}");
    assert_eq!(open, [3], "only the focused tab connects at startup");
}

#[gpui_kit::test]
fn a_tab_beyond_the_cap_connects_on_its_first_run(cx: &mut TestAppContext) {
    let o = restored_with(cx, PRECONNECT_CAP + 2, 3);
    let last = PRECONNECT_CAP + 1;
    let m = models(cx, &o)[last].clone();
    assert!(m.read_with(cx, |m, _| m.events().is_none()));
    m.update(cx, |m, cx| m.run("SELECT x FROM t".into(), cx));
    wait_real(cx, "the run", |cx| m.read_with(cx, |m, _| !m.running()));
    m.read_with(cx, |m, _| {
        assert!(m.events().is_some(), "the Run started a session");
        assert_eq!(m.error(), None);
        assert_eq!(m.read_only(), Some(false));
        assert_eq!(m.results()[0].store.cell(0, 0), Some(CellRef::Text("1")));
    });
}
