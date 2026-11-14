//! The footer of an expired browse offers to continue in place, and a sorted continuation
//! says rows may have moved.
use crate::support::{Opened, label_of, launched, present, wait_real};
use dbento_app::results_pane::{MORE_ROWS_RELEASED, fetch_next_label, skip_notice};
use dbento_app::runner::Event;
use dbento_core::RelationKind;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

const BIG: &str = "CREATE TABLE big(id INTEGER PRIMARY KEY, v TEXT); \
    WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 5000) \
    INSERT INTO big SELECT x, 'v' FROM r;";

/// `big` opened as the explorer does, parked at the default 1,000-row cap, then expired.
fn expired_big(cx: &mut TestAppContext, sort: Option<&str>) -> Opened {
    let o = launched(cx, BIG, "");
    o.open_relation(cx, "big", RelationKind::Table);
    let m = o.model(cx);
    wait_real(cx, "the park at the cap", |cx| {
        m.read_with(cx, |m, _| m.parked_at_cap().is_some())
    });
    if let Some(column) = sort {
        assert!(
            m.update(cx, |m, cx| m.cycle_sort(column, cx)),
            "the sort cycles"
        );
        wait_real(cx, "the sorted park", |cx| {
            m.read_with(cx, |m, _| {
                m.parked_at_cap().is_some()
                    && m.results()[0]
                        .sql
                        .ends_with(&format!("ORDER BY {column} ASC, id ASC"))
            })
        });
    }
    m.update(cx, |m, cx| {
        let run_id = m.run_id();
        m.apply(Event::ParkExpired {
            run_id,
            statement_timeout: false,
        });
        m.cancel();
        cx.notify();
    });
    wait_real(cx, "the end", |cx| {
        m.read_with(cx, |m, _| m.continuation().is_some())
    });
    o
}

#[test]
fn the_continuing_note_reads_as_the_spec_says() {
    assert_eq!(
        MORE_ROWS_RELEASED,
        "More rows available (connection released)"
    );
}

#[gpui_kit::test]
fn fetch_next_continues_an_expired_browse_in_place(cx: &mut TestAppContext) {
    let o = expired_big(cx, None);
    assert_eq!(
        label_of(cx, o.win, "fetch-next").as_deref(),
        Some(fetch_next_label(1_000).as_str())
    );
    assert_eq!(
        label_of(cx, o.win, "footer-note").as_deref(),
        Some(MORE_ROWS_RELEASED)
    );
    assert!(!present(cx, o.win, "raise-limit"));
    assert!(!present(cx, o.win, "run-again"));
    cx.update_window(o.win, |_, w, cx| w.click("fetch-next", cx))
        .unwrap();
    wait_real(cx, "the second thousand", |cx| {
        label_of(cx, o.win, "footer-rows").as_deref() == Some("2,000+ rows")
    });
}

#[gpui_kit::test]
fn the_status_bar_says_paused_for_a_browse_that_can_continue(cx: &mut TestAppContext) {
    let o = expired_big(cx, None);
    assert_eq!(
        label_of(cx, o.win, "status-paused").as_deref(),
        Some(MORE_ROWS_RELEASED)
    );
    assert!(!present(cx, o.win, "status-truncated"));
}

#[gpui_kit::test]
fn a_sorted_continuation_says_rows_may_move(cx: &mut TestAppContext) {
    let o = expired_big(cx, Some("v"));
    cx.update_window(o.win, |_, w, cx| w.click("fetch-next", cx))
        .unwrap();
    let want = skip_notice(1_000);
    wait_real(cx, "the notice", |cx| {
        label_of(cx, o.win, "notice").as_deref() == Some(want.as_str())
    });
}
