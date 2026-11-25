//! The S3 tree model: buckets list prefixes and objects a level and a page at a time, and
//! keys show escaped while rows keep them raw.
use dbento_app::catalog::{CatalogData, CatalogQuery, CatalogReply};
use dbento_app::objects::{ObjectBatch, ObjectEntry, ObjectItem};
use dbento_app::schema_tree::{CatalogTree, LoadRequest, NodeKey, RowKind};
use dbento_core::{ObjectCursor, SchemaInfo};

fn answer(t: &mut CatalogTree, req: LoadRequest, data: CatalogData) {
    t.apply(
        &req.conn,
        CatalogReply {
            generation: req.generation,
            query: req.query,
            result: Ok(data),
        },
    );
}
fn obj(key: &str) -> ObjectItem {
    ObjectItem {
        key: key.into(),
        size: 1,
        last_modified: None,
        etag: None,
        storage_class: None,
        content_type: None,
    }
}
fn p(s: &str) -> ObjectEntry {
    ObjectEntry::Prefix(s.into())
}
fn o(s: &str) -> ObjectEntry {
    ObjectEntry::Object(obj(s))
}
fn batch(entries: Vec<ObjectEntry>, next: Option<&str>) -> CatalogData {
    CatalogData::Objects(ObjectBatch {
        entries,
        next: next.map(ObjectCursor::new),
    })
}
fn bucket() -> NodeKey {
    NodeKey::Schema {
        conn: "s3".into(),
        schema: "logs".into(),
    }
}
fn prefix(pre: &str) -> NodeKey {
    NodeKey::Prefix {
        conn: "s3".into(),
        bucket: "logs".into(),
        prefix: pre.into(),
    }
}
fn root(after: Option<&str>) -> CatalogQuery {
    CatalogQuery::Objects {
        bucket: "logs".into(),
        prefix: String::new(),
        after: after.map(ObjectCursor::new),
    }
}
/// `s3` listing bucket `logs`, opened; returns the tree and the root's first page load.
fn opened() -> (CatalogTree, LoadRequest) {
    let mut t = CatalogTree::new();
    t.set_object_browser("s3");
    let req = t.toggle(&NodeKey::Connection("s3".into())).unwrap();
    answer(
        &mut t,
        req,
        CatalogData::Schemas(vec![SchemaInfo::new("logs")]),
    );
    let req = t.toggle(&bucket()).expect("a bucket lists its root");
    (t, req)
}
fn arias(t: &CatalogTree) -> Vec<String> {
    t.rows("s3", 0, "").into_iter().map(|r| r.aria).collect()
}

#[test]
fn a_bucket_opens_on_its_root_page_with_prefixes_objects_and_load_more() {
    let (mut t, req) = opened();
    assert_eq!(req.query, root(None));
    assert_eq!(arias(&t), ["s3.logs", "s3.logs:loading"]);
    answer(
        &mut t,
        req,
        batch(vec![p("a/"), o("b.txt"), p("c/")], Some("t1")),
    );
    assert_eq!(
        arias(&t),
        [
            "s3.logs",
            "s3.logs/a/",
            "s3.logs.b.txt",
            "s3.logs/c/",
            "s3.logs:more"
        ]
    );
    let rows = t.rows("s3", 0, "");
    assert_eq!(
        (
            rows[1].kind.clone(),
            rows[1].label.as_str(),
            rows[1].depth,
            rows[1].expanded
        ),
        (
            RowKind::Prefix {
                prefix: "a/".into()
            },
            "a",
            1,
            false
        )
    );
    assert_eq!(
        (rows[2].kind.clone(), rows[2].label.as_str()),
        (RowKind::Object(obj("b.txt")), "b.txt")
    );
    assert_eq!(
        (rows[4].kind.clone(), rows[4].label.as_str()),
        (RowKind::LoadMore, "Load more")
    );
}

#[test]
fn a_prefix_lists_its_own_level_once() {
    let (mut t, req) = opened();
    answer(&mut t, req, batch(vec![p("a/")], None));
    let sub = t.toggle(&prefix("a/")).expect("a prefix lists its level");
    assert_eq!(
        sub.query,
        CatalogQuery::Objects {
            bucket: "logs".into(),
            prefix: "a/".into(),
            after: None
        }
    );
    assert_eq!(arias(&t), ["s3.logs", "s3.logs/a/", "s3.logs/a/:loading"]);
    answer(
        &mut t,
        sub,
        batch(vec![o("a/"), p("a/b/"), o("a/x.json")], None),
    );
    let rows = t.rows("s3", 0, "");
    let shown: Vec<(&str, usize)> = rows.iter().map(|r| (r.label.as_str(), r.depth)).collect();
    assert_eq!(
        shown,
        [
            ("logs", 0),
            ("a", 1),
            ("(empty name)", 2),
            ("b", 2),
            ("x.json", 2)
        ]
    );
    assert_eq!(
        rows[2].aria, "s3.logs.a/",
        "the folder marker object is not the prefix row"
    );
    assert_eq!(t.toggle(&prefix("a/")), None, "closing loads nothing");
    assert_eq!(
        t.toggle(&prefix("a/")),
        None,
        "reopening shows the loaded level"
    );
}

#[test]
fn load_more_appends_once_and_drops_a_repeated_prefix() {
    let (mut t, req) = opened();
    answer(&mut t, req, batch(vec![p("a/"), o("b")], Some("t1")));
    let more = t.load_more_objects("s3", "logs", "").expect("a next page");
    assert_eq!(more.query, root(Some("t1")));
    assert_eq!(
        t.load_more_objects("s3", "logs", ""),
        None,
        "one page at a time"
    );
    assert_eq!(
        arias(&t),
        ["s3.logs", "s3.logs/a/", "s3.logs.b", "s3.logs:loading"]
    );
    answer(&mut t, more, batch(vec![p("a/"), o("c")], None));
    assert_eq!(
        arias(&t),
        ["s3.logs", "s3.logs/a/", "s3.logs.b", "s3.logs.c"]
    );
    assert_eq!(
        t.load_more_objects("s3", "logs", ""),
        None,
        "the listing ended"
    );
}

#[test]
fn a_failed_page_keeps_what_loaded_and_retry_lists_again_from_the_start() {
    let (mut t, req) = opened();
    answer(&mut t, req, batch(vec![o("a")], Some("t1")));
    let more = t.load_more_objects("s3", "logs", "").unwrap();
    let failed = "bad (InvalidToken)\nhint: the listing changed or expired; list again";
    t.apply(
        "s3",
        CatalogReply {
            generation: more.generation,
            query: more.query,
            result: Err(failed.into()),
        },
    );
    let rows = t.rows("s3", 0, "");
    assert_eq!(
        rows.iter().map(|r| r.aria.as_str()).collect::<Vec<_>>(),
        ["s3.logs", "s3.logs.a", "s3.logs:error"]
    );
    assert!(
        matches!(&rows[2].kind, RowKind::Failed { message } if message == failed),
        "{:?}",
        rows[2].kind
    );
    let again = t.retry(&bucket()).expect("retry lists again");
    assert_eq!(again.query, root(None));
    assert_eq!(arias(&t), ["s3.logs", "s3.logs:loading"]);
}

#[test]
fn a_refresh_drops_listings_and_ignores_the_old_reply() {
    let (mut t, old) = opened();
    let loads = t.refresh_connection("s3");
    assert!(
        loads.iter().any(|l| l.query == CatalogQuery::Schemas),
        "{loads:?}"
    );
    answer(&mut t, old, batch(vec![o("stale")], None));
    assert!(
        !arias(&t).iter().any(|a| a.contains("stale")),
        "{:?}",
        arias(&t)
    );
}

#[test]
fn keys_show_escaped_while_rows_keep_them_raw() {
    let (mut t, req) = opened();
    answer(
        &mut t,
        req,
        batch(vec![o("bidi\u{202e}txt.exe"), o("tab\tname")], None),
    );
    let rows = t.rows("s3", 0, "");
    assert_eq!(
        (rows[1].label.as_str(), rows[1].aria.as_str()),
        ("bidi\\u{202e}txt.exe", "s3.logs.bidi\\u{202e}txt.exe")
    );
    assert_eq!(rows[2].label, "tab\\u{9}name");
    assert_eq!(
        rows[1].kind,
        RowKind::Object(obj("bidi\u{202e}txt.exe")),
        "the row keeps the raw key"
    );
}

#[test]
fn an_empty_bucket_says_so() {
    let (mut t, req) = opened();
    answer(&mut t, req, batch(Vec::new(), None));
    let rows = t.rows("s3", 0, "");
    assert_eq!(
        (
            rows[1].kind.clone(),
            rows[1].label.as_str(),
            rows[1].aria.as_str()
        ),
        (RowKind::Empty, "no objects", "s3.logs:empty")
    );
}
