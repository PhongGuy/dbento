//! An expired browse result continues past its loaded rows: by keyset when unsorted, by
//! re-running and skipping when sorted; both end with exactly the rows of one uncapped run.
use crate::support;
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use crate::support::{MANY, browsing, exec, fixture, init_tokio, spec, wait_real};
use dbento_app::browse::BrowseEvent;
use dbento_app::history::HistoryLog;
use dbento_app::results_pane::skip_notice;
use dbento_app::runner::{Event, RunnerSpec, RunnerTuning, StatementEnd};
use dbento_app::tab_model::{CapView, Continuation, Expiry, TabModel};
use dbento_core::{CellRef, EditValue, History, HistoryEntry, HistoryFilter, RelationRef};
use gpui_kit::{AppContext as _, Entity, TestAppContext};

const LITE: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, g INTEGER, v TEXT); \
    WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 2500) \
    INSERT INTO t SELECT x, x % 7, 'v' || x FROM r;";
const PG: &str = "CREATE TABLE t(id int PRIMARY KEY, g int, v text); \
    INSERT INTO t SELECT x, x % 7, 'v' || x FROM generate_series(1, 2500) x; \
    CREATE TABLE heap(a int); INSERT INTO heap SELECT x FROM generate_series(1, 2500) x;";

/// No first-rows pause below the cap, and parks that expire after 300 ms.
fn tuning() -> RunnerTuning {
    RunnerTuning {
        first_rows: 100_000,
        park_at_cap: Duration::from_millis(300),
        ..RunnerTuning::default()
    }
}
/// Result 0 has ended and the run is over (`RunDone` applied), so `continuation()` is settled.
fn ended(cx: &mut TestAppContext, m: &Entity<TabModel>) {
    wait_real(cx, "result 0 to end", |cx| {
        m.read_with(cx, |m, _| {
            !m.running() && m.results().first().is_some_and(|r| r.store.end().is_some())
        })
    });
}
/// Continues until result 0 ends Complete; returns each continuation used.
fn drain(cx: &mut TestAppContext, m: &Entity<TabModel>) -> Vec<Continuation> {
    let mut used = Vec::new();
    loop {
        ended(cx, m);
        let (complete, next) = m.read_with(cx, |m, _| {
            (
                matches!(m.results()[0].store.end(), Some(StatementEnd::Complete(_))),
                m.continuation(),
            )
        });
        if complete {
            return used;
        }
        used.push(next.expect("an expired browse continues"));
        assert!(m.update(cx, |m, cx| m.continue_browse(cx)));
        assert!(used.len() < 10, "the browse never completed");
    }
}
fn rows_of(cx: &mut TestAppContext, m: &Entity<TabModel>) -> Vec<Vec<String>> {
    m.read_with(cx, |m, _| {
        let s = &m.results()[0].store;
        (0..s.rows())
            .map(|r| {
                (0..s.columns().len())
                    .map(|c| format!("{:?}", s.cell(r, c)))
                    .collect()
            })
            .collect()
    })
}
fn ids(cx: &mut TestAppContext, m: &Entity<TabModel>) -> Vec<String> {
    m.read_with(cx, |m, _| {
        let s = &m.results()[0].store;
        (0..s.rows())
            .map(|r| match s.cell(r, 0) {
                Some(CellRef::Text(t)) => t.to_owned(),
                other => panic!("an id, got {other:?}"),
            })
            .collect()
    })
}
fn browse_sql_of(cx: &mut TestAppContext, m: &Entity<TabModel>) -> String {
    m.read_with(cx, |m, _| m.browse().unwrap().sql().to_owned())
}
/// `sql` run once with no row cap, as `rows_of` reads it.
fn uncapped(cx: &mut TestAppContext, spec: RunnerSpec, sql: &str) -> Vec<Vec<String>> {
    let m = cx.new(|_| TabModel::with_tuning(spec, tuning()));
    m.update(cx, |m, cx| {
        m.set_max_rows(None);
        m.run(sql.to_owned(), cx)
    });
    ended(cx, &m);
    assert!(m.read_with(cx, |m, _| matches!(
        m.results()[0].store.end(),
        Some(StatementEnd::Complete(_))
    )));
    rows_of(cx, &m)
}
fn skips(cx: &mut TestAppContext, m: &Entity<TabModel>) -> Rc<RefCell<Vec<u64>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let out = Rc::clone(&seen);
    cx.update(|cx| {
        cx.subscribe(m, move |_, e: &BrowseEvent, _| {
            if let BrowseEvent::Skipped { rows } = e {
                out.borrow_mut().push(*rows);
            }
        })
        .detach();
    });
    seen
}
fn entries(path: &Path) -> Vec<HistoryEntry> {
    History::open(path)
        .map(|h| h.recent(&HistoryFilter::default(), 100))
        .unwrap_or_default()
}
/// The tokio runtime the tab runs on, started for this test.
fn runtime(cx: &mut TestAppContext) -> tokio::runtime::Handle {
    init_tokio(cx);
    cx.update(|cx| dbento_app::tokio_bridge::handle(cx))
}
/// A browse of `public.<table>` on a fresh Postgres database built by `setup`. The caller
/// holds `rt.enter()` for the whole test, since a container's drop needs a runtime.
fn pg_browse(
    cx: &mut TestAppContext,
    rt: &tokio::runtime::Handle,
    setup: &str,
    table: &str,
) -> (
    dbento_testkit::PgDb,
    tempfile::TempDir,
    RunnerSpec,
    Entity<TabModel>,
) {
    let (db, dir, spec) = rt.block_on(support::pg_spec(""));
    rt.block_on(async { db.admin().await.batch_execute(setup).await.unwrap() });
    let m = cx.new(|_| TabModel::with_tuning(spec.clone(), tuning()));
    cx.update(|cx| {
        cx.subscribe(&m, |m, e: &BrowseEvent, cx| {
            if let BrowseEvent::Run(sql) = e {
                let sql = sql.clone();
                m.update(cx, |m, cx| m.run(sql, cx));
            }
        })
        .detach();
    });
    m.update(cx, |m, cx| {
        m.load_target(RelationRef::new("public", table), cx)
    });
    (db, dir, spec, m)
}

#[test]
fn the_skip_notice_reads_as_the_spec_says() {
    assert_eq!(
        skip_notice(1_000),
        "Re-ran the sorted table and skipped the 1,000 rows already loaded; rows changed since may have moved."
    );
}

#[gpui_kit::test]
fn an_unsorted_browse_continues_by_keyset_and_matches_one_uncapped_run(cx: &mut TestAppContext) {
    let fx = fixture(LITE, "");
    let m = browsing(cx, &fx, "t", tuning(), "");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    let log = cx.new(|_| HistoryLog::new(path.clone()));
    m.update(cx, |m, _| m.set_history(log));
    ended(cx, &m);
    m.read_with(cx, |m, _| {
        assert_eq!(
            m.cap_view(0),
            Some(CapView::Ended {
                expired: Some(Expiry::Idle)
            })
        );
        assert_eq!(m.continuation(), Some(Continuation::Keyset));
    });
    assert!(m.update(cx, |m, cx| m.stage_cell(5, 2, "w", cx)));
    assert!(m.update(cx, |m, cx| m.continue_browse(cx)));
    ended(cx, &m);
    m.read_with(cx, |m, _| {
        assert!(matches!(
            m.results()[0].store.end(),
            Some(StatementEnd::Truncated(o)) if o.rows == 2000
        ));
        assert_eq!(m.results()[0].sql, m.browse().unwrap().sql());
        assert_eq!(m.edits().cell(5, 2), Some(&EditValue::Text("w".into())));
    });
    assert_eq!(drain(cx, &m), [Continuation::Keyset]);
    let sql = browse_sql_of(cx, &m);
    assert_eq!(rows_of(cx, &m), uncapped(cx, spec(&fx, "lite"), &sql));
    m.update(cx, |m, cx| m.flush_history(cx));
    wait_real(cx, "the first run's entry", |_| !entries(&path).is_empty());
    assert_eq!(entries(&path).len(), 1, "a continuation records no entry");
}

#[gpui_kit::test]
fn a_sorted_browse_continues_by_skipping_and_says_so(cx: &mut TestAppContext) {
    let fx = fixture(LITE, "");
    let m = browsing(cx, &fx, "t", tuning(), "");
    let skipped = skips(cx, &m);
    ended(cx, &m);
    assert!(m.update(cx, |m, cx| m.cycle_sort("g", cx)));
    assert_eq!(
        drain(cx, &m),
        [Continuation::Skip(1000), Continuation::Skip(2000)]
    );
    assert_eq!(*skipped.borrow(), [1000, 2000]);
    let sql = browse_sql_of(cx, &m);
    assert!(sql.ends_with("ORDER BY g ASC, id ASC"), "{sql}");
    assert_eq!(rows_of(cx, &m), uncapped(cx, spec(&fx, "lite"), &sql));
}

#[gpui_kit::test]
fn a_keyset_continuation_never_repeats_a_row_written_meanwhile(cx: &mut TestAppContext) {
    let fx = fixture(LITE, "");
    let m = browsing(cx, &fx, "t", tuning(), "");
    ended(cx, &m);
    exec(
        &fx,
        "INSERT INTO t VALUES (0, 0, 'early'), (9999, 0, 'late')",
    );
    assert_eq!(drain(cx, &m), [Continuation::Keyset, Continuation::Keyset]);
    let ids = ids(cx, &m);
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "no row twice");
    assert_eq!(ids.len(), 2501);
    assert!(ids.iter().any(|i| i == "9999"));
    assert!(
        !ids.iter().any(|i| i == "0"),
        "a row before the key is not fetched"
    );
}

#[gpui_kit::test]
fn a_failed_continuation_keeps_the_loaded_rows_and_says_why(cx: &mut TestAppContext) {
    let fx = fixture(LITE, "");
    let m = browsing(cx, &fx, "t", tuning(), "");
    ended(cx, &m);
    exec(&fx, "ALTER TABLE t RENAME TO gone");
    assert!(m.update(cx, |m, cx| m.continue_browse(cx)));
    ended(cx, &m);
    m.read_with(cx, |m, _| {
        assert!(matches!(
            m.results()[0].store.end(),
            Some(StatementEnd::Truncated(o)) if o.rows == 1000
        ));
        assert_eq!(m.results()[0].store.rows(), 1000);
        assert!(
            m.notice()
                .is_some_and(|n| n.starts_with("Could not fetch more: ")),
            "{:?}",
            m.notice()
        );
        assert_eq!(
            m.continuation(),
            Some(Continuation::Keyset),
            "it can be tried again"
        );
    });
}

#[gpui_kit::test]
fn only_an_expired_keyed_browse_continues(cx: &mut TestAppContext) {
    let fx = fixture(LITE, "");
    let parked = RunnerTuning {
        park_at_cap: Duration::from_secs(30),
        ..tuning()
    };
    let m = browsing(cx, &fx, "t", parked, "");
    wait_real(cx, "the park", |cx| {
        m.read_with(cx, |m, _| m.parked_at_cap().is_some())
    });
    assert_eq!(m.read_with(cx, |m, _| m.continuation()), None);
    assert!(!m.update(cx, |m, cx| m.continue_browse(cx)));
    let q = cx.new(|_| TabModel::with_tuning(spec(&fx, "lite"), tuning()));
    q.update(cx, |q, cx| q.run(MANY.into(), cx));
    ended(cx, &q);
    q.read_with(cx, |q, _| {
        assert!(matches!(
            q.cap_view(0),
            Some(CapView::Ended { expired: Some(_) })
        ));
        assert_eq!(q.continuation(), None, "a query tab never continues");
    });
}

#[gpui_kit::test]
fn postgres_continuations_match_one_uncapped_run(cx: &mut TestAppContext) {
    let rt = runtime(cx);
    let _enter = rt.enter();
    let (_db, _dir, spec, m) = pg_browse(cx, &rt, PG, "t");
    assert_eq!(drain(cx, &m), [Continuation::Keyset, Continuation::Keyset]);
    let sql = browse_sql_of(cx, &m);
    assert_eq!(rows_of(cx, &m), uncapped(cx, spec.clone(), &sql));
    assert!(m.update(cx, |m, cx| m.cycle_sort("g", cx)));
    assert_eq!(
        drain(cx, &m),
        [Continuation::Skip(1000), Continuation::Skip(2000)]
    );
    let sql = browse_sql_of(cx, &m);
    assert!(sql.ends_with("ORDER BY g ASC, id ASC"), "{sql}");
    assert_eq!(rows_of(cx, &m), uncapped(cx, spec, &sql));
}

#[gpui_kit::test]
fn a_postgres_ctid_browse_does_not_continue(cx: &mut TestAppContext) {
    let rt = runtime(cx);
    let _enter = rt.enter();
    let (_db, _dir, _spec, m) = pg_browse(cx, &rt, PG, "heap");
    ended(cx, &m);
    m.read_with(cx, |m, _| {
        assert!(matches!(
            m.cap_view(0),
            Some(CapView::Ended { expired: Some(_) })
        ));
        assert_eq!(m.continuation(), None);
    });
}

#[gpui_kit::test]
fn a_continuation_adds_its_time_to_the_browse(cx: &mut TestAppContext) {
    let fx = fixture(LITE, "");
    let m = browsing(cx, &fx, "t", tuning(), "");
    ended(cx, &m);
    m.update(cx, |m, cx| {
        let before = m.results()[0].elapsed.expect("the first load's time");
        let Some(StatementEnd::Truncated(outcome)) = m.results()[0].store.end() else {
            panic!("an expired browse ends truncated");
        };
        let outcome = outcome.clone();
        assert!(m.continue_browse(cx));
        let later = Instant::now() + Duration::from_secs(5);
        let ticking = m.elapsed_at(0, later).unwrap();
        assert!(ticking > before + Duration::from_secs(4), "{ticking:?}");
        // The continuation's runner reports only its own time; the footer shows the browse's.
        let own = Duration::from_secs(60);
        m.apply(Event::ParkedAtCap {
            run_id: m.run_id(),
            stmt: 0,
            outcome,
            elapsed: own,
        });
        assert_eq!(m.results()[0].elapsed, Some(before + own));
        assert_eq!(m.elapsed_at(0, later), Some(before + own));
    });
}
