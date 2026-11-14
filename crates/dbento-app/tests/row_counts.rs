//! One row count, one "more rows" phrase and a labelled time across the tab, footer and
//! status bar, while a result streams, parks at its first rows or its cap, and after it closes.
use crate::support::{
    MANY, Opened, fixture, init_tokio, label_of, opened, present, spec, wait_real,
};
use dbento_app::history::HistoryLog;
use dbento_app::results_pane::row_count;
use dbento_app::runner::Event;
use dbento_app::status_bar::statement_label;
use dbento_app::tab_model::TabModel;
use dbento_core::{Engine, History, HistoryFilter};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use std::time::Duration;

fn click(cx: &mut TestAppContext, o: &Opened, id: &'static str) {
    cx.update_window(o.win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}
fn parked_at_cap(cx: &mut TestAppContext, o: &Opened) {
    let m = o.model(cx);
    m.update(cx, |m, _| m.set_max_rows(Some(1_000)));
    o.run(cx, MANY);
    wait_real(cx, "the park at the cap", |cx| {
        m.read_with(cx, |m, _| m.parked_at_cap().is_some())
    });
}
fn paused_at_first_rows(cx: &mut TestAppContext, o: &Opened) {
    let m = o.model(cx);
    m.update(cx, |m, _| m.set_max_rows(Some(10_000)));
    o.run(cx, MANY);
    wait_real(cx, "the first-rows pause", |cx| {
        m.read_with(cx, |m, _| m.has_more())
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
fn counts(cx: &mut TestAppContext, o: &Opened) -> [Option<String>; 3] {
    [
        label_of(cx, o.win, ("statement-result", 0usize)),
        label_of(cx, o.win, "footer-rows"),
        label_of(cx, o.win, "status-rows"),
    ]
}

#[test]
fn a_count_with_more_rows_carries_a_plus() {
    assert_eq!(row_count(1_000, true), "1,000+ rows");
    assert_eq!(row_count(1_000, false), "1,000 rows");
    assert_eq!(row_count(1, false), "1 row");
    assert_eq!(row_count(1_234_567, true), "1,234,567+ rows");
}

#[test]
fn the_statement_label_skips_leading_comments_and_blank_lines() {
    let pg = Some(Engine::Postgres);
    assert_eq!(statement_label("-- c;\nSELECT 1", pg), "SELECT 1");
    assert_eq!(
        statement_label("\n  -- one\n\n-- two\n  SELECT x\n  FROM t", pg),
        "SELECT x"
    );
    assert_eq!(statement_label("/* a\n b */ SELECT 2", pg), "SELECT 2");
    assert_eq!(
        statement_label("// find\ndb.users.find()", Some(Engine::Mongodb)),
        "db.users.find()"
    );
    assert_eq!(
        statement_label("# flush\nGET k", Some(Engine::Redis)),
        "GET k"
    );
    // Comments alone still name the statement by its first line.
    assert_eq!(statement_label("-- only\n", pg), "-- only");
    assert_eq!(statement_label("", pg), "");
}

#[gpui_kit::test]
fn a_parked_result_counts_the_same_everywhere(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    parked_at_cap(cx, &o);
    let [tab, footer, status] = counts(cx, &o);
    assert!(tab.unwrap().starts_with("1,000+ rows"));
    assert_eq!(footer.as_deref(), Some("1,000+ rows"));
    assert_eq!(status.as_deref(), Some("1,000+ rows"));
    assert_eq!(
        label_of(cx, o.win, "status-truncated").as_deref(),
        Some("More rows available")
    );
}

#[gpui_kit::test]
fn a_first_rows_pause_offers_fetch_next_and_names_its_time(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    paused_at_first_rows(cx, &o);
    let [tab, footer, status] = counts(cx, &o);
    assert!(tab.unwrap().starts_with("2,048+ rows"));
    assert_eq!(footer.as_deref(), Some("2,048+ rows"));
    assert_eq!(status.as_deref(), Some("2,048+ rows"));
    let time = label_of(cx, o.win, "footer-elapsed").unwrap();
    assert!(time.starts_with("first rows in "), "{time}");
    assert_eq!(
        label_of(cx, o.win, "fetch-next").as_deref(),
        Some("Fetch next 7,952")
    );
    click(cx, &o, "fetch-next");
    let m = o.model(cx);
    wait_real(cx, "the end", |cx| {
        m.read_with(cx, |m, _| m.results()[0].store.end().is_some())
    });
    assert_eq!(
        label_of(cx, o.win, "footer-rows").as_deref(),
        Some("5,000 rows")
    );
    assert!(!present(cx, o.win, "fetch-next"));
    let time = label_of(cx, o.win, "footer-elapsed").unwrap();
    assert!(time.ends_with(" active"), "{time}");
}

#[gpui_kit::test]
fn an_expired_result_holds_exactly_its_limit_without_a_fetch(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    parked_at_cap(cx, &o);
    expire(cx, &o);
    let rows = o
        .model(cx)
        .read_with(cx, |m, _| m.results()[0].store.rows());
    assert_eq!(rows, 1_000, "a 1k limit collects 1k rows");
}

#[gpui_kit::test]
fn after_fetch_next_and_the_stream_close_the_count_keeps_its_plus(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    parked_at_cap(cx, &o);
    let m = o.model(cx);
    // Fetch next raises the cap by the run's limit, so 2,000 rows is one fetch, not a leak.
    click(cx, &o, "fetch-next");
    wait_real(cx, "the second park", |cx| {
        m.read_with(cx, |m, _| {
            m.parked_at_cap().is_some() && m.results()[0].store.rows() == 2_000
        })
    });
    assert_eq!(
        label_of(cx, o.win, "status-truncated").as_deref(),
        Some("More rows available")
    );
    expire(cx, &o);
    let [tab, footer, status] = counts(cx, &o);
    assert!(tab.clone().unwrap().starts_with("2,000+ rows"), "{tab:?}");
    assert_eq!(footer.as_deref(), Some("2,000+ rows"));
    assert_eq!(status.as_deref(), Some("2,000+ rows"));
    assert_eq!(
        label_of(cx, o.win, "status-truncated").as_deref(),
        Some("truncated at 2,000 rows (stream closed)")
    );
}

#[gpui_kit::test]
fn the_time_after_a_park_says_it_is_active_time(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    parked_at_cap(cx, &o);
    let time = label_of(cx, o.win, "footer-elapsed").unwrap();
    assert!(time.ends_with(" active"), "{time}");
}

#[gpui_kit::test]
fn the_status_bar_names_a_statement_by_its_first_sql_line(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    o.run(cx, "-- c;\nSELECT 1");
    o.idle(cx);
    let bar = label_of(cx, o.win, "status-statement");
    assert_eq!(bar.as_deref(), Some("SELECT 1"));
}

#[gpui_kit::test]
fn history_records_a_parked_statement_cut_by_a_new_run_with_its_active_time(
    cx: &mut TestAppContext,
) {
    let (fx, dir) = (fixture("", ""), tempfile::tempdir().unwrap());
    init_tokio(cx);
    let path = dir.path().join("history.jsonl");
    let log = cx.new(|_| HistoryLog::new(path.clone()));
    let m = cx.new(|_| TabModel::new(spec(&fx, "lite")));
    m.update(cx, |m, _| {
        m.set_history(log);
        m.set_max_rows(Some(1_000));
    });
    m.update(cx, |m, cx| m.run(MANY.into(), cx));
    wait_real(cx, "the park at the cap", |cx| {
        m.read_with(cx, |m, _| m.parked_at_cap().is_some())
    });
    let active = m.read_with(cx, |m, _| m.results()[0].elapsed).unwrap();
    // Time parked waiting for the user is not the statement's own time.
    std::thread::sleep(Duration::from_millis(1_200));
    m.update(cx, |m, cx| m.run("SELECT 1".into(), cx));
    let recorded = || {
        History::open(&path)
            .map(|h| h.recent(&HistoryFilter::default(), 10))
            .unwrap_or_default()
            .into_iter()
            .find(|e| e.sql() == MANY)
    };
    wait_real(cx, "the cut statement's entry", |_| recorded().is_some());
    // The log keeps milliseconds.
    assert_eq!(
        recorded().unwrap().elapsed().as_millis(),
        active.as_millis()
    );
}
