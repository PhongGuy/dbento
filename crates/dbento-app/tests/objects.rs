//! The S3 texts and mirror types: pages merge by key, labels and URLs show keys escaped,
//! sizes read in their unit, and error texts keep their hints.
use dbento_app::objects::{
    ObjectEntry, ObjectItem, introspect_message, merge_page, object_error_text,
    object_listing_message, s3_url, segment_label, size_words,
};
use dbento_core::{ExecuteError, IntrospectError, ObjectBrowseError};

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

#[test]
fn pages_merge_by_key_with_prefixes_first() {
    let merged = merge_page(
        vec!["a/".into(), "c/".into()],
        vec![obj("a/"), obj("b"), obj("d")],
    );
    assert_eq!(merged, [p("a/"), o("a/"), o("b"), p("c/"), o("d")]);
}

#[test]
fn labels_urls_and_sizes() {
    assert_eq!(segment_label("a/", "a/b/"), "b");
    assert_eq!(segment_label("", "x.txt"), "x.txt");
    assert_eq!(segment_label("a/", "a/"), "(empty name)");
    assert_eq!(segment_label("a/", "a//"), "(empty name)");
    assert_eq!(segment_label("", "esc\u{1b}[31m"), "esc\\u{1b}[31m");
    assert_eq!(s3_url("logs", "a\u{202e}b"), "s3://logs/a\\u{202e}b");
    assert_eq!(
        (size_words(0), size_words(1), size_words(13)),
        ("0 bytes".into(), "1 byte".into(), "13 bytes".into())
    );
    assert_eq!(
        (size_words(1536), size_words(3 << 20), size_words(1 << 30)),
        ("1.5 KiB".into(), "3.0 MiB".into(), "1.0 GiB".into())
    );
}

#[test]
fn error_texts_escape_keys_and_keep_hints() {
    let nf = ObjectBrowseError::NotFound {
        key: "s3://logs/a\nb".into(),
    };
    assert_eq!(object_error_text(&nf), "not found: s3://logs/a\\u{a}b");
    let db = ObjectBrowseError::Database {
        code: "InvalidToken".into(),
        message: "bad\u{202e}".into(),
        hint: Some("the listing changed or expired; list again".into()),
    };
    assert_eq!(
        object_error_text(&db),
        "bad\\u{202e} (InvalidToken)\nhint: the listing changed or expired; list again"
    );
    assert_eq!(
        object_error_text(&ObjectBrowseError::AccessDenied {
            message: "no".into()
        }),
        "access denied: no"
    );
    let mut e = ExecuteError::database("AccessDenied", "Access Denied");
    if let ExecuteError::Database { hint, .. } = &mut e {
        *hint = Some("set buckets".into());
    }
    assert_eq!(
        introspect_message(&IntrospectError::Execute(e)),
        "Access Denied (AccessDenied)\nhint: set buckets"
    );
    assert_eq!(
        introspect_message(&IntrospectError::Execute(ExecuteError::database("1", "x"))),
        "x (1)"
    );
}

#[test]
fn store_codes_hints_regions_and_bucket_names_show_escaped() {
    let db = ObjectBrowseError::Database {
        code: "Bad\u{1b}[31m".into(),
        message: "m".into(),
        hint: Some("try\u{202e}this".into()),
    };
    assert_eq!(
        object_error_text(&db),
        "m (Bad\\u{1b}[31m)\nhint: try\\u{202e}this"
    );
    let region = ObjectBrowseError::WrongRegion {
        hint: "use eu\nwest".into(),
    };
    assert_eq!(object_error_text(&region), "wrong region: use eu\\u{a}west");
    assert_eq!(s3_url("lo\u{202e}gs", "k"), "s3://lo\\u{202e}gs/k");
}

#[test]
fn bucket_list_errors_show_the_store_text_escaped() {
    let mut e = ExecuteError::database("Access\u{1b}", "Access\nDenied\u{202e}");
    if let ExecuteError::Database { hint, .. } = &mut e {
        *hint = Some("set\u{2028}buckets".into());
    }
    assert_eq!(
        object_listing_message(&IntrospectError::Execute(e)),
        "Access\\u{a}Denied\\u{202e} (Access\\u{1b})\nhint: set\\u{2028}buckets"
    );
    let raw = IntrospectError::Unexpected {
        message: "odd\u{7}".into(),
    };
    assert_eq!(
        object_listing_message(&raw),
        "unexpected catalog answer: odd\\u{7}"
    );
}
