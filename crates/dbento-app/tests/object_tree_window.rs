//! The S3 explorer in the window: buckets open to prefixes and objects, Load more pages, an
//! object opens its tab, menus copy escaped text and a prefix opens as an `ls` listing.
#![cfg(feature = "s3")]
use std::sync::Arc;

use crate::support::{
    Started, click_id, label_of, present, right_click, started_custom, wait_real,
};
use dbento_app::explorer::part_id;
use dbento_app::schema_tree::{CatalogRow, RowKind};
use dbento_core::Engine;
use dbento_testkit::{S3_SECRET, S3_SECRET_VAR, S3Moto, s3_moto};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

fn s3_window(cx: &mut TestAppContext, s3: &S3Moto) -> Started {
    let toml = format!("dbento = 1\n{}", s3.connection_toml("s3", ""));
    window(cx, &toml)
}
fn window(cx: &mut TestAppContext, toml: &str) -> Started {
    started_custom(cx, toml, |sv| {
        sv.env = Arc::new(|v| (v == S3_SECRET_VAR).then(|| S3_SECRET.to_owned()))
    })
}
fn model_rows(cx: &mut TestAppContext, s: &Started) -> Vec<CatalogRow> {
    s.sidebar(cx)
        .read_with(cx, |sb, _| sb.tree().rows("s3", 0, ""))
}
/// The `tree-row` index of the row labelled `aria`, once it is rendered: the list lays out
/// only the rows in view, numbered by their place among the catalog rows.
fn row_ix(cx: &mut TestAppContext, s: &Started, aria: &str) -> Option<usize> {
    let ix = model_rows(cx, s).iter().position(|r| r.aria == aria)?;
    (label_of(cx, s.win, ("tree-row", ix)).as_deref() == Some(aria)).then_some(ix)
}
/// Selects the last row, which scrolls it into view, and returns its index once it shows.
fn last_row(cx: &mut TestAppContext, s: &Started, aria: &str) -> usize {
    // A row click gives the explorer focus; once scrolled away it keeps it.
    if label_of(cx, s.win, ("tree-row", 0usize)).is_some() {
        click_id(cx, s.win, ("tree-row", 0usize));
    }
    cx.update_window(s.win, |_, w, cx| {
        w.dispatch_action(Box::new(dbento_app::keymap::TreeSelectLast), cx)
    })
    .unwrap();
    wait_row(cx, s, aria)
}
fn wait_row(cx: &mut TestAppContext, s: &Started, aria: &str) -> usize {
    wait_real(cx, aria, |cx| row_ix(cx, s, aria).is_some());
    row_ix(cx, s, aria).unwrap()
}
/// Opens the row labelled `aria` unless it is open already, as a lone bucket opens itself.
fn expand(cx: &mut TestAppContext, s: &Started, aria: &str) {
    let ix = wait_row(cx, s, aria);
    let open = model_rows(cx, s)
        .iter()
        .any(|r| r.aria == aria && r.expanded);
    if !open {
        click_id(cx, s.win, ("tree-toggle", ix));
    }
}
fn menu(cx: &mut TestAppContext, s: &Started, aria: &str, item: &'static str) {
    let ix = wait_row(cx, s, aria);
    right_click(cx, s, ("tree-row", ix));
    click_id(cx, s.win, item);
}
fn clip(cx: &mut TestAppContext) -> Option<String> {
    cx.read_from_clipboard().and_then(|c| c.text())
}
fn objects(cx: &mut TestAppContext, s: &Started) -> usize {
    model_rows(cx, s)
        .iter()
        .filter(|r| matches!(r.kind, RowKind::Object(_)))
        .count()
}

#[gpui_kit::test]
fn a_bucket_opens_to_prefixes_and_an_object_opens_its_tab(cx: &mut TestAppContext) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let s3 = rt.block_on(s3_moto());
    let b = s3.bucket().to_owned();
    let s = s3_window(cx, &s3);
    click_id(cx, s.win, part_id("connection-toggle", "s3"));
    expand(cx, &s, &format!("s3.{b}"));
    expand(cx, &s, &format!("s3.{b}/text/"));
    let hello = wait_row(cx, &s, &format!("s3.{b}.text/hello.txt"));
    cx.update_window(s.win, |_, w, cx| w.double_click(("tree-row", hello), cx))
        .unwrap();
    wait_real(cx, "the object tab", |cx| {
        present(cx, s.win, "object-header")
    });
    let key = s.shell.read_with(cx, |sh, cx| {
        sh.tab().and_then(|t| {
            t.read(cx)
                .model()
                .read(cx)
                .object_view()
                .map(|v| v.object_key().key)
        })
    });
    assert_eq!(key.as_deref(), Some("text/hello.txt"));
}

#[gpui_kit::test]
fn load_more_pages_a_large_prefix(cx: &mut TestAppContext) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let s3 = rt.block_on(s3_moto());
    rt.block_on(s3.seed_paged());
    let b = s3.bucket().to_owned();
    let s = s3_window(cx, &s3);
    click_id(cx, s.win, part_id("connection-toggle", "s3"));
    expand(cx, &s, &format!("s3.{b}"));
    expand(cx, &s, &format!("s3.{b}/many/"));
    wait_real(cx, "the first page", |cx| objects(cx, &s) == 500);
    let more = model_rows(cx, &s)
        .into_iter()
        .find(|r| r.kind == RowKind::LoadMore)
        .expect("a Load more row");
    assert_eq!(more.aria, format!("s3.{b}/many/:more"));
    let ix = last_row(cx, &s, &more.aria);
    click_id(cx, s.win, ("tree-row", ix));
    wait_real(cx, "the second page", |cx| objects(cx, &s) == 1000);
}

// A bucket named like a Redis DB must still page as a bucket, by a click and by Enter.
#[gpui_kit::test]
fn load_more_pages_a_bucket_named_like_a_redis_db(cx: &mut TestAppContext) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let s3 = rt.block_on(s3_moto());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let name = format!("db{}{nanos}", std::process::id());
    rt.block_on(async {
        let c = s3.client();
        c.create_bucket().bucket(&name).send().await.unwrap();
        for i in 0..1001 {
            let key = format!("k{i:04}");
            c.put_object().bucket(&name).key(key).send().await.unwrap();
        }
    });
    let toml = format!("dbento = 1\n{}", s3.connection_toml("s3", ""))
        .replace(&format!("[\"{}\"]", s3.bucket()), &format!("[\"{name}\"]"));
    let s = window(cx, &toml);
    click_id(cx, s.win, part_id("connection-toggle", "s3"));
    expand(cx, &s, &format!("s3.{name}"));
    wait_real(cx, "the first page", |cx| objects(cx, &s) == 500);
    let more = format!("s3.{name}:more");
    let ix = last_row(cx, &s, &more);
    click_id(cx, s.win, ("tree-row", ix));
    wait_real(cx, "the second page", |cx| objects(cx, &s) == 1000);
    last_row(cx, &s, &more);
    cx.update_window(s.win, |_, w, cx| {
        w.dispatch_action(Box::new(dbento_app::keymap::OpenSelected), cx)
    })
    .unwrap();
    wait_real(cx, "the last page", |cx| objects(cx, &s) == 1001);
}

#[gpui_kit::test]
fn menus_copy_escaped_text_and_a_prefix_opens_as_an_ls_listing(cx: &mut TestAppContext) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let s3 = rt.block_on(s3_moto());
    let b = s3.bucket().to_owned();
    let s = s3_window(cx, &s3);
    click_id(cx, s.win, part_id("connection-toggle", "s3"));
    expand(cx, &s, &format!("s3.{b}"));
    expand(cx, &s, &format!("s3.{b}/hostile/"));
    let bidi = format!("s3.{b}.hostile/bidi\\u{{202e}}txt.exe");
    menu(cx, &s, &bidi, "tree-menu-copy-key");
    assert_eq!(clip(cx).as_deref(), Some("hostile/bidi\\u{202e}txt.exe"));
    menu(cx, &s, &bidi, "tree-menu-copy-url");
    assert_eq!(
        clip(cx),
        Some(format!("s3://{b}/hostile/bidi\\u{{202e}}txt.exe"))
    );
    menu(
        cx,
        &s,
        &format!("s3.{b}/hostile/"),
        "tree-menu-open-listing",
    );
    wait_real(cx, "the listing", |cx| present(cx, s.win, "cell-0-1"));
    let sql = s
        .shell
        .read_with(cx, |sh, cx| sh.tab().unwrap().read(cx).sql(cx));
    assert_eq!(sql, format!("ls 's3://{b}/hostile/'"));
    for row in 0..8 {
        if let Some(cell) = label_of(cx, s.win, format!("cell-{row}-1")) {
            assert!(
                !cell.chars().any(|c| c.is_control() || c == '\u{202e}'),
                "row {row}: {cell:?}"
            );
        }
    }
}

#[gpui_kit::test]
fn s3_rows_offer_their_own_menus(cx: &mut TestAppContext) {
    crate::support::init(cx);
    let ids = |kind: RowKind, cx: &mut TestAppContext| {
        cx.update(|cx| dbento_app::schema_tree::catalog_entries(&kind, Engine::S3, cx))
            .into_iter()
            .map(|e| e.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ids(RowKind::Schema, cx),
        [
            "tree-menu-open-listing",
            "tree-menu-copy-name",
            "tree-menu-refresh"
        ]
    );
    assert_eq!(
        ids(
            RowKind::Prefix {
                prefix: "a/".into()
            },
            cx
        ),
        [
            "tree-menu-open-listing",
            "tree-menu-copy-key",
            "tree-menu-copy-url",
            "tree-menu-refresh"
        ]
    );
    let obj = dbento_app::objects::ObjectItem {
        key: "a/b".into(),
        size: 1,
        last_modified: None,
        etag: None,
        storage_class: None,
        content_type: None,
    };
    assert_eq!(
        ids(RowKind::Object(obj), cx),
        [
            "tree-menu-preview",
            "tree-menu-download",
            "tree-menu-copy-key",
            "tree-menu-copy-url"
        ]
    );
}
