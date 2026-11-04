//! The folder path grammar, its errors and its tree order.
use dbento_core::{FolderPath, FolderPathError, WorkspaceItem};

fn p(s: &str) -> FolderPath {
    s.parse().unwrap()
}

#[test]
fn a_path_splits_into_segments_with_a_name_and_a_parent() {
    let x = p("Prod/EU west/小城镇");
    assert_eq!(
        x.segments().collect::<Vec<_>>(),
        ["Prod", "EU west", "小城镇"]
    );
    assert_eq!(x.name(), "小城镇");
    assert_eq!(x.parent(), Some(p("Prod/EU west")));
    assert_eq!(p("Prod").parent(), None);
    assert_eq!(x.as_str(), "Prod/EU west/小城镇");
    assert_eq!(x.to_string(), "Prod/EU west/小城镇");
}

#[test]
fn bad_paths_name_their_fault() {
    assert_eq!("".parse::<FolderPath>(), Err(FolderPathError::Empty));
    assert_eq!("/".parse::<FolderPath>(), Err(FolderPathError::Empty));
    for bad in ["Prod//EU", "/Prod", "Prod/"] {
        let want = FolderPathError::EmptySegment { path: bad.into() };
        assert_eq!(bad.parse::<FolderPath>(), Err(want), "{bad}");
    }
    for bad in ["Prod ", " Prod", "Prod/ EU", "  "] {
        let want = FolderPathError::Untrimmed { path: bad.into() };
        assert_eq!(bad.parse::<FolderPath>(), Err(want), "{bad:?}");
    }
    for bad in ["Pr\tod", "Prod/x\u{7f}y", "a\u{85}b"] {
        let want = FolderPathError::Control { path: bad.into() };
        assert_eq!(bad.parse::<FolderPath>(), Err(want), "{bad:?}");
    }
}

#[test]
fn errors_read_as_sentences() {
    assert_eq!(FolderPathError::Empty.to_string(), "folder path is empty");
    let e = "a//b".parse::<FolderPath>().unwrap_err();
    assert_eq!(e.to_string(), "folder path `a//b` has an empty segment");
}

#[test]
fn paths_sort_in_tree_order_not_string_order() {
    let mut v = vec![p("Prod-x"), p("Prod/EU"), p("prod"), p("Prod"), p("Alpha")];
    v.sort();
    assert_eq!(
        v,
        [p("Alpha"), p("Prod"), p("Prod/EU"), p("Prod-x"), p("prod")]
    );
}

#[test]
fn items_tell_folders_from_connections() {
    let f = WorkspaceItem::Folder(p("A"));
    assert_ne!(f, WorkspaceItem::Connection("A".into()));
    assert_eq!(f.clone(), f);
}
