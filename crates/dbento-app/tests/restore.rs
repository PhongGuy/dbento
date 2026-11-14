//! Restore planning and the saved sessions: which tabs come back, in what order.
use dbento_app::app::{MAX_SESSIONS, SavedState, TabState, WorkspaceSession};
use dbento_app::drafts::Draft;
use dbento_app::restore::{RestoredTab, plan_restore};
use std::path::{Path, PathBuf};

fn tab(conn: &str, draft: &str, file: Option<&str>) -> TabState {
    TabState {
        connection: conn.into(),
        draft: draft.into(),
        file: file.map(PathBuf::from),
        table: None,
        number: None,
    }
}
fn draft(id: &str, conn: &str, sql: &str, at: u64) -> Draft {
    let mut d = Draft::new(id, Path::new("/ws"), conn, None, sql);
    d.saved_at_ms = at;
    d
}
fn restored(conn: &str, id: &str, file: Option<&str>, sql: Option<&str>) -> RestoredTab {
    RestoredTab {
        connection: conn.into(),
        draft: id.into(),
        file: file.map(PathBuf::from),
        sql: sql.map(str::to_owned),
        label: None,
        table: None,
        number: None,
    }
}

#[test]
fn the_session_order_comes_back_with_drafts_then_orphans_oldest_first() {
    let session = WorkspaceSession {
        workspace: "/ws".into(),
        tabs: vec![
            tab("lite", "a", Some("queries/lite/a.sql")),
            tab("gone", "b", None),
            tab("pg", "c", None),
        ],
        active: Some(2),
        ..Default::default()
    };
    let drafts = vec![
        draft("a", "lite", "SELECT edited", 5),
        draft("b", "gone", "SELECT lost connection", 6),
        draft("z", "pg", "SELECT newer orphan", 9),
        draft("y", "lite", "SELECT older orphan", 7),
        draft("x", "gone", "SELECT orphan on a gone connection", 8),
    ];
    let (tabs, active) = plan_restore(Some(&session), drafts, &["lite", "pg"]);
    assert_eq!(
        tabs,
        [
            restored(
                "lite",
                "a",
                Some("queries/lite/a.sql"),
                Some("SELECT edited")
            ),
            restored("pg", "c", None, None),
            restored("lite", "y", None, Some("SELECT older orphan")),
            restored("pg", "z", None, Some("SELECT newer orphan")),
        ]
    );
    assert_eq!(active, Some(1));
}

#[test]
fn a_skipped_active_tab_activates_the_first_and_nothing_gives_none() {
    let session = WorkspaceSession {
        workspace: "/ws".into(),
        tabs: vec![tab("lite", "a", None), tab("gone", "b", None)],
        active: Some(1),
        ..Default::default()
    };
    assert_eq!(plan_restore(Some(&session), vec![], &["lite"]).1, Some(0));
    assert_eq!(plan_restore(None, vec![], &["lite"]), (vec![], None));
}

#[test]
fn put_session_moves_it_first_and_keeps_the_newest_ten() {
    let mut s = SavedState::default();
    for i in 0..12 {
        s.put_session(WorkspaceSession {
            workspace: format!("/w{i}").into(),
            ..Default::default()
        });
    }
    assert_eq!(s.sessions.len(), MAX_SESSIONS);
    assert_eq!(s.sessions[0].workspace, Path::new("/w11"));
    s.put_session(WorkspaceSession {
        workspace: "/w5".into(),
        tabs: vec![tab("lite", "a", None)],
        active: Some(0),
        ..Default::default()
    });
    assert_eq!(
        (s.sessions.len(), s.sessions[0].workspace.as_path()),
        (MAX_SESSIONS, Path::new("/w5"))
    );
    assert_eq!(s.session(Path::new("/w5")).unwrap().tabs.len(), 1);
    assert!(s.session(Path::new("/w0")).is_none());
}

#[test]
fn an_m3e_state_file_loads_with_no_sessions() {
    let s: SavedState =
        serde_json::from_str(r#"{"last_workspace":"/a","appearance":"dark"}"#).unwrap();
    assert!(s.sessions.is_empty());
    assert_eq!(s.last_workspace.as_deref(), Some(Path::new("/a")));
}
