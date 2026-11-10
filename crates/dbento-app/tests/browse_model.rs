//! The tab model's browse state: target, filter, sort and why editing is blocked.
use crate::support;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use crate::support::{edit_target, fixture, init_tokio, spec, wait_real};
use dbento_app::browse::{BrowseEvent, BrowseState, EditBlock};
use dbento_app::runner::Event;
use dbento_app::tab_model::TabModel;
use dbento_core::{Browse, BrowseError, RelationRef};
use gpui_kit::{AppContext as _, Entity, TestAppContext};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
                 INSERT INTO t VALUES (1, 'b'), (2, 'a'), (3, 'c'); \
                 CREATE TABLE plain(a, b);";

fn model(cx: &mut TestAppContext, fx: &support::Fx) -> Entity<TabModel> {
    init_tokio(cx);
    cx.new(|_| TabModel::new(spec(fx, "lite")))
}
fn record(cx: &mut TestAppContext, m: &Entity<TabModel>) -> Rc<RefCell<Vec<String>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    cx.update(|cx| {
        cx.subscribe(m, move |_, e: &BrowseEvent, _| {
            let line = match e {
                BrowseEvent::Run(sql) => format!("run:{sql}"),
                BrowseEvent::Unavailable(msg) => format!("unavailable:{msg}"),
                BrowseEvent::Committed { changes, relation } => {
                    format!(
                        "committed:{changes}:{}.{}",
                        relation.schema(),
                        relation.name()
                    )
                }
                BrowseEvent::Skipped { rows } => format!("skipped:{rows}"),
                // Leaving the browse is the shell's cue to save the session, not a tab action.
                BrowseEvent::Cleared => return,
            };
            sink.borrow_mut().push(line);
        })
        .detach();
    });
    seen
}
fn run_to_end(cx: &mut TestAppContext, m: &Entity<TabModel>, sql: &str) {
    m.update(cx, |m, cx| m.run(sql.into(), cx));
    wait_real(cx, "the run", |cx| m.read_with(cx, |m, _| !m.running()));
}

#[gpui_kit::test]
fn a_landed_target_browses_and_asks_to_run_its_sql(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    let ev = record(cx, &m);
    let rel = RelationRef::new("main", "plain");
    m.update(cx, |m, cx| m.load_target(rel.clone(), cx));
    assert_eq!(
        m.read_with(cx, |m, _| m.target_pending().cloned()),
        Some(rel)
    );
    wait_real(cx, "the browse", |cx| {
        m.read_with(cx, |m, _| m.browse().is_some())
    });
    assert_eq!(
        *ev.borrow(),
        ["run:SELECT rowid, *\nFROM main.plain\nORDER BY rowid ASC"]
    );
    assert_eq!(m.read_with(cx, |m, _| m.target_pending().cloned()), None);
}

#[gpui_kit::test]
fn a_missing_table_is_unavailable_and_does_not_browse(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    let ev = record(cx, &m);
    m.update(cx, |m, cx| {
        m.load_target(RelationRef::new("main", "gone"), cx)
    });
    wait_real(cx, "the answer", |_| !ev.borrow().is_empty());
    assert!(
        ev.borrow()[0].starts_with("unavailable:"),
        "{:?}",
        ev.borrow()
    );
    assert!(m.read_with(cx, |m, _| m.browse().is_none()));
}

#[gpui_kit::test]
fn clearing_before_the_target_lands_drops_it(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    let ev = record(cx, &m);
    m.update(cx, |m, cx| {
        m.load_target(RelationRef::new("main", "t"), cx);
        m.clear_browse(cx);
    });
    // The runner answers the Target before it takes this run, so its reply has landed.
    run_to_end(cx, &m, "SELECT 1");
    assert!(m.read_with(cx, |m, _| m.browse().is_none()));
    assert!(ev.borrow().is_empty(), "{:?}", ev.borrow());
}

#[gpui_kit::test]
fn a_reconnect_before_the_target_lands_still_browses(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    let ev = record(cx, &m);
    let ws = Arc::clone(&fx.ws);
    m.update(cx, |m, cx| {
        m.load_target(RelationRef::new("main", "t"), cx);
        let _closing = m.reconnect(ws, cx);
    });
    wait_real(cx, "the browse", |cx| {
        m.read_with(cx, |m, _| m.browse().is_some())
    });
    assert_eq!(*ev.borrow(), ["run:SELECT *\nFROM main.t\nORDER BY id ASC"]);
}

#[gpui_kit::test]
fn a_disconnect_before_the_target_lands_browses_on_the_next_connect(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    let ev = record(cx, &m);
    m.update(cx, |m, cx| {
        m.load_target(RelationRef::new("main", "t"), cx);
        let _closing = m.disconnect(cx);
    });
    cx.run_until_parked();
    assert!(m.read_with(cx, |m, _| m.target_pending().is_some()));
    m.update(cx, |m, cx| m.connect(cx));
    wait_real(cx, "the browse", |cx| {
        m.read_with(cx, |m, _| m.browse().is_some())
    });
    assert_eq!(*ev.borrow(), ["run:SELECT *\nFROM main.t\nORDER BY id ASC"]);
}

#[gpui_kit::test]
fn filter_and_sort_update_the_state_and_ask_to_run(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    let ev = record(cx, &m);
    assert!(
        !m.update(cx, |m, cx| m.apply_filter("id > 1", cx)),
        "no browse yet"
    );
    assert!(
        !m.update(cx, |m, cx| m.cycle_sort("id", cx)),
        "no browse yet"
    );
    let state = BrowseState::new(edit_target(&fx, "t"), Browse::default()).unwrap();
    m.update(cx, |m, cx| m.set_browse(state, cx));
    assert!(m.update(cx, |m, cx| m.apply_filter("id > 1", cx)));
    assert!(m.update(cx, |m, cx| m.cycle_sort("name", cx)));
    assert!(m.update(cx, |m, cx| m.cycle_sort("name", cx)));
    cx.run_until_parked();
    assert_eq!(
        *ev.borrow(),
        [
            "run:SELECT *\nFROM main.t\nORDER BY id ASC",
            "run:SELECT *\nFROM main.t\nWHERE (\nid > 1\n)\nORDER BY id ASC",
            "run:SELECT *\nFROM main.t\nWHERE (\nid > 1\n)\nORDER BY name ASC, id ASC",
            "run:SELECT *\nFROM main.t\nWHERE (\nid > 1\n)\nORDER BY name DESC, id ASC",
        ]
    );
    // A rejected filter runs nothing and keeps the state it had.
    assert!(!m.update(cx, |m, cx| m.apply_filter("id > 1) /*", cx)));
    cx.run_until_parked();
    assert_eq!(ev.borrow().len(), 4);
    m.read_with(cx, |m, _| {
        let want = BrowseError::Unterminated.to_string();
        assert_eq!(m.filter_error(), Some(want.as_str()));
        assert_eq!(
            m.browse().unwrap().browse().filter.as_deref(),
            Some("id > 1")
        );
    });
    assert!(m.update(cx, |m, cx| m.apply_filter("", cx)));
    assert_eq!(
        m.read_with(cx, |m, _| m.filter_error().map(str::to_owned)),
        None
    );
    m.update(cx, |m, cx| m.clear_browse(cx));
    assert!(m.read_with(cx, |m, _| m.browse().is_none() && m.edit_block().is_none()));
}

#[gpui_kit::test]
fn the_edit_block_waits_for_a_result_and_names_a_foreign_run(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    let state = BrowseState::new(edit_target(&fx, "t"), Browse::default()).unwrap();
    m.update(cx, |m, cx| m.set_browse(state, cx));
    assert_eq!(
        m.read_with(cx, |m, _| m.edit_block()),
        None,
        "no result yet"
    );
    // Nothing subscribes here, so the browse SQL is run by hand.
    run_to_end(cx, &m, "SELECT *\nFROM main.t\nORDER BY id ASC");
    assert_eq!(m.read_with(cx, |m, _| m.edit_block()), None);
    run_to_end(cx, &m, "SELECT 1");
    assert_eq!(
        m.read_with(cx, |m, _| m.edit_block()),
        Some(EditBlock::NotBrowseRun)
    );
}

#[gpui_kit::test]
fn a_stopped_runner_makes_a_pending_target_unavailable(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    let ev = record(cx, &m);
    m.update(cx, |m, cx| {
        m.load_target(RelationRef::new("main", "t"), cx);
        m.apply(Event::Stopped {
            message: "session stopped".into(),
        });
        m.flush_browse(cx);
    });
    cx.run_until_parked();
    assert_eq!(*ev.borrow(), ["unavailable:session stopped"]);
    assert_eq!(m.read_with(cx, |m, _| m.target_pending().cloned()), None);
    // The next run starts a fresh runner; nothing late may turn the tab into a browse tab.
    run_to_end(cx, &m, "SELECT 1");
    assert!(m.read_with(cx, |m, _| m.browse().is_none()));
    assert_eq!(ev.borrow().len(), 1);
}

#[gpui_kit::test]
fn a_target_asked_for_during_a_run_stops_the_run_and_browses(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    let ev = record(cx, &m);
    m.update(cx, |m, cx| m.run(support::LONG.into(), cx));
    wait_real(cx, "the statement to start", |cx| {
        m.read_with(cx, |m, _| m.running() && !m.results().is_empty())
    });
    m.update(cx, |m, cx| m.load_target(RelationRef::new("main", "t"), cx));
    wait_real(cx, "the browse", |cx| {
        m.read_with(cx, |m, _| m.browse().is_some())
    });
    assert_eq!(*ev.borrow(), ["run:SELECT *\nFROM main.t\nORDER BY id ASC"]);
}

#[gpui_kit::test]
fn cancel_drops_a_loading_target_and_interrupts_its_read(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    let ev = record(cx, &m);
    m.update(cx, |m, cx| m.connect(cx));
    wait_real(cx, "the session", |cx| {
        m.read_with(cx, |m, _| m.read_only().is_some())
    });
    m.update(cx, |m, cx| {
        m.load_target(RelationRef::new("main", "t"), cx);
        m.cancel();
    });
    assert_eq!(m.read_with(cx, |m, _| m.target_pending().cloned()), None);
    assert_eq!(m.read_with(cx, |m, _| m.cancels_fired()), 1);
    // The interrupted answer lands as nothing: the tab neither browses nor falls back.
    run_to_end(cx, &m, "SELECT 1");
    assert!(m.read_with(cx, |m, _| m.browse().is_none()));
    assert!(ev.borrow().is_empty(), "{:?}", ev.borrow());
}

#[gpui_kit::test]
fn a_target_that_stops_a_script_starts_none_of_its_later_statements(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    m.update(cx, |m, cx| m.connect(cx));
    wait_real(cx, "the session", |cx| {
        m.read_with(cx, |m, _| m.read_only().is_some())
    });
    // Both land before the runner looks: a cancel fired this early is spent by the first
    // statement's settle, so only a Stop keeps the INSERT from running.
    m.update(cx, |m, cx| {
        m.run("SELECT 1; INSERT INTO t VALUES (9, 'z')".into(), cx);
        m.load_target(RelationRef::new("main", "t"), cx);
    });
    wait_real(cx, "the browse", |cx| {
        m.read_with(cx, |m, _| m.browse().is_some())
    });
    assert_eq!(fx.count("SELECT count(*) FROM t"), 3);
}

#[gpui_kit::test]
fn cancel_drops_a_target_queued_behind_a_run(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    let ev = record(cx, &m);
    m.update(cx, |m, cx| m.run(support::LONG.into(), cx));
    wait_real(cx, "the statement to start", |cx| {
        m.read_with(cx, |m, _| m.running() && !m.results().is_empty())
    });
    m.update(cx, |m, cx| {
        m.load_target(RelationRef::new("main", "t"), cx);
        m.cancel();
    });
    assert_eq!(m.read_with(cx, |m, _| m.target_pending().cloned()), None);
    wait_real(cx, "the run to stop", |cx| {
        m.read_with(cx, |m, _| !m.running())
    });
    run_to_end(cx, &m, "SELECT 1");
    assert!(m.read_with(cx, |m, _| m.browse().is_none()));
    assert!(ev.borrow().is_empty(), "{:?}", ev.borrow());
}

#[gpui_kit::test]
fn a_run_replaces_a_target_queued_behind_the_run_before(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    let ev = record(cx, &m);
    m.update(cx, |m, cx| m.run(support::LONG.into(), cx));
    wait_real(cx, "the statement to start", |cx| {
        m.read_with(cx, |m, _| m.running() && !m.results().is_empty())
    });
    m.update(cx, |m, cx| m.load_target(RelationRef::new("main", "t"), cx));
    run_to_end(cx, &m, "SELECT 1");
    cx.run_until_parked();
    assert_eq!(m.read_with(cx, |m, _| m.target_pending().cloned()), None);
    assert!(m.read_with(cx, |m, _| m.browse().is_none()));
    assert!(ev.borrow().is_empty(), "{:?}", ev.borrow());
}
