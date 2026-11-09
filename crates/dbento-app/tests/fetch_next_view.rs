//! The results footer and status bar at the row cap: Fetch next, Fetch all, expiry and raise limit.
use crate::support::{MANY, Opened, label_of, opened, present, wait_real};
use dbento_app::results_pane::{
    expiry_note, fetch_next_label, group_digits, more_rows, raise_label, run_again_label,
};
use dbento_app::runner::{Event, StatementEnd, StopReason};
use dbento_app::status_bar::truncation_text;
use dbento_app::tab_model::{CapView, Expiry, raise_target};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

fn parked(cx: &mut TestAppContext, o: &Opened) {
    parked_with(cx, o, MANY);
}
fn parked_with(cx: &mut TestAppContext, o: &Opened, sql: &str) {
    let m = o.model(cx);
    m.update(cx, |m, _| m.set_max_rows(Some(1_000)));
    o.run(cx, sql);
    wait_real(cx, "the park at the cap", |cx| {
        m.read_with(cx, |m, _| m.parked_at_cap().is_some())
    });
}
fn expire(cx: &mut TestAppContext, o: &Opened) {
    let m = o.model(cx);
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
        m.read_with(cx, |m, _| {
            !m.running() && m.results().last().is_some_and(|r| r.store.end().is_some())
        })
    });
}
fn click(cx: &mut TestAppContext, o: &Opened, id: &'static str) {
    cx.update_window(o.win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}

#[test]
fn labels_read_as_the_spec_says() {
    assert_eq!(group_digits(1_000), "1,000");
    assert_eq!(group_digits(999), "999");
    assert_eq!(more_rows(1_000), "1,000+ rows");
    assert_eq!(fetch_next_label(1_000), "Fetch next 1,000");
    assert_eq!(fetch_next_label(250), "Fetch next 250");
    assert_eq!(raise_target(Some(1_000)), Some(10_000));
    assert_eq!(raise_target(Some(1_500)), Some(10_000));
    assert_eq!(raise_target(Some(100_000)), Some(1_000_000));
    assert_eq!(raise_target(Some(1_000_000)), None);
    assert_eq!(raise_target(None), None);
    assert_eq!(raise_label(10_000), "Raise limit to 10,000");
    assert_eq!(run_again_label(10_000), "Run again with 10,000");
    assert_eq!(expiry_note(Expiry::Idle), "Stream closed after 60 s");
    assert_eq!(
        expiry_note(Expiry::StatementTimeout),
        "Stream closed: statement timeout"
    );
    assert_eq!(
        expiry_note(Expiry::Released),
        "Stream closed so another tab could commit"
    );
    let more = Some("row limit reached at 1,000 rows, more available");
    assert_eq!(
        truncation_text(None, Some(CapView::Parked { rows: 1000 })).as_deref(),
        more
    );
    // While a fetch streams past the cap the old count is stale, so the chip is hidden.
    assert_eq!(truncation_text(None, Some(CapView::Fetching)), None);
    let lost = StatementEnd::Stopped {
        rows: 1000,
        reason: StopReason::Lost,
    };
    assert_eq!(
        truncation_text(Some(&lost), Some(CapView::Ended { expired: None })).as_deref(),
        Some("connection lost at 1,000 rows")
    );
    assert_eq!(truncation_text(Some(&lost), None), None);
    let budget = StatementEnd::Stopped {
        rows: 5,
        reason: StopReason::MemoryBudget,
    };
    assert_eq!(
        truncation_text(Some(&budget), Some(CapView::Ended { expired: None })).as_deref(),
        Some("memory budget reached at 5 rows")
    );
}

#[gpui_kit::test]
fn the_footer_offers_fetch_at_the_cap_and_fetch_all_completes(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    parked(cx, &o);
    assert_eq!(
        label_of(cx, o.win, "footer-rows").as_deref(),
        Some("1,000+ rows")
    );
    assert_eq!(
        label_of(cx, o.win, "fetch-next").as_deref(),
        Some("Fetch next 1,000")
    );
    assert_eq!(
        label_of(cx, o.win, "fetch-all").as_deref(),
        Some("Fetch all")
    );
    assert_eq!(
        label_of(cx, o.win, "status-truncated").as_deref(),
        Some("row limit reached at 1,000 rows, more available")
    );
    assert_eq!(
        label_of(cx, o.win, ("statement-result", 0usize)).as_deref(),
        Some("first 1,000 rows · more available")
    );
    assert!(!present(cx, o.win, "raise-limit"));
    let m = o.model(cx);
    click(cx, &o, "fetch-next");
    wait_real(cx, "the second park", |cx| {
        m.read_with(cx, |m, _| {
            m.parked_at_cap().is_some() && m.results()[0].store.rows() == 2_000
        })
    });
    assert_eq!(
        label_of(cx, o.win, "footer-rows").as_deref(),
        Some("2,000+ rows")
    );
    assert_eq!(
        label_of(cx, o.win, "status-truncated").as_deref(),
        Some("row limit reached at 2,000 rows, more available")
    );
    click(cx, &o, "fetch-all");
    wait_real(cx, "the end", |cx| {
        m.read_with(cx, |m, _| m.results()[0].store.end().is_some())
    });
    assert_eq!(
        label_of(cx, o.win, "footer-rows").as_deref(),
        Some("5,000 rows")
    );
    assert!(!present(cx, o.win, "fetch-next"));
    assert!(!present(cx, o.win, "fetch-all"));
    assert!(!present(cx, o.win, "status-truncated"));
}

#[gpui_kit::test]
fn an_expired_script_offers_to_raise_the_limit_and_runs_nothing(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    parked_with(cx, &o, &format!("SELECT 1;\n{MANY}"));
    let m = o.model(cx);
    expire(cx, &o);
    assert_eq!(
        label_of(cx, o.win, "footer-rows").as_deref(),
        Some("1,000 rows")
    );
    assert_eq!(
        label_of(cx, o.win, "footer-note").as_deref(),
        Some("Stream closed after 60 s")
    );
    assert_eq!(
        label_of(cx, o.win, "status-truncated").as_deref(),
        Some("truncated at 1,000 rows (stream closed)")
    );
    assert!(!present(cx, o.win, "fetch-next"));
    assert!(!present(cx, o.win, "run-again"));
    assert_eq!(
        label_of(cx, o.win, "raise-limit").as_deref(),
        Some("Raise limit to 10,000")
    );
    let run = m.read_with(cx, |m, _| m.run_id());
    click(cx, &o, "raise-limit");
    assert_eq!(m.read_with(cx, |m, _| m.max_rows()), Some(10_000));
    assert_eq!(
        label_of(cx, o.win, "row-limit").as_deref(),
        Some("10k rows")
    );
    assert_eq!(
        m.read_with(cx, |m, _| m.run_id()),
        run,
        "raising the limit runs nothing"
    );
    assert_eq!(
        label_of(cx, o.win, "raise-limit").as_deref(),
        Some("Raise limit to 100,000")
    );
}

#[gpui_kit::test]
fn the_parked_text_fits_a_narrow_status_bar(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    parked(cx, &o);
    cx.simulate_window_resize(
        o.win,
        gpui_kit::size(gpui_kit::px(520.), gpui_kit::px(600.)),
    );
    assert!(present(cx, o.win, "status-truncated"));
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        let bar = w.find("status-bar").bounds();
        for id in ["status-connection", "status-rows", "status-truncated"] {
            let b = w.find(id).bounds();
            assert!(
                b.right() <= bar.right() && b.size.width > gpui_kit::px(0.),
                "{id} {b:?} outside {bar:?}"
            );
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn an_expired_single_select_runs_again_with_the_next_limit(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    parked(cx, &o);
    expire(cx, &o);
    let m = o.model(cx);
    assert_eq!(
        label_of(cx, o.win, "run-again").as_deref(),
        Some("Run again with 10,000")
    );
    assert!(!present(cx, o.win, "raise-limit"));
    let run = m.read_with(cx, |m, _| m.run_id());
    click(cx, &o, "run-again");
    assert_eq!(m.read_with(cx, |m, _| m.max_rows()), Some(10_000));
    assert_eq!(
        label_of(cx, o.win, "row-limit").as_deref(),
        Some("10k rows")
    );
    // The new run streams past the old cap; it may pause at its first-rows window, so only
    // its start and its own limit are checked.
    wait_real(cx, "the second run", |cx| {
        m.read_with(cx, |m, _| {
            m.run_id() > run
                && m.run_limits().max_rows == Some(10_000)
                && m.results().first().is_some_and(|r| r.store.rows() > 1_000)
        })
    });
    assert_eq!(
        m.read_with(cx, |m, _| m.results()[0].sql.clone()),
        MANY,
        "the same SQL, not rewritten"
    );
    assert!(!present(cx, o.win, "run-again"));
}
