//! A stream key's tab on a real Valkey: newest page, paging back, groups, pending, Follow.
#![cfg(feature = "redis")]
use crate::support::{init_tokio, redis_seed as seed, redis_spec, wait_real};
use dbento_app::message_view::{Follow, MESSAGE_PAGE, Panel};
use dbento_app::runner::{RunnerSpec, StatementEnd};
use dbento_app::tab_model::TabModel;
use dbento_core::CellRef;
use dbento_testkit::{RdUser, RedisDb, redis_db};
use gpui_kit::{AppContext as _, Entity, TestAppContext};
use std::time::{Duration, Instant};

fn spec(db: &RedisDb) -> (tempfile::TempDir, RunnerSpec) {
    redis_spec(
        db,
        db.connection_toml("rd", RdUser::Reader, "read_only = true"),
    )
}
fn rows(cx: &mut TestAppContext, m: &Entity<TabModel>) -> u64 {
    m.read_with(cx, |m, _| m.results().first().map_or(0, |r| r.store.rows()))
}
fn id_at(cx: &mut TestAppContext, m: &Entity<TabModel>, row: u64) -> Option<String> {
    m.read_with(cx, |m, _| {
        let CellRef::Text(t) = m.results().first()?.store.cell(row, 0)? else {
            return None;
        };
        Some(t.to_owned())
    })
}
fn view_idle(cx: &mut TestAppContext, m: &Entity<TabModel>) -> bool {
    m.read_with(cx, |m, _| m.message_view().is_some_and(|v| !v.reading()))
}
/// Opens `key` and waits until its first message page has landed (result 0 exists).
fn open_stream(cx: &mut TestAppContext, m: &Entity<TabModel>, index: u32, key: &str) {
    m.update(cx, |m, cx| m.open_key(index, key.as_bytes().to_vec(), cx));
    wait_real(cx, "the newest page", |cx| {
        view_idle(cx, m) && m.read_with(cx, |m, _| !m.results().is_empty())
    });
}
/// The app's bridge plus a runtime the test enters, so the container drops inside it.
fn runtime(cx: &mut TestAppContext) -> tokio::runtime::Runtime {
    init_tokio(cx);
    tokio::runtime::Runtime::new().unwrap()
}

#[gpui_kit::test]
fn a_stream_key_opens_newest_first_and_pages_back_to_its_start(cx: &mut TestAppContext) {
    let rt = runtime(cx);
    let _inside = rt.enter();
    let db = rt.block_on(redis_db());
    let (_dir, spec) = spec(&db);
    let m = cx.new(|_| TabModel::new(spec));
    open_stream(cx, &m, db.index(), "big:stream");
    assert_eq!(rows(cx, &m), u64::from(MESSAGE_PAGE));
    assert_eq!(
        (id_at(cx, &m, 0).as_deref(), id_at(cx, &m, 499).as_deref()),
        (Some("1-3000"), Some("1-2501"))
    );
    m.update(cx, |m, cx| {
        let names: Vec<&str> = m.results()[0]
            .store
            .columns()
            .iter()
            .map(|c| c.name())
            .collect();
        assert_eq!(names, ["id", "time", "fields"]);
        assert_eq!(m.results()[0].sql, m.key_view().unwrap().title());
        assert_eq!(
            m.key_view().unwrap().header(),
            Some("stream \u{B7} 3000 entries \u{B7} no TTL")
        );
        assert!(
            !m.load_more_value(cx),
            "a stream pages with Older, not Load more"
        );
    });
    // Six full pages, then an empty one that ends paging back.
    let mut older = 0;
    while m.update(cx, |m, cx| m.older_messages(cx)) {
        wait_real(cx, "an older page", |cx| view_idle(cx, &m));
        older += 1;
    }
    assert_eq!((older, rows(cx, &m)), (6, 3000));
    assert_eq!(id_at(cx, &m, 2999).as_deref(), Some("1-1"));
}

#[gpui_kit::test]
fn a_short_stream_is_one_page_and_an_empty_stream_shows_its_headers(cx: &mut TestAppContext) {
    let rt = runtime(cx);
    let _inside = rt.enter();
    let db = rt.block_on(redis_db());
    rt.block_on(seed(
        &db,
        "XADD mt:s 1-1 a 1\nXADD mt:s 1-2 a 2\nXADD mt:e 1-1 a 1\nXDEL mt:e 1-1",
    ));
    let (_dir, short) = spec(&db);
    let m = cx.new(|_| TabModel::new(short));
    open_stream(cx, &m, db.index(), "mt:s");
    assert_eq!(rows(cx, &m), 2);
    assert!(
        m.read_with(cx, |m, _| !m.message_view().unwrap().has_older()),
        "a short page is the last"
    );
    let (_dir2, empty) = spec(&db);
    let e = cx.new(|_| TabModel::new(empty));
    open_stream(cx, &e, db.index(), "mt:e");
    e.read_with(cx, |m, _| {
        let names: Vec<&str> = m.results()[0]
            .store
            .columns()
            .iter()
            .map(|c| c.name())
            .collect();
        assert_eq!(names, ["id", "time", "fields"]);
        assert_eq!(m.results()[0].store.rows(), 0);
        let v = m.message_view().unwrap();
        assert!(v.newest().is_none() && !v.has_older() && v.error().is_none());
    });
}

#[gpui_kit::test]
fn groups_and_pending_load_into_the_panel(cx: &mut TestAppContext) {
    let rt = runtime(cx);
    let _inside = rt.enter();
    let db = rt.block_on(redis_db());
    rt.block_on(seed(&db, "XADD mt:g 1-1 a 1\nXADD mt:g 1-2 b 2\nXGROUP CREATE mt:g g 0\nXREADGROUP GROUP g c1 COUNT 2 STREAMS mt:g >"));
    let (_dir, spec) = spec(&db);
    let m = cx.new(|_| TabModel::new(spec));
    open_stream(cx, &m, db.index(), "mt:g");
    assert!(m.update(cx, |m, cx| m.load_groups(cx)));
    wait_real(cx, "the groups", |cx| {
        m.read_with(cx, |m, _| {
            matches!(m.message_view().unwrap().groups(), Some(Panel::Ready(_)))
        })
    });
    m.read_with(cx, |m, _| {
        let Some(Panel::Ready(groups)) = m.message_view().unwrap().groups() else {
            unreachable!()
        };
        assert_eq!((groups.len(), groups[0].name.as_str()), (1, "g"));
        assert!(
            groups[0]
                .text
                .starts_with("g \u{B7} 1 consumer \u{B7} 2 pending"),
            "{}",
            groups[0].text
        );
    });
    assert!(m.update(cx, |m, cx| m.load_pending("g".into(), cx)));
    wait_real(cx, "the pending list", |cx| {
        m.read_with(cx, |m, _| {
            matches!(
                m.message_view().unwrap().pending(),
                Some((_, Panel::Ready(_)))
            )
        })
    });
    m.read_with(cx, |m, _| {
        let Some(("g", Panel::Ready(p))) = m.message_view().unwrap().pending() else {
            unreachable!()
        };
        assert_eq!(p.len(), 2);
        assert!(
            p[0].starts_with("1-1 \u{B7} c1 \u{B7} idle ") && p[0].ends_with(" \u{B7} 1 delivery"),
            "{}",
            p[0]
        );
    });
    m.update(cx, |m, cx| m.close_groups(cx));
    assert!(m.read_with(cx, |m, _| m.message_view().unwrap().groups().is_none()));
}

#[gpui_kit::test]
fn a_denied_xinfo_fails_the_panel_and_keeps_the_rows(cx: &mut TestAppContext) {
    let rt = runtime(cx);
    let _inside = rt.enter();
    let db = rt.block_on(redis_db());
    let pw = db.password(RdUser::Reader).unwrap().to_owned();
    rt.block_on(seed(
        &db,
        &format!("XADD mt:d 1-1 a 1\nACL SETUSER mt_noxinfo on >{pw} ~* &* +@all -xinfo"),
    ));
    let (_dir, spec) = redis_spec(&db, db.connection_toml_as("rd", "mt_noxinfo", ""));
    let m = cx.new(|_| TabModel::new(spec));
    open_stream(cx, &m, db.index(), "mt:d");
    assert!(m.update(cx, |m, cx| m.load_groups(cx)));
    wait_real(cx, "the refusal", |cx| {
        m.read_with(cx, |m, _| {
            matches!(m.message_view().unwrap().groups(), Some(Panel::Failed(_)))
        })
    });
    assert_eq!(rows(cx, &m), 1, "browsing still works");
}

#[gpui_kit::test]
fn a_key_that_stops_being_a_stream_reopens_in_the_value_view(cx: &mut TestAppContext) {
    let rt = runtime(cx);
    let _inside = rt.enter();
    let db = rt.block_on(redis_db());
    let entries: String = (1..=600)
        .map(|i| format!("XADD mt:w 1-{i} n {i}\n"))
        .collect();
    rt.block_on(seed(&db, &entries));
    let (_dir, spec) = spec(&db);
    let m = cx.new(|_| TabModel::new(spec));
    open_stream(cx, &m, db.index(), "mt:w");
    rt.block_on(seed(&db, "DEL mt:w\nRPUSH mt:w a b"));
    assert!(m.update(cx, |m, cx| m.older_messages(cx)));
    wait_real(cx, "the value view", |cx| {
        m.read_with(cx, |m, _| {
            m.message_view().is_none()
                && m.key_view().and_then(|v| v.header())
                    == Some("list \u{B7} 2 elements \u{B7} no TTL")
        })
    });
    let names: Vec<String> = m.read_with(cx, |m, _| {
        m.results()[0]
            .store
            .columns()
            .iter()
            .map(|c| c.name().to_owned())
            .collect()
    });
    assert_eq!(names, ["index", "value"]);
}

/// Pumps the app for `ms` milliseconds of real time.
fn settle(cx: &mut TestAppContext, ms: u64) {
    let until = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < until {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn polling(cx: &mut TestAppContext, m: &Entity<TabModel>) -> bool {
    m.read_with(cx, |m, _| m.message_view().is_some_and(|v| v.polling()))
}
/// Opens `key` with Follow on and waits for the first poll to go out.
fn following(
    cx: &mut TestAppContext,
    db: &RedisDb,
    key: &str,
) -> (tempfile::TempDir, Entity<TabModel>) {
    let (dir, spec) = spec(db);
    let m = cx.new(|_| TabModel::new(spec));
    open_stream(cx, &m, db.index(), key);
    m.update(cx, |m, cx| m.set_follow(Follow::On, cx));
    wait_real(cx, "a poll", |cx| polling(cx, &m));
    (dir, m)
}

#[gpui_kit::test]
fn follow_lands_new_entries_on_top_even_from_an_empty_stream(cx: &mut TestAppContext) {
    let rt = runtime(cx);
    let _inside = rt.enter();
    let db = rt.block_on(redis_db());
    rt.block_on(seed(
        &db,
        "XADD mf:s 1-1 a 1\nXADD mf:s 1-2 a 2\nXADD mf:e 1-1 a 1\nXDEL mf:e 1-1",
    ));
    let (_d, m) = following(cx, &db, "mf:s");
    rt.block_on(seed(&db, "XADD mf:s 2-1 k v"));
    wait_real(cx, "the new entry", |cx| {
        id_at(cx, &m, 0).as_deref() == Some("2-1")
    });
    assert_eq!(
        (rows(cx, &m), id_at(cx, &m, 1).as_deref()),
        (3, Some("1-2"))
    );
    // An empty stream follows from 0-0, so its first entry arrives.
    let (_e, e) = following(cx, &db, "mf:e");
    rt.block_on(seed(&db, "XADD mf:e 2-1 k v"));
    wait_real(cx, "the first entry", |cx| {
        id_at(cx, &e, 0).as_deref() == Some("2-1")
    });
}

#[gpui_kit::test]
fn pause_lets_the_poll_in_flight_land_and_fires_no_cancel(cx: &mut TestAppContext) {
    let rt = runtime(cx);
    let _inside = rt.enter();
    let db = rt.block_on(redis_db());
    rt.block_on(seed(&db, "XADD mp:s 1-1 a 1"));
    let (_d, m) = following(cx, &db, "mp:s");
    let before = m.read_with(cx, |m, _| m.cancels_fired());
    let t0 = Instant::now();
    m.update(cx, |m, cx| m.set_follow(Follow::Paused, cx));
    wait_real(cx, "the poll to land", |cx| !polling(cx, &m));
    assert!(
        t0.elapsed() < Duration::from_millis(1_500),
        "pause waited {:?}",
        t0.elapsed()
    );
    rt.block_on(seed(&db, "XADD mp:s 2-1 k v"));
    settle(cx, 2_000);
    assert_eq!(
        id_at(cx, &m, 0).as_deref(),
        Some("1-1"),
        "a paused tab reads nothing new"
    );
    m.update(cx, |m, cx| m.set_follow(Follow::On, cx));
    wait_real(cx, "the held entry", |cx| {
        id_at(cx, &m, 0).as_deref() == Some("2-1")
    });
    m.update(cx, |m, cx| m.set_follow(Follow::Off, cx));
    wait_real(cx, "the last poll", |cx| !polling(cx, &m));
    assert_eq!(m.read_with(cx, |m, _| m.cancels_fired()), before);
}

#[gpui_kit::test]
fn a_run_holds_follow_for_at_most_one_poll_and_follow_resumes_after(cx: &mut TestAppContext) {
    let rt = runtime(cx);
    let _inside = rt.enter();
    let db = rt.block_on(redis_db());
    rt.block_on(seed(&db, "XADD mr:s 1-1 a 1"));
    let (_d, m) = following(cx, &db, "mr:s");
    let title = m.read_with(cx, |m, _| m.key_view().unwrap().title());
    let before = m.read_with(cx, |m, _| m.cancels_fired());
    let t0 = Instant::now();
    // 3000 rows park at the tab's 1000-row cap, so the run holds the session until fetched.
    m.update(cx, |m, cx| m.run("XRANGE big:stream - +".into(), cx));
    wait_real(cx, "the run to park", |cx| {
        m.read_with(cx, |m, _| {
            m.paused().is_some() || m.parked_at_cap().is_some()
        })
    });
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "the run waited {:?}",
        t0.elapsed()
    );
    rt.block_on(seed(&db, "XADD mr:s 2-1 k v"));
    settle(cx, 1_500);
    m.read_with(cx, |m, _| {
        assert!(
            !m.message_view().unwrap().polling(),
            "no poll while the run holds the session"
        );
        assert_ne!(m.results()[0].sql, title, "result 0 is still the run's");
        assert_eq!(
            m.message_view().unwrap().follow(),
            Follow::On,
            "a held follow stays on"
        );
    });
    m.update(cx, |m, _| m.fetch_all());
    wait_real(cx, "follow to resume", |cx| {
        m.read_with(cx, |m, _| {
            m.results().first().is_some_and(|r| r.sql == title)
        }) && id_at(cx, &m, 0).as_deref() == Some("2-1")
    });
    assert_eq!(m.read_with(cx, |m, _| m.cancels_fired()), before);
}

#[gpui_kit::test]
fn the_ring_drops_the_oldest_rows_and_counts_them(cx: &mut TestAppContext) {
    let rt = runtime(cx);
    let _inside = rt.enter();
    let db = rt.block_on(redis_db());
    rt.block_on(seed(
        &db,
        "XADD md:s 1-1 a 1\nXADD md:s 1-2 a 2\nXADD md:s 1-3 a 3",
    ));
    let (_d, m) = following(cx, &db, "md:s");
    m.update(cx, |m, _| m.set_message_ring(3, usize::MAX));
    rt.block_on(seed(&db, "XADD md:s 2-1 k v\nXADD md:s 2-2 k v"));
    wait_real(cx, "both new entries", |cx| {
        id_at(cx, &m, 0).as_deref() == Some("2-2")
    });
    assert_eq!(rows(cx, &m), 3);
    assert_eq!(
        (id_at(cx, &m, 1).as_deref(), id_at(cx, &m, 2).as_deref()),
        (Some("2-1"), Some("1-3"))
    );
    m.update(cx, |m, cx| {
        assert_eq!(m.message_view().unwrap().dropped(), 2);
        assert!(!m.older_messages(cx));
    });
}

#[gpui_kit::test]
fn a_deleted_stream_stops_following_and_close_ends_a_follow_within_one_poll(
    cx: &mut TestAppContext,
) {
    let rt = runtime(cx);
    let _inside = rt.enter();
    let db = rt.block_on(redis_db());
    rt.block_on(seed(&db, "XADD mn:s 1-1 a 1\nXADD mc:s 1-1 a 1"));
    let (_d, m) = following(cx, &db, "mn:s");
    rt.block_on(seed(&db, "DEL mn:s"));
    wait_real(cx, "follow to stop", |cx| {
        m.read_with(cx, |m, _| m.message_view().unwrap().follow() == Follow::Off)
    });
    let why = m.read_with(cx, |m, _| {
        m.message_view().unwrap().error().map(str::to_owned)
    });
    assert_eq!(why.as_deref(), Some("the stream no longer exists"));

    let (_c, c) = following(cx, &db, "mc:s");
    let closing = c.update(cx, |m, cx| m.close(cx));
    rt.block_on(tokio::time::timeout(
        Duration::from_millis(1_500),
        closing.wait(),
    ))
    .expect("the session closed within one poll");
    assert_eq!(
        c.read_with(cx, |m, _| m.message_view().unwrap().follow()),
        Follow::Off
    );
}

#[gpui_kit::test]
fn a_followed_complete_stream_counts_its_new_rows_until_the_ring_drops_some(
    cx: &mut TestAppContext,
) {
    let rt = runtime(cx);
    let _inside = rt.enter();
    let db = rt.block_on(redis_db());
    rt.block_on(seed(&db, "XADD mo:s 1-1 a 1"));
    let (_d, m) = following(cx, &db, "mo:s");
    rt.block_on(seed(&db, "XADD mo:s 2-1 k v"));
    wait_real(cx, "the new entry", |cx| {
        id_at(cx, &m, 0).as_deref() == Some("2-1")
    });
    let end = |cx: &mut TestAppContext| {
        m.read_with(cx, |m, _| match m.results()[0].store.end() {
            Some(StatementEnd::Complete(o)) => Some(o.rows),
            _ => None,
        })
    };
    assert_eq!(end(cx), Some(2), "the outcome counts the polled row");
    m.update(cx, |m, _| m.set_message_ring(2, usize::MAX));
    rt.block_on(seed(&db, "XADD mo:s 2-2 k v"));
    wait_real(cx, "the second entry", |cx| {
        id_at(cx, &m, 0).as_deref() == Some("2-2")
    });
    assert_eq!(
        end(cx),
        None,
        "a ring that dropped rows is no longer complete"
    );
}
