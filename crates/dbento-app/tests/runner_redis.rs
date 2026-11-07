//! Redis through the app's runners on a real server: the catalog pages a key scan, and a
//! tab's runner reads a key's value in pages.
#![cfg(feature = "redis")]
mod support;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use dbento_app::catalog::{CatalogData, CatalogQuery, spawn_catalog};
use dbento_app::key_view::{KeyView, VALUE_PAGE};
use dbento_app::keys::{KEY_PAGE, KeyBatch, KeyFilter};
use dbento_app::results::ResultStore;
use dbento_app::runner::{Command, KeyValueAnswer, Runner, RunnerSpec, StatementEnd, spawn_on};
use dbento_app::tab_model::TabModel;
use dbento_core::{CellRef, KeyKind, KeyRead, KeyTtl, Workspace};
use dbento_testkit::{RD_SECRET_VAR, RdUser, RedisDb, redis_db};
use gpui_kit::{AppContext as _, TestAppContext};
use support::{here, init_tokio, wait_real};

/// A runner spec on connection `rd`: the reader, read-only.
fn spec(db: &RedisDb) -> (tempfile::TempDir, RunnerSpec) {
    spec_with(db, "read_only = true")
}

/// `spec` with `extra` as the connection's own settings.
fn spec_with(db: &RedisDb, extra: &str) -> (tempfile::TempDir, RunnerSpec) {
    let dir = tempfile::tempdir().unwrap();
    let toml = format!(
        "dbento = 1\n{}",
        db.connection_toml("rd", RdUser::Reader, extra)
    );
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let password = db.password(RdUser::Reader).map(str::to_owned);
    let spec = RunnerSpec {
        workspace: Arc::new(Workspace::open(dir.path()).unwrap()),
        connection: "rd".into(),
        store: Arc::new(dbento_core::MemoryStore::new()),
        env: Arc::new(move |v| (v == RD_SECRET_VAR).then(|| password.clone()).flatten()),
    };
    (dir, spec)
}

/// Runs each line of `text` as the admin, split on spaces; a server error fails the test.
async fn seed(db: &RedisDb, text: &str) {
    for line in text.lines() {
        let v = db.cmd(&line.split(' ').collect::<Vec<_>>()).await;
        assert!(!format!("{v:?}").contains("ServerError"), "{line}: {v:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_catalog_pages_a_scan_and_every_key_arrives_once() {
    let db = redis_db().await;
    let pairs: String = (0..1_200).map(|i| format!(" ck:{i:04} v")).collect();
    seed(&db, &format!("MSET{pairs}\nHSET ck:h f 1\nEXPIRE ck:h 600")).await;
    let (_dir, spec) = spec(&db);
    let r = spawn_catalog(&here(), spec);
    let replies = r.replies();
    let filter = KeyFilter {
        pattern: Some("ck:*".into()),
        kind: None,
    };
    let (mut seen, mut after, mut pages) = (HashSet::new(), None, 0);
    loop {
        r.load(
            0,
            CatalogQuery::Keys {
                db: db.index(),
                filter: filter.clone(),
                after: after.clone(),
            },
        );
        let reply = tokio::time::timeout(Duration::from_secs(30), replies.recv())
            .await
            .unwrap()
            .unwrap();
        let Ok(CatalogData::Keys(KeyBatch { items, next })) = reply.result else {
            panic!("{:?}", reply.result)
        };
        for k in items {
            if k.name == b"ck:h" {
                assert_eq!(k.kind, KeyKind::Hash);
                assert!(matches!(k.ttl, KeyTtl::Expires(_)), "{:?}", k.ttl);
            }
            assert!(seen.insert(k.name.clone()), "{:?} twice", k.name);
        }
        pages += 1;
        if next.is_none() {
            break;
        }
        after = next;
    }
    assert_eq!(seen.len(), 1_201);
    assert!(pages >= 2, "{pages} pages of at least {KEY_PAGE}");
    let hashes = KeyFilter {
        pattern: Some("ck:*".into()),
        kind: Some("hash".into()),
    };
    r.load(
        0,
        CatalogQuery::Keys {
            db: db.index(),
            filter: hashes,
            after: None,
        },
    );
    let reply = replies.recv().await.unwrap();
    let Ok(CatalogData::Keys(KeyBatch { items, next: None })) = reply.result else {
        panic!("{:?}", reply.result)
    };
    assert_eq!(
        items.iter().map(|k| k.name.as_slice()).collect::<Vec<_>>(),
        [b"ck:h".as_slice()]
    );
}

// A browse timeout ends the Redis session, so the catalog must reconnect for the next load
// rather than answer it from the dead session. The admin pauses every client to force it.
#[tokio::test(flavor = "multi_thread")]
async fn a_timed_out_scan_drops_the_session_so_the_next_load_reconnects() {
    let db = redis_db().await;
    seed(&db, "SET tk:a 1").await;
    let (_dir, spec) = spec_with(&db, "read_only = true\nstatement_timeout = \"300ms\"");
    let r = spawn_catalog(&here(), spec);
    let replies = r.replies();
    let filter = KeyFilter {
        pattern: Some("tk:*".into()),
        kind: None,
    };
    let keys = || CatalogQuery::Keys {
        db: db.index(),
        filter: filter.clone(),
        after: None,
    };
    let answer = async |q| {
        r.load(0, q);
        let reply = tokio::time::timeout(Duration::from_secs(30), replies.recv()).await;
        reply.unwrap().unwrap().result
    };
    assert!(matches!(answer(keys()).await, Ok(CatalogData::Keys(_))));
    seed(&db, "CLIENT PAUSE 1500 ALL").await;
    let timed_out = answer(keys()).await;
    assert_eq!(timed_out, Err("the browse timed out".to_owned()));
    tokio::time::sleep(Duration::from_millis(1_700)).await;
    let after = answer(keys()).await;
    let Ok(CatalogData::Keys(KeyBatch { items, .. })) = after else {
        panic!("{after:?}")
    };
    assert_eq!(
        items.iter().map(|k| k.name.as_slice()).collect::<Vec<_>>(),
        [b"tk:a".as_slice()]
    );
}

async fn read(r: &Runner, read: KeyRead) -> KeyValueAnswer {
    let (tx, rx) = async_channel::bounded(1);
    r.send(Command::ReadKey { read, reply: tx });
    tokio::time::timeout(Duration::from_secs(30), rx.recv())
        .await
        .unwrap()
        .unwrap()
}

/// Reads `key` page by page through a reader tab's runner into one store.
async fn read_all(db: &RedisDb, key: &[u8]) -> (KeyView, ResultStore, usize) {
    let (_dir, spec) = spec(db);
    let r = spawn_on(&here(), spec);
    let mut view = KeyView::new(db.index(), key.to_vec());
    let (mut store, mut pages) = (None, 0);
    while let Some(next) = view.read() {
        view.begin();
        let (columns, batch) = view.apply(read(&r, next).await.unwrap());
        store
            .get_or_insert_with(|| ResultStore::new(columns.into()))
            .push(batch);
        pages += 1;
    }
    (view, store.unwrap(), pages)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_list_reads_in_pages_and_a_long_string_in_one_capped_page() {
    let db = redis_db().await;
    let items: String = (0..1_200).map(|i| format!(" {i}")).collect();
    seed(
        &db,
        &format!("RPUSH vk:list{items}\nSETRANGE vk:big 2097151 x"),
    )
    .await;
    let (view, store, pages) = read_all(&db, b"vk:list").await;
    let names: Vec<&str> = store.columns().iter().map(|c| c.name()).collect();
    assert_eq!(names, ["index", "value"]);
    assert_eq!(
        (store.rows(), pages),
        (1_200, 1_200usize.div_ceil(VALUE_PAGE as usize))
    );
    assert!(matches!(store.cell(1_199, 1), Some(CellRef::Text("1199"))));
    assert_eq!(
        view.header(),
        Some("list \u{B7} 1200 elements \u{B7} no TTL")
    );
    let (view, store, pages) = read_all(&db, b"vk:big").await;
    assert_eq!((store.rows(), pages), (1, 1));
    assert_eq!(
        view.header(),
        Some("string \u{B7} 2097152 bytes \u{B7} no TTL \u{B7} first 1 MiB shown")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_key_reads_as_gone_with_no_rows() {
    let db = redis_db().await;
    let (view, store, _) = read_all(&db, b"vk:none").await;
    assert_eq!(store.rows(), 0);
    assert_eq!(view.header(), Some("the key no longer exists"));
}

// The tab model reads the value's pages into result 0, one read in flight at a time.
#[gpui_kit::test]
fn a_key_tab_loads_more_until_the_value_ends(cx: &mut TestAppContext) {
    init_tokio(cx);
    let rt = tokio::runtime::Runtime::new().unwrap();
    // The container drops inside the runtime, which its cleanup needs.
    let _inside = rt.enter();
    let db = rt.block_on(redis_db());
    let fields: String = (0..600).map(|i| format!(" f{i:03} {i}")).collect();
    rt.block_on(seed(&db, &format!("HSET mk:h{fields}")));
    let (_dir, spec) = spec(&db);
    let m = cx.new(|_| TabModel::new(spec));
    let index = db.index();
    m.update(cx, |m, cx| {
        m.open_key(index, b"mk:h".to_vec(), cx);
        assert!(!m.load_more_value(cx), "the first page is still in flight");
    });
    wait_real(cx, "the first page", |cx| {
        m.read_with(cx, |m, _| {
            m.key_view().is_some_and(|v| v.header().is_some())
        })
    });
    m.read_with(cx, |m, _| {
        let v = m.key_view().unwrap();
        assert_eq!(v.header(), Some("hash \u{B7} 600 fields \u{B7} no TTL"));
        assert!(v.has_more());
        assert_eq!(m.results().len(), 1);
        assert_eq!(
            m.results()[0].sql,
            "mk:h \u{B7} db ".to_owned() + &index.to_string()
        );
        assert!(m.results()[0].store.rows() >= u64::from(VALUE_PAGE));
        assert!(m.results()[0].store.end().is_none(), "pages remain");
    });
    // A scan may end with an empty page, so more is asked for until the view says it ended.
    while m.update(cx, |m, cx| m.load_more_value(cx)) {
        wait_real(cx, "the next page", |cx| {
            m.read_with(cx, |m, _| {
                m.key_view()
                    .is_some_and(|v| v.read().is_some() || !v.has_more())
            })
        });
        m.read_with(cx, |m, _| assert_eq!(m.key_view().unwrap().error(), None));
    }
    m.read_with(cx, |m, _| {
        let store = &m.results()[0].store;
        assert_eq!(store.rows(), 600);
        assert!(
            matches!(store.end(), Some(StatementEnd::Complete(o)) if o.rows == 600),
            "{:?}",
            store.end()
        );
    });
    assert!(
        !m.update(cx, |m, cx| m.load_more_value(cx)),
        "nothing is left"
    );
}

// A key that turns into another kind between pages answers WRONGTYPE on the old cursor; the
// runner restarts from the top and the tab replaces result 0 with the new kind's rows.
#[gpui_kit::test]
fn a_key_that_changes_kind_between_pages_restarts_from_the_top(cx: &mut TestAppContext) {
    init_tokio(cx);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _inside = rt.enter();
    let db = rt.block_on(redis_db());
    let fields: String = (0..600).map(|i| format!(" f{i:03} {i}")).collect();
    rt.block_on(seed(&db, &format!("HSET wk:k{fields}")));
    let (_dir, spec) = spec(&db);
    let m = cx.new(|_| TabModel::new(spec));
    let index = db.index();
    m.update(cx, |m, cx| m.open_key(index, b"wk:k".to_vec(), cx));
    wait_real(cx, "the first page", |cx| {
        m.read_with(cx, |m, _| {
            m.key_view().is_some_and(|v| v.header().is_some())
        })
    });
    assert!(m.read_with(cx, |m, _| m.key_view().unwrap().has_more()));
    rt.block_on(seed(&db, "DEL wk:k\nRPUSH wk:k a b c"));
    assert!(m.update(cx, |m, cx| m.load_more_value(cx)));
    wait_real(cx, "the restarted read", |cx| {
        m.read_with(cx, |m, _| {
            m.key_view()
                .is_some_and(|v| v.read().is_some() || !v.has_more())
        })
    });
    m.read_with(cx, |m, _| {
        let v = m.key_view().unwrap();
        assert_eq!(v.error(), None);
        assert_eq!(v.header(), Some("list \u{B7} 3 elements \u{B7} no TTL"));
        let store = &m.results()[0].store;
        let names: Vec<&str> = store.columns().iter().map(|c| c.name()).collect();
        assert_eq!(names, ["index", "value"]);
        assert_eq!(store.rows(), 3);
        assert!(matches!(store.end(), Some(StatementEnd::Complete(_))));
    });
}
