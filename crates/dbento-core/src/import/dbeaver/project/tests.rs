//! Finding a DBeaver project and reading its storages, credentials and script bindings.
use super::*;
use crate::Secret;
use crate::import::dbeaver::DBeaverError;
use crate::import::dbeaver::credentials::encrypt;
use std::path::Path;
use tempfile::TempDir;

fn put(root: &Path, rel: &str, bytes: &[u8]) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn creds(json: &str) -> Vec<u8> {
    encrypt(json.as_bytes(), [9; 16])
}

const ONE: &str = r#"{"connections":{"pg-1":{"provider":"postgresql","driver":"postgres-jdbc","name":"One","configuration":{"host":"db.one.example"}}}}"#;

fn ids(p: &RawProject) -> Vec<&str> {
    p.connections.iter().map(|c| c.id.as_str()).collect()
}

#[test]
fn a_project_folder_or_its_dbeaver_folder_is_read_from_the_project_root() {
    let tmp = TempDir::new().unwrap();
    put(tmp.path(), ".dbeaver/data-sources.json", ONE.as_bytes());
    for given in [tmp.path().to_path_buf(), tmp.path().join(".dbeaver")] {
        let p = read_project(&given, true).unwrap();
        assert_eq!(p.root, tmp.path());
        assert_eq!(ids(&p), ["pg-1"]);
    }
}

#[test]
fn a_folder_without_storages_names_the_projects_below_it() {
    let tmp = TempDir::new().unwrap();
    put(tmp.path(), "Work/.dbeaver/data-sources-team.json", b"{}");
    put(tmp.path(), "General/.dbeaver/data-sources.json", b"{}");
    std::fs::create_dir_all(tmp.path().join("Empty/.dbeaver")).unwrap();
    match read_project(tmp.path(), true) {
        Err(DBeaverError::NotAProject { folder, projects }) => {
            assert_eq!(folder, tmp.path());
            assert_eq!(projects, ["General", "Work"]);
        }
        other => panic!("{other:?}"),
    }
    let text = read_project(tmp.path(), true).unwrap_err().to_string();
    assert!(
        text.contains("not a DBeaver project") && text.contains("General, Work"),
        "{text}"
    );
}

#[test]
fn an_encrypted_project_is_refused_and_a_false_flag_is_not() {
    for settings in [
        r#"{"secureProject":true}"#,
        r#"{"useProjectPassword":"TRUE"}"#,
    ] {
        let tmp = TempDir::new().unwrap();
        put(tmp.path(), ".dbeaver/data-sources.json", ONE.as_bytes());
        put(
            tmp.path(),
            ".dbeaver/project-settings.json",
            settings.as_bytes(),
        );
        let got = read_project(tmp.path(), true);
        assert!(
            matches!(got, Err(DBeaverError::EncryptedProject { .. })),
            "{settings}"
        );
    }
    let tmp = TempDir::new().unwrap();
    put(tmp.path(), ".dbeaver/data-sources.json", ONE.as_bytes());
    put(
        tmp.path(),
        ".dbeaver/project-settings.json",
        br#"{"id":"x","secureProject":false}"#,
    );
    assert!(read_project(tmp.path(), true).is_ok());
}

#[test]
fn storages_are_read_default_first_each_with_its_own_credentials_and_the_first_id_wins() {
    let tmp = TempDir::new().unwrap();
    let d = tmp.path();
    put(
        d,
        ".dbeaver/data-sources-b.json",
        br#"{"connections":{"pg-1":{"name":"Shadow"},"pg-3":{"name":"Three"}}}"#,
    );
    put(
        d,
        ".dbeaver/data-sources.json",
        br#"{"connections":{"pg-2":{"name":"Two"},"pg-1":{"name":"One"}}}"#,
    );
    put(
        d,
        ".dbeaver/data-sources-a.json",
        br#"{"connections":{"pg-4":{}}}"#,
    );
    put(d, ".dbeaver/data-sources.json.bak", b"not json");
    put(
        d,
        ".dbeaver/credentials-config.json",
        &creds(r##"{"pg-1":{"#connection":{"user":"one","password":"fake-pw-1"}}}"##),
    );
    put(
        d,
        ".dbeaver/credentials-config-b.json",
        &creds(
            r##"{"pg-1":{"#connection":{"user":"shadow","password":"fake-pw-x"}},"pg-3":{"#connection":{"user":"three","password":"fake-pw-3"}}}"##,
        ),
    );
    let p = read_project(d, true).unwrap();
    assert_eq!(ids(&p), ["pg-1", "pg-2", "pg-4", "pg-3"]);
    let one = &p.connections[0];
    assert_eq!(
        (
            one.name.as_str(),
            one.user.as_deref(),
            one.password.as_ref().map(Secret::expose)
        ),
        ("One", Some("one"), Some("fake-pw-1"))
    );
    assert_eq!(
        (
            p.connections[1].user.as_deref(),
            p.connections[2].name.as_str()
        ),
        (None, "pg-4")
    );
    let three = &p.connections[3];
    assert_eq!(
        (
            three.user.as_deref(),
            three.password.as_ref().map(Secret::expose)
        ),
        (Some("three"), Some("fake-pw-3"))
    );
    let p = read_project(d, false).unwrap();
    assert!(p.connections.iter().all(|c| c.password.is_none()));
    assert_eq!(p.connections[0].user.as_deref(), Some("one"));
}

#[test]
fn configuration_handlers_folders_and_flags_are_read_as_strings() {
    let tmp = TempDir::new().unwrap();
    put(tmp.path(), ".dbeaver/data-sources.json", br#"{
      "folders": {"Prod": {}, "EU": {"parent": "Prod"}},
      "connections": {"pg-1": {
        "provider": "postgresql", "driver": "postgres-jdbc", "name": "One", "folder": "EU", "read-only": true,
        "configuration": {
          "host": "db.one.example", "port": 6543, "database": "shop", "url": "jdbc:postgresql://stale/x",
          "configurationType": "URL", "auth-model": "native", "type": "prod", "color": "255,0,0",
          "properties": {"ApplicationName": "dbento", "prepareThreshold": 0, "nested": {"x": 1}},
          "handlers": {
            "postgre_ssl": {"type": "CONFIG", "enabled": true, "properties": {"sslMode": "require", "ssl.forceTls12": false}},
            "ssh_tunnel": {"type": "TUNNEL", "enabled": false, "properties": {"host": "jump.example", "port": 22}}
          }
        }
      }}
    }"#);
    let p = read_project(tmp.path(), true).unwrap();
    let c = &p.connections[0];
    assert_eq!(
        (c.provider.as_str(), c.driver.as_str(), c.name.as_str()),
        ("postgresql", "postgres-jdbc", "One")
    );
    assert_eq!((c.folder.as_deref(), c.read_only), (Some("Prod/EU"), true));
    let cfg = &c.config;
    assert_eq!(
        (
            cfg.host.as_deref(),
            cfg.port.as_deref(),
            cfg.database.as_deref()
        ),
        (Some("db.one.example"), Some("6543"), Some("shop"))
    );
    assert_eq!(
        (
            cfg.url.as_deref(),
            cfg.configuration_type.as_deref(),
            cfg.auth_model.as_deref()
        ),
        (
            Some("jdbc:postgresql://stale/x"),
            Some("URL"),
            Some("native")
        )
    );
    assert_eq!(
        (cfg.connection_type.as_deref(), cfg.color.as_deref()),
        (Some("prod"), Some("255,0,0"))
    );
    let props: Vec<(&str, &str)> = cfg
        .properties
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        props,
        [("ApplicationName", "dbento"), ("prepareThreshold", "0")]
    );
    let ssl = &cfg.handlers["postgre_ssl"];
    assert!(ssl.enabled);
    assert_eq!(
        ssl.properties.get("sslMode").map(String::as_str),
        Some("require")
    );
    assert_eq!(
        ssl.properties.get("ssl.forceTls12").map(String::as_str),
        Some("false")
    );
    assert!(!cfg.handlers["ssh_tunnel"].enabled);
}

#[test]
fn malformed_or_non_utf8_files_are_errors_that_never_quote_the_file() {
    let cases: [(&str, &[u8]); 5] = [
        (
            ".dbeaver/data-sources.json",
            b"{\"connections\": {\"pg-1\": {\"name\": \"fake-pw-7\"",
        ),
        (".dbeaver/data-sources.json", b"{\"connections\": \xff}"),
        (".dbeaver/data-sources.json", b"[\"fake-pw-6\"]"),
        (
            ".dbeaver/project-metadata.json",
            b"{\"resources\": {\"Scripts/a.sql\": \"fake-pw-8",
        ),
        (".dbeaver/project-settings.json", b"[1,"),
    ];
    for (rel, bytes) in cases {
        let tmp = TempDir::new().unwrap();
        put(tmp.path(), ".dbeaver/data-sources.json", ONE.as_bytes());
        put(tmp.path(), rel, bytes);
        let err = read_project(tmp.path(), true).unwrap_err();
        let path = tmp.path().join(rel);
        match &err {
            DBeaverError::Json { path: p, line, .. } => assert_eq!((p, *line), (&path, 1)),
            DBeaverError::NotUtf8 { path: p } => assert_eq!(p, &path),
            other @ (DBeaverError::NotAProject { .. }
            | DBeaverError::EncryptedProject { .. }
            | DBeaverError::Io { .. }
            | DBeaverError::Credentials { .. }) => panic!("{rel}: {other:?}"),
        }
        assert!(!err.to_string().contains("fake-pw"), "{err}");
    }
    let tmp = TempDir::new().unwrap();
    put(tmp.path(), ".dbeaver/data-sources.json", ONE.as_bytes());
    put(
        tmp.path(),
        ".dbeaver/credentials-config.json",
        b"not encrypted at all",
    );
    assert!(matches!(
        read_project(tmp.path(), true),
        Err(DBeaverError::Credentials { .. })
    ));
}

#[test]
fn script_bindings_come_from_project_metadata() {
    let tmp = TempDir::new().unwrap();
    put(tmp.path(), ".dbeaver/data-sources.json", ONE.as_bytes());
    put(tmp.path(), ".dbeaver/project-metadata.json", br#"{"resources":{"Scripts/a.sql":{"default-datasource":"pg-1","default-schema":"app"},"Scripts/b.sql":{"sql-editor-project-id":"x"},"Scripts/c.sql":{"default-datasource":7}}}"#);
    let p = read_project(tmp.path(), true).unwrap();
    let got: Vec<(&str, &str)> = p
        .bindings
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(got, [("Scripts/a.sql", "pg-1")]);
}

#[test]
fn debug_of_raw_records_never_shows_property_values_or_the_url() {
    let tmp = TempDir::new().unwrap();
    put(tmp.path(), ".dbeaver/data-sources.json", br#"{"connections":{"pg-1":{"configuration":{
      "url": "jdbc:postgresql://h/d?password=fake-pw-url",
      "properties": {"password": "fake-pw-prop"},
      "handlers": {"postgre_ssl": {"enabled": true, "properties": {"ssl.keystore.password": "fake-pw-ks"}}}}}}}"#);
    put(
        tmp.path(),
        ".dbeaver/credentials-config.json",
        &creds(r##"{"pg-1":{"#connection":{"user":"u","password":"fake-pw-1"}}}"##),
    );
    let text = format!("{:?}", read_project(tmp.path(), true).unwrap());
    assert!(!text.contains("fake-pw"), "{text}");
    assert!(
        text.contains("ssl.keystore.password") && text.contains("pg-1"),
        "{text}"
    );
}
