//! Opening a browse tab filtered to a key: a new preview tab, a hop inside it, and the origin's
//! staged edits left alone.
use crate::support::{Opened, launched, present, wait_real};
use dbento_app::runner::StatementEnd;
use dbento_core::{RelationKind, RelationRef};
use gpui_kit::{AppContext as _, TestAppContext};

const T: &str = "CREATE TABLE a(id INTEGER PRIMARY KEY, name TEXT); \
                 CREATE TABLE b(id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a(id), note TEXT); \
                 INSERT INTO a VALUES (1, 'x'), (2, 'y'); \
                 INSERT INTO b VALUES (1, 1, 'n'), (2, NULL, 'm'), (3, 1, 'o');";
const B_SQL: &str = "SELECT *\nFROM main.b\nORDER BY id ASC";

fn b(cx: &mut TestAppContext) -> Opened {
    let o = launched(cx, T, "");
    o.open_relation(cx, "b", RelationKind::Table);
    o.wait_browse(cx, B_SQL);
    o
}
fn open_filtered(cx: &mut TestAppContext, o: &Opened, conn: &str, rel: &str, filter: &str) -> bool {
    let (shell, rel, filter, conn) = (
        o.shell.clone(),
        RelationRef::new("main", rel),
        filter.to_owned(),
        conn.to_owned(),
    );
    let opened = cx
        .update_window(o.win, |_, w, cx| {
            shell.update(cx, |s, cx| s.open_filtered(&conn, &rel, filter, w, cx))
        })
        .unwrap();
    cx.run_until_parked();
    opened
}
fn tabs(cx: &mut TestAppContext, o: &Opened) -> usize {
    o.shell.read_with(cx, |s, _| s.tabs().len())
}
fn filter_text(cx: &mut TestAppContext, o: &Opened) -> String {
    o.shell.read_with(cx, |s, cx| {
        let pane = s.tab().unwrap().read(cx).results_pane().read(cx);
        pane.filter_input().read(cx).value().to_string()
    })
}

#[gpui_kit::test]
fn a_new_tab_browses_the_referenced_table_filtered(cx: &mut TestAppContext) {
    let o = b(cx);
    assert!(open_filtered(cx, &o, "lite", "a", "id = '1'"));
    assert_eq!(tabs(cx, &o), 2);
    o.wait_browse(
        cx,
        "SELECT *\nFROM main.a\nWHERE (\nid = '1'\n)\nORDER BY id ASC",
    );
    let m = o.model(cx);
    wait_real(cx, "one row", |cx| {
        m.read_with(cx, |m, _| !m.running() && m.results()[0].store.rows() == 1)
    });
    assert_eq!(filter_text(cx, &o), "id = '1'");
}

#[gpui_kit::test]
fn a_follow_from_a_preview_hops_inside_it(cx: &mut TestAppContext) {
    let o = b(cx);
    assert!(open_filtered(cx, &o, "lite", "a", "id = '1'"));
    o.wait_browse(
        cx,
        "SELECT *\nFROM main.a\nWHERE (\nid = '1'\n)\nORDER BY id ASC",
    );
    assert!(open_filtered(cx, &o, "lite", "b", "a_id = '1'"));
    assert_eq!(tabs(cx, &o), 2);
    assert_eq!(o.shell.read_with(cx, |s, _| s.active()), Some(1));
    o.wait_browse(
        cx,
        "SELECT *\nFROM main.b\nWHERE (\na_id = '1'\n)\nORDER BY id ASC",
    );
    let m = o.model(cx);
    wait_real(cx, "two rows", |cx| {
        m.read_with(cx, |m, _| !m.running() && m.results()[0].store.rows() == 2)
    });
}

// The follow lands in a preview, so the origin's staged edits stay and nothing asks.
#[gpui_kit::test]
fn a_follow_never_asks_about_the_origins_edits(cx: &mut TestAppContext) {
    let o = b(cx);
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 2, "z", cx)));
    assert!(open_filtered(cx, &o, "lite", "b", "id = '3'"));
    assert_eq!(tabs(cx, &o), 2);
    assert!(!present(cx, o.win, "confirm-dialog"));
    m.read_with(cx, |m, _| {
        assert!(m.discard_question().is_none());
        assert_eq!(m.edits().len(), 1);
    });
}

#[gpui_kit::test]
fn an_unknown_connection_opens_nothing(cx: &mut TestAppContext) {
    let o = b(cx);
    assert!(!open_filtered(cx, &o, "nosuch", "a", "id = '1'"));
    assert_eq!(tabs(cx, &o), 1);
}

// A NUL inside a text key reaches the server, which rejects the filter; the tab shows that
// failure instead of an empty result.
#[gpui_kit::test]
fn a_key_the_server_rejects_shows_an_error(cx: &mut TestAppContext) {
    let o = launched(
        cx,
        "CREATE TABLE k(id TEXT PRIMARY KEY); INSERT INTO k VALUES ('a' || char(0) || 'b');",
        "",
    );
    assert!(open_filtered(cx, &o, "lite", "k", "id = 'a\0b'"));
    let m = o.model(cx);
    wait_real(cx, "the browse ended", |cx| {
        m.read_with(cx, |m, _| {
            !m.running() && m.results().first().is_some_and(|r| r.store.end().is_some())
        })
    });
    m.read_with(cx, |m, _| {
        let end = m.results()[0].store.end();
        assert!(matches!(end, Some(StatementEnd::Failed(_))), "{end:?}");
    });
}
