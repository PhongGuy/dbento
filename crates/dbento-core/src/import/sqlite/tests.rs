//! The shared SQLite tail: dropped names, in-memory paths, Windows paths, roots and missing files.
use super::*;
use crate::import::model::{SkipReason, WarningKind};
use std::path::Path;
use tempfile::TempDir;

fn dropped(name: &str) -> WarningKind {
    WarningKind::DroppedParameter(name.into())
}

#[test]
fn dropped_names_warn_once_each_in_order_then_a_missing_file_warns() {
    let (draft, warnings) = map_path(
        "data/app.db",
        &["cache", "mode", "cache"],
        Path::new("/proj"),
        false,
    )
    .unwrap();
    assert_eq!(draft.path(), Some("/proj/data/app.db"));
    assert!(!draft.read_only());
    assert_eq!(
        warnings,
        [dropped("cache"), dropped("mode"), WarningKind::FileMissing]
    );
}

#[test]
fn an_empty_or_memory_path_is_in_memory() {
    for p in ["", ":memory:"] {
        assert_eq!(
            map_path(p, &["cache"], Path::new("/proj"), false).unwrap_err(),
            SkipReason::InMemory,
            "{p:?}"
        );
    }
}

#[test]
fn a_windows_path_is_kept_verbatim() {
    for p in [r"C:\data\app.db", "D:/app.db", r"\\server\share\app.db"] {
        let (draft, warnings) = map_path(p, &[], Path::new("/proj"), true).unwrap();
        assert_eq!(draft.path(), Some(p));
        assert!(draft.read_only());
        assert_eq!(warnings, [WarningKind::FileMissing]);
    }
}

#[test]
fn a_relative_path_joins_the_root_and_an_absolute_one_stays() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir(root.path().join("data")).unwrap();
    std::fs::write(root.path().join("data/app.db"), b"").unwrap();
    let joined = root.path().join("data/app.db");
    let (rel, w1) = map_path("data/app.db", &[], root.path(), false).unwrap();
    assert_eq!(rel.path(), joined.to_str());
    assert!(w1.is_empty(), "{w1:?}");
    let (abs, w2) = map_path(
        joined.to_str().unwrap(),
        &[],
        Path::new("/elsewhere"),
        false,
    )
    .unwrap();
    assert_eq!(abs.path(), joined.to_str());
    assert!(w2.is_empty(), "{w2:?}");
}
