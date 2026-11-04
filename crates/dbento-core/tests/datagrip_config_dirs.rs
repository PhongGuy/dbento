//! Which JetBrains product folders are scanned for consoles, and in what order.
use dbento_core::import::datagrip::jetbrains_config_dirs;
use std::path::Path;

#[test]
fn product_folders_with_consoles_come_newest_first_with_ties_by_name() {
    let root = tempfile::TempDir::new().unwrap();
    for dir in [
        "DataGrip2024.1/consoles/db",
        "PyCharm2024.1/consoles/db",
        "DataGrip2024.2/consoles/db",
        "Rider2023.3/consoles/db",
        "DataGrip2024.10/consoles/db",
        "Fleet/consoles/db",
        "RustRover2025.1/options",
        "Data2024.1x/consoles/db",
    ] {
        std::fs::create_dir_all(root.path().join(dir)).unwrap();
    }
    std::fs::write(root.path().join("WebStorm2025.1"), "a file").unwrap();
    let names: Vec<String> = jetbrains_config_dirs(root.path())
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        [
            "DataGrip2024.10",
            "DataGrip2024.2",
            "DataGrip2024.1",
            "PyCharm2024.1",
            "Rider2023.3"
        ]
    );
}

#[test]
fn a_missing_root_has_no_folders() {
    assert!(jetbrains_config_dirs(Path::new("/nonexistent/dbento/jetbrains")).is_empty());
}
