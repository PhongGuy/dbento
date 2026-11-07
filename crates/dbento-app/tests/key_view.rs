//! The value view's words: its title and the header for each kind.
use std::time::Duration;

use dbento_app::key_view::{KeyView, header_text};
use dbento_core::{KeyKind, KeyTtl};

#[test]
fn titles_and_headers_word_each_kind() {
    assert_eq!(
        KeyView::new(3, b"user:\xff".to_vec()).title(),
        "user:\\xff \u{B7} db 3"
    );
    let p = KeyTtl::Persistent;
    assert_eq!(
        header_text(&KeyKind::Hash, &p, Some(3), false),
        "hash \u{B7} 3 fields \u{B7} no TTL"
    );
    assert_eq!(
        header_text(&KeyKind::List, &p, Some(1), false),
        "list \u{B7} 1 element \u{B7} no TTL"
    );
    assert_eq!(
        header_text(&KeyKind::Set, &p, Some(1), false),
        "set \u{B7} 1 member \u{B7} no TTL"
    );
    assert_eq!(
        header_text(&KeyKind::SortedSet, &p, Some(2), false),
        "zset \u{B7} 2 members \u{B7} no TTL"
    );
    assert_eq!(
        header_text(&KeyKind::Stream, &p, Some(5), false),
        "stream \u{B7} 5 entries \u{B7} no TTL"
    );
    assert_eq!(
        header_text(
            &KeyKind::String,
            &KeyTtl::Expires(Duration::from_secs(90)),
            Some(2_097_152),
            true
        ),
        "string \u{B7} 2097152 bytes \u{B7} expires in 1m \u{B7} first 1 MiB shown"
    );
    assert_eq!(
        header_text(&KeyKind::Other("ReJSON-RL".into()), &p, None, false),
        "ReJSON-RL \u{B7} no TTL"
    );
    assert_eq!(
        header_text(&KeyKind::String, &KeyTtl::Gone, None, false),
        "the key no longer exists"
    );
}

#[test]
fn a_new_view_reads_its_first_page_once() {
    let mut v = KeyView::new(0, b"k".to_vec());
    assert!(v.read().is_some() && v.header().is_none() && !v.has_more());
    v.begin();
    assert!(v.read().is_none(), "one read at a time");
    v.fail("WRONGTYPE Operation against a key holding the wrong kind of value".into());
    assert_eq!(v.error().map(|e| e.starts_with("WRONGTYPE")), Some(true));
}
