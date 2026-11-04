//! A table tab across a restart: its title, its explorer selection and its browse state.
mod support;
use dbento_app::app::{AppPaths, SavedState, start};
use dbento_app::persist::{load_json, save_json};
use dbento_app::schema_tree::NodeKey;
use dbento_app::shell::Shell;
use dbento_core::{RelationKind, RelationRef, Sort};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};
use support::{Opened, label_of, launched, present, wait_real};

const T: &str = "CREATE TABLE Album(AlbumId INTEGER PRIMARY KEY, Title TEXT NOT NULL); \
                 INSERT INTO Album(Title) VALUES ('a'), ('b'), ('c');";
const BROWSED: &str =
    "SELECT *\nFROM main.Album\nWHERE (\nAlbumId > 1\n)\nORDER BY Title DESC, AlbumId ASC";

/// A second start on the same files and workspace: what the next launch sees.
fn restart(cx: &mut TestAppContext, o: &Opened) -> (AnyWindowHandle, Entity<Shell>) {
    let (win, shell) = cx
        .update(|cx| {
            start(
                support::services(o.files.path()),
                Some(o.fx.dir.path().into()),
                cx,
            )
        })
        .unwrap();
    assert!(present(cx, win, "connection-list"));
    (win, shell)
}

/// The active tab's title, origin, editor text and run count.
fn active(
    cx: &mut TestAppContext,
    shell: &Entity<Shell>,
) -> (String, Option<RelationRef>, String, u64) {
    shell.read_with(cx, |s, cx| {
        let t = s.tab().unwrap().read(cx);
        (
            t.title(),
            t.origin().cloned(),
            t.sql(cx),
            t.model().read(cx).run_id(),
        )
    })
}

/// The active tab's browse filter, sort and SQL once a browse has run.
fn browse(
    cx: &mut TestAppContext,
    shell: &Entity<Shell>,
) -> Option<(Option<String>, Option<Sort>, String)> {
    shell.read_with(cx, |s, cx| {
        let t = s.tab()?.read(cx);
        let m = t.model().read(cx);
        let b = m.browse()?;
        let ended = m.results().first()?.store.end().is_some();
        ended.then(|| {
            (
                b.browse().filter.clone(),
                b.browse().sort.clone(),
                t.sql(cx),
            )
        })
    })
}

fn album() -> RelationRef {
    RelationRef::new("main", "Album")
}

fn run_all(cx: &mut TestAppContext, win: AnyWindowHandle) {
    cx.update_window(win, |_, w, cx| w.click("run-all", cx))
        .unwrap();
    cx.run_until_parked();
}

/// Saves `tabs` (raw JSON tab entries) as the workspace's only session.
fn write_session(o: &Opened, tabs: serde_json::Value) {
    let path = AppPaths::in_dir(o.files.path()).state;
    let mut state = load_json::<SavedState>(&path).value;
    let raw = serde_json::json!({ "workspace": o.fx.ws.dir(), "tabs": tabs, "active": 0 });
    state.sessions = vec![serde_json::from_value(raw).unwrap()];
    save_json(&path, &state).unwrap();
}

#[gpui_kit::test]
fn a_restored_table_tab_keeps_its_title_table_and_browse_state(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "Album", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.Album\nORDER BY AlbumId ASC");
    let m = o.model(cx);
    m.update(cx, |m, cx| m.apply_filter("AlbumId > 1", cx));
    wait_real(cx, "the filter", |cx| {
        browse(cx, &o.shell).is_some_and(|b| b.0.is_some())
    });
    m.update(cx, |m, cx| m.set_sort(Some(Sort::new("Title", true)), cx));
    wait_real(cx, "the sort", |cx| {
        browse(cx, &o.shell).is_some_and(|b| b.2 == BROWSED)
    });

    let (win, shell) = restart(cx, &o);
    let (title, origin, sql, runs) = active(cx, &shell);
    assert_eq!(title, "main.Album");
    assert_eq!(origin, Some(album()));
    assert_eq!(runs, 0, "restore runs nothing");
    assert!(!sql.is_empty(), "the editor holds the table's select");
    assert_eq!(
        label_of(cx, win, ("tab", 0usize)).as_deref(),
        Some("main.Album")
    );
    let selected = shell.read_with(cx, |s, cx| s.sidebar().unwrap().read(cx).selected_node());
    assert_eq!(
        selected,
        Some(NodeKey::Relation {
            conn: "lite".into(),
            rel: album()
        })
    );

    // The first run browses again with the saved filter and sort, so editing is available.
    run_all(cx, win);
    wait_real(cx, "the restored browse", |cx| browse(cx, &shell).is_some());
    assert_eq!(
        browse(cx, &shell),
        Some((
            Some("AlbumId > 1".into()),
            Some(Sort::new("Title", true)),
            BROWSED.into()
        ))
    );
}

#[gpui_kit::test]
fn a_session_without_a_table_field_restores_a_plain_query_tab(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    // The format written before table tabs were saved: connection, draft and file only.
    write_session(
        &o,
        serde_json::json!([{ "connection": "lite", "draft": "d1", "file": null }]),
    );
    let (_, shell) = restart(cx, &o);
    let (title, origin, _, runs) = active(cx, &shell);
    assert_eq!((title.as_str(), origin, runs), ("Query 1", None, 0));
}

#[gpui_kit::test]
fn a_restored_table_that_is_gone_runs_as_a_plain_query(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    write_session(
        &o,
        serde_json::json!([{
            "connection": "lite",
            "draft": "d1",
            "table": { "schema": "main", "name": "gone", "browse": { "filter": null, "sort": null } }
        }]),
    );
    let (win, shell) = restart(cx, &o);
    assert_eq!(active(cx, &shell).0, "main.gone");
    run_all(cx, win);
    let want = "Opened as a plain query, so rows cannot be edited here: \
                no table or view main.gone";
    wait_real(cx, "the notice", |cx| {
        label_of(cx, win, "notice").as_deref() == Some(want)
    });
    let (_, _, sql, runs) = active(cx, &shell);
    assert_eq!(sql, "SELECT *\nFROM main.gone");
    assert!(runs > 0, "the plain query ran");
    assert!(shell.read_with(cx, |s, cx| {
        s.tab()
            .unwrap()
            .read(cx)
            .model()
            .read(cx)
            .browse()
            .is_none()
    }));
}
