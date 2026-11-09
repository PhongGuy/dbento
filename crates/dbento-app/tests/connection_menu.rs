//! The connection menu: its order, New query, Refresh schema, Disconnect and Duplicate.
use crate::support::{
    MANY, Opened, click_id, exec, form, label_of, launched, opened, present, wait_real,
};
use dbento_app::catalog::CatalogQuery;
use dbento_app::connection_dialog::Field;
use dbento_app::explorer::{connection_id, part_id};
use dbento_app::link::LinkState;
use dbento_core::RelationKind;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, TestAppContext};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
                 INSERT INTO t VALUES (1, 'b'), (2, 'a');";

fn menu(cx: &mut TestAppContext, win: AnyWindowHandle, conn: &str) {
    cx.update_window(win, |_, w, cx| w.right_click(connection_id(conn), cx))
        .unwrap();
    cx.run_until_parked();
}
fn top(cx: &mut TestAppContext, win: AnyWindowHandle, id: &'static str) -> f32 {
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        f32::from(w.find(id).bounds().top())
    })
    .unwrap()
}
fn sidebar(cx: &mut TestAppContext, o: &Opened) -> gpui_kit::Entity<dbento_app::sidebar::Sidebar> {
    o.shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap())
}
fn link(cx: &mut TestAppContext, o: &Opened, conn: &str) -> LinkState {
    let sidebar = sidebar(cx, o);
    sidebar.read_with(cx, |s, _| s.link(conn))
}

#[gpui_kit::test]
fn session_items_come_first_then_edits_then_delete_then_new(cx: &mut TestAppContext) {
    let o = launched(cx, "", "");
    menu(cx, o.win, "lite");
    let order = [
        "menu-new-query",
        "menu-refresh-schema",
        "menu-disconnect",
        "menu-edit",
        "menu-duplicate",
        "menu-rename",
        "menu-delete",
        "menu-new-connection",
        "menu-new-folder",
    ];
    let tops: Vec<f32> = order.iter().map(|id| top(cx, o.win, id)).collect();
    assert!(
        tops.windows(2).all(|p| p[0] < p[1]),
        "{order:?} at {tops:?}"
    );
    assert!(present(cx, o.win, ("menu-color", 0usize)));
    assert_eq!(
        label_of(cx, o.win, "menu-edit").as_deref(),
        Some("Edit connection")
    );
}

#[gpui_kit::test]
fn new_query_opens_a_tab_on_that_connection(cx: &mut TestAppContext) {
    let o = launched(cx, "", "");
    menu(cx, o.win, "nopg");
    click_id(cx, o.win, "menu-new-query");
    assert_eq!(o.shell.read_with(cx, |s, _| s.tabs().len()), 1);
    assert_eq!(
        label_of(cx, o.win, ("tab-connection", 0usize)).as_deref(),
        Some("nopg")
    );
}

#[gpui_kit::test]
fn refresh_schema_drops_that_connections_cache(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    let sidebar = sidebar(cx, &o);
    let main = [CatalogQuery::Relations {
        schema: "main".into(),
    }];
    let tables = |cx: &mut TestAppContext| {
        sidebar.read_with(cx, |s, _| {
            s.tree().relations("lite", "main").map(|l| l.items().len())
        })
    };
    sidebar.update(cx, |s, cx| s.ensure_loaded("lite", &main, cx));
    wait_real(cx, "one table", |cx| tables(cx) == Some(1));
    exec(&o.fx, "CREATE TABLE u(x INTEGER)");
    menu(cx, o.win, "lite");
    click_id(cx, o.win, "menu-refresh-schema");
    sidebar.update(cx, |s, cx| s.ensure_loaded("lite", &main, cx));
    wait_real(cx, "two tables", |cx| tables(cx) == Some(2));
}

#[gpui_kit::test]
fn disconnect_closes_the_sessions_and_the_next_run_reconnects(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    click_id(cx, o.win, part_id("connection-toggle", "lite"));
    let sidebar = sidebar(cx, &o);
    wait_real(cx, "both sessions", |cx| {
        link(cx, &o, "lite") == LinkState::Connected
            && sidebar.read_with(cx, |s, _| s.catalog_sessions()) == 1
    });
    menu(cx, o.win, "lite");
    click_id(cx, o.win, "menu-disconnect");
    assert_eq!(link(cx, &o, "lite"), LinkState::Idle);
    assert_eq!(sidebar.read_with(cx, |s, _| s.catalog_sessions()), 0);
    assert!(!present(cx, o.win, part_id("connection-status", "lite")));
    let m = o.model(cx);
    assert!(m.read_with(cx, |m, _| m.events().is_none() && !m.closed()));
    o.run(cx, "SELECT 1");
    o.idle(cx);
    assert_eq!(m.read_with(cx, |m, _| m.results().len()), 1);
    assert_eq!(link(cx, &o, "lite"), LinkState::Connected);
}

#[gpui_kit::test]
fn disconnect_while_parked_keeps_the_rows(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let m = o.model(cx);
    m.update(cx, |m, _| m.set_max_rows(Some(1_000)));
    o.run(cx, MANY);
    wait_real(cx, "the park at the cap", |cx| {
        m.read_with(cx, |m, _| m.parked_at_cap().is_some())
    });
    menu(cx, o.win, "lite");
    click_id(cx, o.win, "menu-disconnect");
    m.read_with(cx, |m, _| {
        assert!(!m.running());
        assert_eq!(m.results()[0].store.rows(), 1_000);
        assert!(m.results()[0].store.end().is_some());
    });
}

#[gpui_kit::test]
fn disconnect_with_staged_edits_asks_first(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    cx.run_until_parked();
    for (answer, staged, has_runner) in [("discard-keep", 1, true), ("discard-close", 0, false)] {
        menu(cx, o.win, "lite");
        click_id(cx, o.win, "menu-disconnect");
        wait_real(cx, "the question", |cx| {
            present(cx, o.win, "confirm-dialog")
        });
        click_id(cx, o.win, answer);
        assert_eq!(m.read_with(cx, |m, _| m.edits().len()), staged, "{answer}");
        assert_eq!(
            m.read_with(cx, |m, _| m.events().is_some()),
            has_runner,
            "{answer}"
        );
    }
}

// The apply prompt holds the batch in a transaction; Disconnect rolls it back and must still
// drop the staged edits it said it would.
#[gpui_kit::test]
fn disconnect_during_the_apply_prompt_drops_the_edits(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    cx.run_until_parked();
    click_id(cx, o.win, "save-edits");
    wait_real(cx, "the apply prompt", |cx| {
        present(cx, o.win, "apply-commit")
    });
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.disconnect("lite", w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    wait_real(cx, "the question", |cx| present(cx, o.win, "discard-close"));
    click_id(cx, o.win, "discard-close");
    wait_real(cx, "the disconnect", |cx| {
        m.read_with(cx, |m, _| m.events().is_none())
    });
    assert_eq!(m.read_with(cx, |m, _| m.edits().len()), 0);
}

#[gpui_kit::test]
fn duplicate_opens_a_copy_without_the_secret(cx: &mut TestAppContext) {
    let o = launched(cx, "", "");
    menu(cx, o.win, "nopg");
    click_id(cx, o.win, "menu-duplicate");
    let f = form(cx, &o);
    let text = |cx: &mut TestAppContext, field: Field| {
        f.read_with(cx, |f, cx| f.input(field).read(cx).value().to_string())
    };
    assert_eq!(text(cx, Field::Name), "nopg-copy");
    assert_eq!(text(cx, Field::Host), "127.0.0.1");
    assert_eq!(
        f.read_with(cx, |f, _| f.previous().map(str::to_owned)),
        None
    );
    assert!(f.read_with(cx, |f, cx| f.password(cx)).is_none());
}

#[gpui_kit::test]
fn the_menu_counts_as_open_until_dismissed(cx: &mut TestAppContext) {
    let o = launched(cx, "", "");
    let sidebar = sidebar(cx, &o);
    assert!(!sidebar.read_with(cx, |s, _| s.menu_open()));
    menu(cx, o.win, "lite");
    assert!(sidebar.read_with(cx, |s, _| s.menu_open()));
    cx.update_window(o.win, |_, w, cx| w.press("escape", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(!sidebar.read_with(cx, |s, _| s.menu_open()));
}
