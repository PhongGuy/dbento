//! Notices: the kit's toast layer under the app root, and the commit notice.
mod support;
use std::time::Duration;

use dbento_app::staged::{committed_text, pending_label};
use dbento_app::ui::{Notice, NoticeKind, notice};
use dbento_core::{RelationKind, RelationRef};
use gpui_kit::{AppContext as _, TestAppContext};
use support::{TREE, label_of, launched, present, started, wait_idle, wait_prompt, wait_real};

fn hello() -> Notice {
    Notice {
        text: "Hello there".into(),
        kind: NoticeKind::Info,
    }
}

#[test]
fn the_commit_notice_counts_changes() {
    let rel = RelationRef::new("main", "orders");
    assert_eq!(committed_text(1, &rel), "Committed 1 change to main.orders");
    assert_eq!(
        committed_text(2, &rel),
        "Committed 2 changes to main.orders"
    );
}

#[gpui_kit::test]
fn a_notice_shows_its_text_then_hides(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    assert!(!present(cx, s.win, "notice"));
    cx.update_window(s.win, |_, w, cx| notice(w, cx, hello()))
        .unwrap();
    assert_eq!(
        label_of(cx, s.win, "notice").as_deref(),
        Some("Hello there")
    );
    cx.executor().advance_clock(Duration::from_secs(4));
    assert!(present(cx, s.win, "notice"), "still up before 5 s");
    // The kit hides a toast after 5 s, then plays its exit.
    for _ in 0..2 {
        cx.executor().advance_clock(Duration::from_secs(6));
        cx.run_until_parked();
    }
    assert!(!present(cx, s.win, "notice"));
}

// A second toast with the same text would make the lookup below ambiguous, which panics.
#[gpui_kit::test]
fn the_same_text_twice_is_one_notice(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    for _ in 0..2 {
        cx.update_window(s.win, |_, w, cx| notice(w, cx, hello()))
            .unwrap();
    }
    assert_eq!(
        label_of(cx, s.win, "notice").as_deref(),
        Some("Hello there")
    );
}

// Two toasts up at once must not share an element id, or the lookup would panic.
#[gpui_kit::test]
fn the_newest_of_two_notices_is_the_one_named_notice(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    cx.update_window(s.win, |_, w, cx| notice(w, cx, hello()))
        .unwrap();
    let later = Notice {
        text: "General Kenobi".into(),
        kind: NoticeKind::Success,
    };
    cx.update_window(s.win, |_, w, cx| notice(w, cx, later))
        .unwrap();
    assert_eq!(
        label_of(cx, s.win, "notice").as_deref(),
        Some("General Kenobi")
    );
}

#[gpui_kit::test]
fn a_commit_counts_rows_like_the_pending_pill(cx: &mut TestAppContext) {
    let o = launched(
        cx,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, note TEXT); \
         INSERT INTO t VALUES (1, 'a', 'p'), (2, 'b', 'q');",
        "",
    );
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    let m = o.model(cx);
    // Two cells of row 0 and one of row 1: three cell edits, two changed rows.
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "x", cx)));
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 2, "y", cx)));
    assert!(m.update(cx, |m, cx| m.stage_cell(1, 1, "z", cx)));
    assert_eq!(
        m.read_with(cx, |m, _| pending_label(m.edits().len())),
        "2 pending changes"
    );
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_prompt(cx, &m);
    m.update(cx, |m, cx| m.commit(cx));
    wait_idle(cx, &m);
    wait_real(cx, "the commit notice", |cx| {
        label_of(cx, o.win, "notice").as_deref() == Some("Committed 2 changes to main.t")
    });
}
