//! Reading DataGrip metadata files into raw records.
use super::*;
use crate::import::datagrip::DataGripError;
use std::path::{Path, PathBuf};

const MODERN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/datagrip/modern"
);
const SHARED: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/datagrip/shared-only"
);
const MALFORMED: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/datagrip/malformed"
);
const ORDERS: &str = "11111111-1111-4111-8111-111111111111";
const APP: &str = "33333333-3333-4333-8333-333333333333";
const TUNNEL: &str = "55555555-5555-4555-8555-555555555555";
const SOURCES: &str = r#"<project version="4"><component name="DataSourceManagerImpl"><data-source source="LOCAL" name="a" uuid="u1"><driver-ref>postgresql</driver-ref></data-source></component></project>"#;

fn project(files: &[(&str, &[u8])]) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    for (rel, bytes) in files {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    dir
}

fn xml_error(files: &[(&str, &[u8])]) -> (PathBuf, u32, String) {
    let d = project(files);
    match read_project(d.path()) {
        Err(DataGripError::Xml {
            path,
            line,
            message,
        }) => (path, line, message),
        other => panic!("{other:?}"),
    }
}

#[test]
fn reads_the_modern_fixture() {
    let p = read_project(Path::new(MODERN)).unwrap();
    assert_eq!(
        (p.root.as_path(), p.idea.clone()),
        (Path::new(MODERN), Path::new(MODERN).join(".idea"))
    );
    let names: Vec<&str> = p.sources.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Orders DB",
            "Reporting (RO)",
            "app.db",
            "Cache",
            "Tunnelled"
        ]
    );
    let orders = &p.sources[0];
    assert_eq!(orders.uuid, ORDERS);
    assert_eq!(orders.driver_ref.as_deref(), Some("postgresql"));
    assert_eq!(orders.working_dir.as_deref(), Some("$ProjectFileDir$"));
    assert!(
        orders
            .url
            .as_deref()
            .unwrap()
            .contains("?currentSchema=sales&password=")
    );
    assert_eq!(
        orders.properties[0],
        ("ApplicationName".to_owned(), "dbento-fixture".to_owned())
    );
    assert!(p.sources[1].read_only && !orders.read_only);
    assert!(!p.has_object_colors && p.colors.is_empty() && p.forest.is_empty());
    assert_eq!(p.sources[0].group.as_deref(), Some("backend"));
    let local = &p.local[ORDERS];
    assert_eq!(local.user_name.as_deref(), Some("orders_app"));
    let ssl = local.ssl.as_ref().unwrap();
    assert_eq!(
        (ssl.enabled, ssl.mode.as_deref(), ssl.ca_cert.as_deref()),
        (
            Some(true),
            Some("REQUIRE"),
            Some("/nonexistent/dbento-fixture/ca.pem")
        )
    );
    assert_eq!(p.local[APP].auth_provider.as_deref(), Some("no-auth"));
    assert!(p.local[TUNNEL].ssh_enabled);
    assert_eq!(p.mappings.len(), 3);
    assert_eq!(
        (p.mappings[0].url.as_str(), p.mappings[0].uuid.as_str()),
        ("file://$PROJECT_DIR$/queries/orders/top.sql", ORDERS)
    );
}

#[test]
fn debug_output_never_shows_a_url_or_property_value() {
    let text = format!("{:?}", read_project(Path::new(MODERN)).unwrap());
    assert!(!text.contains("hunter2"), "{text}");
}

#[test]
fn a_folder_holding_data_sources_xml_is_its_own_root_and_an_idea_folder_has_its_parent() {
    let p = read_project(Path::new(SHARED)).unwrap();
    assert_eq!(
        (p.root.as_path(), p.idea.as_path()),
        (Path::new(SHARED), Path::new(SHARED))
    );
    assert_eq!(p.sources.len(), 2);
    assert!(
        p.local.is_empty() && p.mappings.is_empty() && !p.has_object_colors && p.colors.is_empty()
    );
    let idea = Path::new(MODERN).join(".idea");
    assert_eq!(read_project(&idea).unwrap().root, idea.parent().unwrap());
}

#[test]
fn malformed_truncated_and_dtd_files_are_xml_errors_even_when_optional() {
    match read_project(Path::new(MALFORMED)) {
        Err(DataGripError::Xml { path, .. }) => assert!(path.ends_with(".idea/dataSources.xml")),
        other => panic!("{other:?}"),
    }
    let dtd = br#"<?xml version="1.0"?><!DOCTYPE p [<!ENTITY a "b">]><project/>"#;
    assert!(
        xml_error(&[(".idea/dataSources.xml", dtd)])
            .0
            .ends_with("dataSources.xml")
    );
    let broken_mapping = xml_error(&[
        (".idea/dataSources.xml", SOURCES.as_bytes()),
        (".idea/data_source_mapping.xml", b"<project><component"),
    ]);
    assert!(broken_mapping.0.ends_with("data_source_mapping.xml"));
}

#[test]
fn an_xml_error_names_the_line_but_never_quotes_the_document() {
    let (_, line, message) = xml_error(&[(
        ".idea/dataSources.xml",
        b"<project>\n<x a=\"&hunter2;\"/></project>",
    )]);
    assert_eq!(line, 2);
    assert!(!message.contains("hunter2"), "{message}");
}

#[test]
fn a_non_utf8_file_is_refused_before_parsing() {
    let d = project(&[
        (".idea/dataSources.xml", SOURCES.as_bytes()),
        (".idea/dataSources.local.xml", b"<project>\xff</project>"),
    ]);
    assert!(
        matches!(read_project(d.path()), Err(DataGripError::NotUtf8 { path }) if path.ends_with("dataSources.local.xml"))
    );
}

#[test]
fn a_folder_without_data_sources_xml_is_not_a_project() {
    let d = project(&[("readme.txt", b"x")]);
    assert!(matches!(
        read_project(d.path()),
        Err(DataGripError::NotAProject { .. })
    ));
}

#[test]
fn components_are_found_by_name_in_any_file() {
    let both = r#"<project version="4"><component name="DataSourceManagerImpl"><data-source source="LOCAL" name="a" uuid="u1"><driver-ref>postgresql</driver-ref></data-source></component><component name="dataSourceStorageLocal"><data-source name="a" uuid="u1"><user-name>alice</user-name></data-source></component></project>"#;
    let d = project(&[(".idea/dataSources.xml", both.as_bytes())]);
    assert_eq!(
        read_project(d.path()).unwrap().local["u1"]
            .user_name
            .as_deref(),
        Some("alice")
    );
}

#[cfg(unix)]
#[test]
fn symlinked_metadata_files_are_followed() {
    let real = project(&[("dataSources.xml", SOURCES.as_bytes())]);
    let d = project(&[]);
    std::fs::create_dir(d.path().join(".idea")).unwrap();
    std::os::unix::fs::symlink(
        real.path().join("dataSources.xml"),
        d.path().join(".idea/dataSources.xml"),
    )
    .unwrap();
    assert_eq!(read_project(d.path()).unwrap().sources.len(), 1);
}

#[test]
fn an_error_whose_cause_starts_with_a_quote_still_has_a_message() {
    let (_, _, message) = xml_error(&[(
        ".idea/dataSources.xml",
        br#"<project xmlns:xml="urn:wrong"/>"#,
    )]);
    assert!(!message.is_empty());
}

#[cfg(unix)]
#[test]
fn a_metadata_file_that_is_not_a_regular_file_is_refused() {
    let d = project(&[(".idea/dataSources.xml", SOURCES.as_bytes())]);
    std::os::unix::fs::symlink("/dev/zero", d.path().join(".idea/dataSources.local.xml")).unwrap();
    assert!(matches!(
        read_project(d.path()),
        Err(DataGripError::Io { path, .. }) if path.ends_with("dataSources.local.xml")
    ));
}

#[test]
fn an_oversized_metadata_file_is_refused_unread() {
    let d = project(&[(".idea/dataSources.xml", SOURCES.as_bytes())]);
    let big = std::fs::File::create(d.path().join(".idea/dataSources.local.xml")).unwrap();
    big.set_len(MAX_METADATA_BYTES + 1).unwrap();
    assert!(matches!(
        read_project(d.path()),
        Err(DataGripError::Io { path, .. }) if path.ends_with("dataSources.local.xml")
    ));
}

// A forest file the reader refuses leaves the import without folders, never failed.
fn forest_of(d: &tempfile::TempDir) -> HashMap<String, Arc<[String]>> {
    read_project(d.path()).unwrap().forest
}

#[test]
fn a_forest_path_that_is_a_directory_or_not_utf8_gives_no_folders() {
    let d = project(&[(".idea/dataSources.xml", SOURCES.as_bytes())]);
    std::fs::create_dir(d.path().join(".idea/db-forest-config.xml")).unwrap();
    assert!(forest_of(&d).is_empty());
    let d = project(&[
        (".idea/dataSources.xml", SOURCES.as_bytes()),
        (".idea/db-forest-config.xml", b"<project>\xff\xfe</project>"),
    ]);
    assert!(forest_of(&d).is_empty());
}

#[cfg(unix)]
#[test]
fn a_forest_file_linked_to_dev_zero_gives_no_folders() {
    let d = project(&[(".idea/dataSources.xml", SOURCES.as_bytes())]);
    std::os::unix::fs::symlink("/dev/zero", d.path().join(".idea/db-forest-config.xml")).unwrap();
    assert!(forest_of(&d).is_empty());
}

#[cfg(unix)]
#[test]
fn a_fifo_is_refused_by_the_open_itself_without_blocking() {
    let d = project(&[(".idea/dataSources.xml", SOURCES.as_bytes())]);
    let fifo = d.path().join(".idea/db-forest-config.xml");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    assert!(forest_of(&d).is_empty());
    // Opened directly, as when a regular file is swapped for a FIFO after a check.
    assert!(matches!(
        open_regular(&fifo),
        Err(e) if e.to_string() == "not a regular file"
    ));
}

#[test]
fn reads_the_client_cert_and_key_of_ssl_config() {
    let local = br#"<project version="4"><component name="dataSourceStorageLocal"><data-source name="a" uuid="u1"><ssl-config><client-cert>/c/me.crt</client-cert><client-key>/c/me.key</client-key><enabled>true</enabled></ssl-config></data-source></component></project>"#;
    let d = project(&[
        (".idea/dataSources.xml", SOURCES.as_bytes()),
        (".idea/dataSources.local.xml", local),
    ]);
    let p = read_project(d.path()).unwrap();
    let ssl = p.local["u1"].ssl.as_ref().unwrap();
    assert_eq!(
        (ssl.client_cert.as_deref(), ssl.client_key.as_deref()),
        (Some("/c/me.crt"), Some("/c/me.key"))
    );
}
