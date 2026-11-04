//! "Discard N pending changes?" before a run, a filter or sort change or typing drops a
//! browse tab's staged edits; an abandoned save's prompt closes.
mod support;
use dbento_app::tab_model::Discard;
use dbento_core::RelationKind;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use support::{Opened, fixture, label_of, launched, present, ready, wait_real, wait_unlocked};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
                 INSERT INTO t VALUES (1, 'b'), (2, 'a');";
const T_SQL: &str = "SELECT *\nFROM main.t\nORDER BY id ASC";

fn t(cx: &mut TestAppContext) -> Opened {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, T_SQL);
    o
}
fn stage_z(cx: &mut TestAppContext, o: &Opened) {
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    cx.run_until_parked();
}
fn click(cx: &mut TestAppContext, o: &Opened, id: &'static str) {
    cx.update_window(o.win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}
fn type_in_editor(cx: &mut TestAppContext, o: &Opened, text: &str) {
    let e = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone());
    cx.update_window(o.win, |_, w, cx| {
        e.update(cx, |e, cx| e.focus(w, cx));
        w.input(text, cx);
    })
    .unwrap();
    cx.run_until_parked();
}
fn dialog(cx: &mut TestAppContext, o: &Opened) {
    wait_real(cx, "the question", |cx| {
        present(cx, o.win, "confirm-dialog")
    });
}

#[gpui_kit::test]
fn a_run_with_staged_edits_asks_first(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    let run = m.read_with(cx, |m, _| m.run_id());
    assert!(!m.update(cx, |m, cx| m.request_run("SELECT 1".into(), cx)));
    let q = m
        .read_with(cx, |m, _| m.discard_question().cloned())
        .unwrap();
    assert_eq!(q.title, "Discard 1 pending change?");
    assert_eq!(q.action, Discard::Run("SELECT 1".into()));
    assert_eq!(m.read_with(cx, |m, _| m.run_id()), run, "nothing ran");
    assert!(
        !m.update(cx, |m, cx| m.request_run("SELECT 2".into(), cx)),
        "a second request waits"
    );
    assert_eq!(
        m.read_with(cx, |m, _| m.discard_question().unwrap().id),
        q.id
    );
    assert_eq!(
        m.update(cx, |m, cx| m.answer_discard(q.id, false, cx)),
        Some(Discard::Run("SELECT 1".into()))
    );
    m.read_with(cx, |m, _| {
        assert!(m.discard_question().is_none());
        assert_eq!(m.edits().len(), 1);
        assert_eq!(m.run_id(), run);
    });
    assert!(!m.update(cx, |m, cx| m.request_run("SELECT 1".into(), cx)));
    let id = m.read_with(cx, |m, _| m.discard_question().unwrap().id);
    assert_eq!(
        m.update(cx, |m, cx| m.answer_discard(id + 1, true, cx)),
        None
    );
    m.update(cx, |m, cx| m.answer_discard(id, true, cx));
    m.read_with(cx, |m, _| {
        assert!(m.edits().is_empty());
        assert!(m.run_id() > run);
        assert!(!m.can_undo());
    });
}

#[gpui_kit::test]
fn nothing_staged_runs_at_once(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    let run = m.read_with(cx, |m, _| m.run_id());
    assert!(m.update(cx, |m, cx| m.request_run("SELECT 1".into(), cx)));
    m.read_with(cx, |m, _| {
        assert!(m.discard_question().is_none());
        assert!(m.run_id() > run);
    });
}

#[gpui_kit::test]
fn filter_and_sort_ask_while_edits_are_staged(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    assert!(!m.update(cx, |m, cx| m.apply_filter("id > 1", cx)));
    let q = m
        .read_with(cx, |m, _| m.discard_question().cloned())
        .unwrap();
    assert_eq!(q.action, Discard::Filter("id > 1".into()));
    assert_eq!(
        m.read_with(cx, |m, _| m.browse().unwrap().browse().filter.clone()),
        None
    );
    m.update(cx, |m, cx| m.answer_discard(q.id, true, cx));
    wait_real(cx, "the filtered browse", |cx| {
        m.read_with(cx, |m, _| {
            m.editable() && !m.running() && m.results().first().is_some_and(|r| r.store.rows() == 1)
        })
    });
    assert_eq!(
        m.read_with(cx, |m, _| m.browse().unwrap().browse().filter.clone())
            .as_deref(),
        Some("id > 1")
    );
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "q", cx)));
    assert!(!m.update(cx, |m, cx| m.cycle_sort("name", cx)));
    let q = m
        .read_with(cx, |m, _| m.discard_question().cloned())
        .unwrap();
    assert_eq!(q.action, Discard::Sort("name".into()));
    m.update(cx, |m, cx| m.answer_discard(q.id, false, cx));
    m.read_with(cx, |m, _| {
        assert!(m.browse().unwrap().browse().sort.is_none());
        assert_eq!(m.edits().len(), 1);
    });
}

#[gpui_kit::test]
fn a_refused_filter_or_sort_keeps_the_edits_and_asks_nothing(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    assert!(!m.update(cx, |m, cx| m.apply_filter("name = 'x", cx)));
    m.read_with(cx, |m, _| {
        assert!(
            m.discard_question().is_none(),
            "an open quote is refused before asking"
        );
        assert!(m.filter_error().is_some());
        assert_eq!(m.edits().len(), 1);
    });
    assert!(!m.update(cx, |m, cx| m.apply_filter("id = 1; DELETE FROM t", cx)));
    assert!(!m.update(cx, |m, cx| m.cycle_sort("nope", cx)));
    m.read_with(cx, |m, _| {
        assert!(m.discard_question().is_none());
        assert_eq!(m.edits().len(), 1);
        assert!(m.browse().unwrap().browse().filter.is_none());
        assert!(m.browse().unwrap().browse().sort.is_none());
    });
}

#[gpui_kit::test]
fn clearing_the_browse_asks_and_keep_holds_it(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    assert!(!m.update(cx, |m, cx| m.request_clear_browse(cx)));
    let q = m
        .read_with(cx, |m, _| m.discard_question().cloned())
        .unwrap();
    assert_eq!(q.action, Discard::Edit);
    m.update(cx, |m, cx| m.answer_discard(q.id, false, cx));
    m.read_with(cx, |m, _| {
        assert!(m.browse().is_some());
        assert_eq!(m.edits().len(), 1);
    });
    assert!(!m.update(cx, |m, cx| m.request_clear_browse(cx)));
    let id = m.read_with(cx, |m, _| m.discard_question().unwrap().id);
    m.update(cx, |m, cx| m.answer_discard(id, true, cx));
    m.read_with(cx, |m, _| {
        assert!(m.browse().is_none());
        assert!(m.edits().is_empty());
    });
}

#[gpui_kit::test]
fn typing_in_the_editor_asks_and_keep_restores_the_browse_sql(cx: &mut TestAppContext) {
    let o = t(cx);
    stage_z(cx, &o);
    type_in_editor(cx, &o, "x");
    dialog(cx, &o);
    assert_eq!(
        label_of(cx, o.win, "confirm-detail").as_deref(),
        Some("They have not been saved to the database.")
    );
    click(cx, &o, "discard-keep");
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    assert_eq!(tab.read_with(cx, |t, cx| t.sql(cx)), T_SQL);
    let m = o.model(cx);
    m.read_with(cx, |m, _| {
        assert!(m.browse().is_some());
        assert_eq!(m.edits().len(), 1);
    });
    type_in_editor(cx, &o, "x");
    dialog(cx, &o);
    click(cx, &o, "discard-edits");
    m.read_with(cx, |m, _| {
        assert!(m.browse().is_none());
        assert!(m.edits().is_empty());
    });
}

#[gpui_kit::test]
fn the_run_button_asks_before_dropping_edits(cx: &mut TestAppContext) {
    let o = t(cx);
    stage_z(cx, &o);
    click(cx, &o, "run-all");
    dialog(cx, &o);
    click(cx, &o, "discard-keep");
    let m = o.model(cx);
    assert_eq!(m.read_with(cx, |m, _| m.edits().len()), 1);
    click(cx, &o, "run-all");
    dialog(cx, &o);
    click(cx, &o, "discard-edits");
    wait_real(cx, "the re-run", |cx| {
        m.read_with(cx, |m, _| {
            m.edits().is_empty() && !m.running() && m.editable()
        })
    });
}

#[gpui_kit::test]
fn keep_after_a_filter_puts_the_applied_filter_back(cx: &mut TestAppContext) {
    let o = t(cx);
    stage_z(cx, &o);
    let p = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).results_pane().clone());
    let input = p.read_with(cx, |p, _| p.filter_input().clone());
    cx.update_window(o.win, |_, w, cx| {
        input.update(cx, |s, cx| s.focus(w, cx));
        w.input("id > 1", cx);
        w.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    dialog(cx, &o);
    click(cx, &o, "discard-keep");
    assert_eq!(input.read_with(cx, |s, _| s.value().to_string()), "");
    assert_eq!(o.model(cx).read_with(cx, |m, _| m.edits().len()), 1);
}

#[gpui_kit::test]
fn an_abandoned_save_closes_its_prompt(cx: &mut TestAppContext) {
    let o = t(cx);
    stage_z(cx, &o);
    click(cx, &o, "save-edits");
    dialog(cx, &o);
    o.model(cx).update(cx, |m, cx| m.run("SELECT 1".into(), cx));
    cx.run_until_parked();
    assert!(!present(cx, o.win, "confirm-dialog"));
    wait_unlocked(cx, &o.fx);
}
