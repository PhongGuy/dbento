//! The S3 texts and mirror types: pages merge by key, labels and URLs show keys escaped,
//! sizes read in their unit, and error texts keep their hints.
use dbento_app::objects::{
    ObjectEntry, ObjectItem, introspect_message, merge_page, object_error_text, s3_url,
    segment_label, size_words,
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
