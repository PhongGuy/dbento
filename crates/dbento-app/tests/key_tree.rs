//! The Redis key tree model: `:` grouping, Load more paging, the key filter, names, TTLs.
use std::time::Duration;

use dbento_app::catalog::{CatalogData, CatalogQuery, CatalogReply};
use dbento_app::keys::{
    KeyBatch, KeyFilter, KeyItem, KeyToken, display_key, key_levels, kind_of, parse_key_filter,
    ttl_text, type_name,
};
use dbento_app::schema_tree::{CatalogTree, LoadRequest, NodeKey, RowKind, db_index};
use dbento_core::{KeyCursor, KeyKind, KeyTtl, SchemaInfo};

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
fn item(name: &[u8], kind: KeyKind, ttl: KeyTtl) -> KeyItem {
    KeyItem {
        name: name.to_vec(),
        kind,
        ttl,
    }
}
fn s(name: &str) -> KeyItem {
    item(name.as_bytes(), KeyKind::String, KeyTtl::Persistent)
}
fn batch(items: Vec<KeyItem>, next: Option<KeyToken>) -> CatalogData {
    CatalogData::Keys(KeyBatch { items, next })
}
fn db0() -> NodeKey {
    NodeKey::Schema {
        conn: "rd".into(),
        schema: "db0".into(),
    }
}
fn folder(prefix: &str) -> NodeKey {
    NodeKey::KeyFolder {
        conn: "rd".into(),
        db: 0,
        prefix: prefix.as_bytes().to_vec(),
    }
}
/// `rd` listing db0 and db1 with db0 opened; returns the tree and db0's first key load.
fn opened() -> (CatalogTree, LoadRequest) {
    let mut t = CatalogTree::new();
    t.set_key_browser("rd");
    let req = t.toggle(&NodeKey::Connection("rd".into())).unwrap();
    answer(
        &mut t,
        req,
        CatalogData::Schemas(vec![SchemaInfo::new("db0"), SchemaInfo::new("db1")]),
    );
    let req = t.toggle(&db0()).expect("db0 loads its keys");
    (t, req)
}
fn arias(t: &CatalogTree, filter: &str) -> Vec<String> {
    t.rows("rd", 0, filter)
        .into_iter()
        .map(|r| r.aria)
        .collect()
}

#[test]
fn names_group_by_colon_folders_first() {
    let names: Vec<&[u8]> = vec![b"user:2", b"user:1:name", b"a", b"user:1:age", b"b:"];
    let top = key_levels(&names, b"");
    assert_eq!(
        top.folders,
        vec![(b"b:".to_vec(), 1), (b"user:".to_vec(), 3)]
    );
    assert_eq!(top.leaves, vec![2]);
    let user = key_levels(&names, b"user:");
    assert_eq!(user.folders, vec![(b"user:1:".to_vec(), 2)]);
    assert_eq!(user.leaves, vec![0]);
    assert_eq!(key_levels(&names, b"b:").leaves, vec![4]);
}

#[test]
fn a_db_opens_on_its_first_key_page_and_lists_folders_keys_and_load_more() {
    let (mut t, req) = opened();
    assert_eq!(
        req.query,
        CatalogQuery::Keys {
            db: 0,
            filter: KeyFilter::default(),
            after: None
        }
    );
    assert_eq!(arias(&t, ""), ["rd.db0", "rd.db0:loading", "rd.db1"]);
    let u1 = item(
        b"user:1",
        KeyKind::Hash,
        KeyTtl::Expires(Duration::from_secs(90)),
    );
    let items = vec![
        u1.clone(),
        s("a"),
        item(b"user:2", KeyKind::Hash, KeyTtl::Persistent),
    ];
    answer(
        &mut t,
        req,
        batch(items, Some(KeyToken::new(KeyCursor::start()))),
    );
    assert_eq!(
        arias(&t, ""),
        [
            "rd.db0",
            "rd.db0/user:",
            "rd.db0.a",
            "rd.db0:more",
            "rd.db1"
        ]
    );
    let rows = t.rows("rd", 0, "");
    assert_eq!(
        rows[1].kind,
        RowKind::KeyFolder {
            prefix: b"user:".to_vec(),
            keys: 2
        }
    );
    assert_eq!(
        (rows[1].label.as_str(), rows[1].depth, rows[1].expanded),
        ("user", 1, false)
    );
    assert_eq!(rows[2].kind, RowKind::Key(s("a")));
    assert_eq!(
        (rows[3].kind.clone(), rows[3].label.as_str()),
        (RowKind::LoadMore, "Load more")
    );
    assert_eq!(t.toggle(&folder("user:")), None, "a folder opens locally");
    let rows = t.rows("rd", 0, "");
    let shown: Vec<(&str, usize)> = rows.iter().map(|r| (r.label.as_str(), r.depth)).collect();
    assert_eq!(
        shown,
        [
            ("db0", 0),
            ("user", 1),
            ("1", 2),
            ("2", 2),
            ("a", 1),
            ("Load more", 1),
            ("db1", 0)
        ]
    );
    assert_eq!(
        (rows[2].aria.as_str(), rows[2].kind.clone()),
        ("rd.db0.user:1", RowKind::Key(u1))
    );
}

#[test]
fn load_more_appends_the_next_page_once_and_a_finished_scan_drops_the_row() {
    let (mut t, req) = opened();
    let token = KeyToken::new(KeyCursor::start());
    answer(&mut t, req, batch(vec![s("a")], Some(token.clone())));
    let more = t.load_more_keys("rd", 0).expect("a next page");
    assert_eq!(
        more.query,
        CatalogQuery::Keys {
            db: 0,
            filter: KeyFilter::default(),
            after: Some(token)
        }
    );
    assert_eq!(t.load_more_keys("rd", 0), None, "one page at a time");
    assert_eq!(
        arias(&t, ""),
        ["rd.db0", "rd.db0.a", "rd.db0:loading", "rd.db1"]
    );
    answer(&mut t, more, batch(vec![s("b")], None));
    assert_eq!(arias(&t, ""), ["rd.db0", "rd.db0.a", "rd.db0.b", "rd.db1"]);
    assert_eq!(t.load_more_keys("rd", 0), None, "the scan finished");
}

#[test]
fn a_new_filter_rescans_and_a_reply_for_the_old_filter_is_dropped() {
    let (mut t, old) = opened();
    let filter = parse_key_filter("user:* type:hash").unwrap();
    let req = t
        .filter_keys("rd", 0, filter.clone())
        .expect("an open db scans again");
    assert_eq!(
        req.query,
        CatalogQuery::Keys {
            db: 0,
            filter: filter.clone(),
            after: None
        }
    );
    assert_eq!(t.key_filter("rd", 0), filter);
    answer(&mut t, old, batch(vec![s("stale")], None));
    assert_eq!(arias(&t, ""), ["rd.db0", "rd.db0:loading", "rd.db1"]);
    answer(&mut t, req, batch(Vec::new(), None));
    let rows = t.rows("rd", 0, "");
    assert_eq!(
        (rows[1].kind.clone(), rows[1].label.as_str()),
        (RowKind::Empty, "no keys")
    );
    // A closed db keeps its filter for its next opening and scans nothing now.
    assert_eq!(
        t.filter_keys("rd", 1, parse_key_filter("x*").unwrap()),
        None
    );
    assert_eq!(t.key_filter("rd", 1).pattern.as_deref(), Some("x*"));
}

#[test]
fn the_explorer_filter_matches_full_key_names_in_open_folders() {
    let (mut t, req) = opened();
    answer(
        &mut t,
        req,
        batch(vec![s("user:1"), s("user:2"), s("order:9")], None),
    );
    t.toggle(&folder("user:"));
    assert_eq!(
        arias(&t, "USER:2"),
        ["rd.db0", "rd.db0/user:", "rd.db0.user:2"]
    );
}

#[test]
fn a_refresh_reloads_an_open_db_from_its_first_page() {
    let (mut t, req) = opened();
    answer(&mut t, req, batch(vec![s("a")], None));
    let loads = t.refresh();
    let first = |l: &LoadRequest| {
        matches!(
            &l.query,
            CatalogQuery::Keys {
                db: 0,
                after: None,
                ..
            }
        )
    };
    assert!(loads.iter().any(first), "{loads:?}");
}

#[test]
fn a_connection_refresh_drops_its_key_listing_and_rescans_from_the_first_page() {
    let (mut t, req) = opened();
    answer(
        &mut t,
        req,
        batch(vec![s("a")], Some(KeyToken::new(KeyCursor::start()))),
    );
    let mut loads = t.refresh_connection("rd");
    let at = loads.iter().position(|l| {
        matches!(
            l.query,
            CatalogQuery::Keys {
                db: 0,
                after: None,
                ..
            }
        )
    });
    let keys = loads.remove(at.expect("db0 rescans"));
    let schemas = loads
        .into_iter()
        .find(|l| l.query == CatalogQuery::Schemas)
        .expect("schemas reload");
    answer(
        &mut t,
        schemas,
        CatalogData::Schemas(vec![SchemaInfo::new("db0"), SchemaInfo::new("db1")]),
    );
    assert_eq!(
        arias(&t, ""),
        ["rd.db0", "rd.db0:loading", "rd.db1"],
        "the old page is gone"
    );
    assert_eq!(t.load_more_keys("rd", 0), None, "no stale next cursor");
    answer(&mut t, keys, batch(vec![s("b")], None));
    assert_eq!(arias(&t, ""), ["rd.db0", "rd.db0.b", "rd.db1"]);
}

#[test]
fn key_names_show_escaped_when_not_plain_text() {
    assert_eq!(display_key(b"user:1"), "user:1");
    assert_eq!(display_key("caf\u{e9}".as_bytes()), "caf\u{e9}");
    assert_eq!(display_key(b"bin\xff\x00"), "bin\\xff\\x00");
    assert_eq!(display_key(b"a\\b\xff"), "a\\\\b\\xff");
    assert_eq!(display_key(b"tab\tkey"), "tab\\x09key");
}

#[test]
fn ttls_read_in_their_largest_unit() {
    let e = |s: u64| ttl_text(&KeyTtl::Expires(Duration::from_secs(s)));
    assert_eq!(ttl_text(&KeyTtl::Persistent), "");
    assert_eq!(
        ttl_text(&KeyTtl::Expires(Duration::from_millis(400))),
        "<1s"
    );
    assert_eq!(
        (e(59), e(90), e(7_200), e(3 * 86_400)),
        ("59s".into(), "1m".into(), "2h".into(), "3d".into())
    );
    assert_eq!(ttl_text(&KeyTtl::Gone), "gone");
}

#[test]
fn kinds_read_as_type_names_and_filters_parse() {
    assert_eq!(type_name(&KeyKind::SortedSet), "zset");
    assert_eq!(type_name(&KeyKind::Other("ReJSON-RL".into())), "ReJSON-RL");
    assert_eq!(kind_of("zset"), KeyKind::SortedSet);
    assert_eq!(kind_of("ReJSON-RL"), KeyKind::Other("ReJSON-RL".into()));
    assert_eq!(parse_key_filter("").unwrap(), KeyFilter::default());
    assert_eq!(parse_key_filter("  *  ").unwrap(), KeyFilter::default());
    let f = |p: Option<&str>, k: Option<&str>| KeyFilter {
        pattern: p.map(Into::into),
        kind: k.map(Into::into),
    };
    assert_eq!(parse_key_filter("user:*").unwrap(), f(Some("user:*"), None));
    assert_eq!(
        parse_key_filter("type:zset").unwrap(),
        f(None, Some("zset"))
    );
    assert_eq!(
        parse_key_filter("a* type:hash").unwrap(),
        f(Some("a*"), Some("hash"))
    );
    assert!(parse_key_filter("a b").unwrap_err().contains("one pattern"));
    assert!(parse_key_filter("type:").unwrap_err().contains("type"));
}

#[test]
fn db_rows_name_their_index() {
    assert_eq!((db_index("db0"), db_index("db15")), (Some(0), Some(15)));
    assert_eq!(
        (db_index("main"), db_index("db-1"), db_index("db")),
        (None, None, None)
    );
}

#[test]
fn a_key_browser_with_no_readable_db_shows_no_schemas() {
    let mut t = CatalogTree::new();
    t.set_key_browser("rd");
    let req = t.toggle(&NodeKey::Connection("rd".into())).unwrap();
    answer(&mut t, req, CatalogData::Schemas(Vec::new()));
    let rows = t.rows("rd", 0, "");
    let shown: Vec<(&str, &str)> = rows
        .iter()
        .map(|r| (r.aria.as_str(), r.label.as_str()))
        .collect();
    assert_eq!(shown, [("rd:empty", "no schemas")]);
}

#[test]
fn an_open_db_asks_for_no_schema_objects() {
    let (mut t, _) = opened();
    assert_eq!(t.objects_to_load(), Vec::new());
}
