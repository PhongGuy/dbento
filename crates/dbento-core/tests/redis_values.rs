//! The key browser's value pages, one kind at a time.
#![cfg(feature = "redis")]
mod support;

use std::collections::BTreeSet;

use dbento_core::{
    CellRef, KeyBrowseError, KeyKind, KeyRead, KeyTtl, KeyValuePage, Session, SessionOptions,
};
use dbento_testkit::{
    BIG_LEN, BIG_LIST_LEN, BIG_STRING_LEN, BINARY_KEY, BINARY_VALUE,
    RdUser::{Reader, Writer},
};
use support::{Cell, forced, rd, read, text};

fn cells(p: &KeyValuePage) -> Vec<Vec<Cell>> {
    (0..p.batch.len())
        .map(|r| {
            (0..p.batch.column_count())
                .map(|c| match p.batch.cell(r, c) {
                    CellRef::Null => Cell::Null,
                    CellRef::Text(t) => Cell::Text(t.to_owned()),
                    CellRef::Bytes(b) => Cell::Bytes(b.to_vec()),
                    other => panic!("{other:?}"),
                })
                .collect()
        })
        .collect()
}

fn names(p: &KeyValuePage) -> Vec<&str> {
    p.columns.iter().map(|c| c.name()).collect()
}

async fn first(s: &mut Session, db: u32, key: &[u8]) -> KeyValuePage {
    s.read_key(&KeyRead::new(db, key.to_vec())).await.unwrap()
}

/// Every row of `key`, page by page, with how many pages it took.
async fn all_rows(s: &mut Session, db: u32, key: &[u8], want: u32) -> (Vec<Vec<Cell>>, usize) {
    let mut req = KeyRead::new(db, key.to_vec());
    req.want = want;
    let (mut rows, mut pages) = (Vec::new(), 0);
    loop {
        let p = s.read_key(&req).await.unwrap();
        rows.extend(cells(&p));
        pages += 1;
        match p.next {
            Some(n) => req.from = n,
            None => return (rows, pages),
        }
    }
}

#[tokio::test]
async fn each_kind_reads_as_its_own_columns() {
    let fx = rd("").await;
    let mut s = fx.session(Reader, forced()).await;
    let db = fx.db.index();
    let p = first(&mut s, db, b"str").await;
    assert_eq!(
        (p.kind.clone(), p.length, names(&p)),
        (KeyKind::String, Some(5), vec!["value"])
    );
    assert_eq!(cells(&p), vec![vec![text("hello")]]);
    let p = first(&mut s, db, b"list").await;
    assert_eq!(
        (p.kind.clone(), names(&p)),
        (KeyKind::List, vec!["index", "value"])
    );
    assert_eq!(
        cells(&p),
        vec![
            vec![text("0"), text("a")],
            vec![text("1"), text("b")],
            vec![text("2"), text("c")]
        ]
    );
    let p = first(&mut s, db, b"zset").await;
    assert_eq!(
        (p.kind.clone(), names(&p)),
        (KeyKind::SortedSet, vec!["member", "score"])
    );
    assert_eq!(
        cells(&p),
        vec![vec![text("a"), text("1.5")], vec![text("b"), text("2.5")]]
    );
    let p = first(&mut s, db, b"stream").await;
    assert_eq!(
        (p.kind.clone(), names(&p)),
        (KeyKind::Stream, vec!["id", "fields"])
    );
    assert_eq!(
        cells(&p),
        vec![
            vec![text("1-1"), text(r#"{"f":"v"}"#)],
            vec![text("1-2"), text(r#"{"g":"w"}"#)]
        ]
    );
    let p = first(&mut s, db, b"hash").await;
    let mut rows = cells(&p);
    rows.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    assert_eq!((names(&p), p.length), (vec!["field", "value"], Some(2)));
    assert_eq!(
        rows,
        vec![
            vec![text("age"), text("36")],
            vec![text("name"), text("Ada")]
        ]
    );
    let p = first(&mut s, db, b"set").await;
    let members: BTreeSet<_> = cells(&p)
        .into_iter()
        .map(|r| format!("{:?}", r[0]))
        .collect();
    assert_eq!((names(&p), members.len()), (vec!["member"], 3));
    let p = first(&mut s, db, BINARY_KEY).await;
    assert_eq!(cells(&p), vec![vec![Cell::Bytes(BINARY_VALUE.to_vec())]]);
}

#[tokio::test]
async fn big_values_read_in_pages() {
    let fx = rd("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let db = fx.db.index();
    let (rows, pages) = all_rows(&mut s, db, b"big:list", 10_000).await;
    assert_eq!((rows.len(), pages), (BIG_LIST_LEN, 10));
    assert_eq!(rows[12_345], vec![text("12345"), text("12345")]);
    let (rows, _) = all_rows(&mut s, db, b"big:hash", 500).await;
    let fields: BTreeSet<_> = rows.iter().map(|r| format!("{:?}", r[0])).collect();
    assert_eq!((rows.len(), fields.len()), (BIG_LEN, BIG_LEN));
    let (rows, pages) = all_rows(&mut s, db, b"big:stream", 1_000).await;
    assert_eq!((rows.len(), pages), (BIG_LEN, 4));
    assert_eq!(rows[1_000][0], text("1-1001"));
    let (rows, _) = all_rows(&mut s, db, b"big:zset", 1_000).await;
    assert_eq!(rows.len(), BIG_LEN);
}

#[tokio::test]
async fn a_big_string_is_one_capped_page() {
    let fx = rd("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let p = first(&mut s, fx.db.index(), b"big:string").await;
    assert!(p.truncated_value && p.next.is_none());
    assert_eq!(p.length, Some(BIG_STRING_LEN as u64));
    match p.batch.cell(0, 0) {
        CellRef::Text(t) => assert_eq!(t.len(), 1 << 20),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_fat_hash_page_carries_its_overflow() {
    let fx = rd("").await;
    let fat = "y".repeat(1 << 20);
    let mut args = vec!["HSET".to_owned(), "fat".to_owned()];
    for i in 0..12 {
        args.push(format!("f{i}"));
        args.push(fat.clone());
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    assert_eq!(fx.db.int(&refs).await, 12);
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let (rows, pages) = all_rows(&mut s, fx.db.index(), b"fat", 100).await;
    let fields: BTreeSet<_> = rows.iter().map(|r| format!("{:?}", r[0])).collect();
    assert_eq!(fields.len(), 12);
    assert!(pages >= 2, "{pages}");
}

#[tokio::test]
async fn a_missing_key_is_gone_and_multi_refuses() {
    let fx = rd("").await;
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    let p = first(&mut s, fx.db.index(), b"nosuch").await;
    assert_eq!(
        (p.ttl, p.batch.len(), p.next.is_none()),
        (KeyTtl::Gone, 0, true)
    );
    read(&mut s, "MULTI").await;
    let e = s
        .read_key(&KeyRead::new(fx.db.index(), b"str".to_vec()))
        .await
        .unwrap_err();
    assert!(matches!(e, KeyBrowseError::Unsupported { .. }), "{e:?}");
    read(&mut s, "DISCARD").await;
}
