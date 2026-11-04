//! `read_datagrip` connections, skips and warnings on the synthetic fixtures.
use dbento_core::import::datagrip::{DataGripOptions, read_datagrip};
use dbento_core::import::{ItemKind, OnConflict, SkipReason, WarningKind, plan};
use dbento_core::{Color, Engine, TlsMode, Workspace};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

type Pairs = Vec<(Option<String>, Option<String>)>;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/datagrip");

fn opts() -> DataGripOptions {
    DataGripOptions::new().home(Some(PathBuf::from("/home/fixture")))
}

#[test]
fn modern_connections_skips_and_warnings() {
    let set = read_datagrip(&Path::new(FIXTURES).join("modern"), &opts()).unwrap();
    let names: Vec<&str> = set.connections().iter().map(|c| c.name()).collect();
    assert_eq!(names, ["orders-db", "reporting-ro", "app-db"]);
    assert_eq!(
        set.connections()[0].source_id(),
        "11111111-1111-4111-8111-111111111111"
    );
    let skipped: Vec<(&str, &SkipReason)> = set
        .skipped()
        .iter()
        .filter(|s| s.kind() == ItemKind::DataSource)
        .map(|s| (s.subject(), s.reason()))
        .collect();
    assert_eq!(
        skipped,
        [
            ("Cache", &SkipReason::UnsupportedDriver("redis".into())),
            ("Tunnelled", &SkipReason::SshTunnel)
        ]
    );
    assert_eq!(set.warnings()[0].subject(), "Orders DB");
    assert!(
        !set.warnings()
            .iter()
            .any(|w| w.kind() == &WarningKind::ColorsNotImported)
    );
    assert_eq!(
        set.connections()[0].draft().folder().map(|f| f.as_str()),
        Some("backend")
    );
    let settings = set.connections()[0].draft().settings();
    assert_eq!(
        settings.get("application_name").map(String::as_str),
        Some("dbento-fixture")
    );
    assert!(!format!("{set:?}").contains("hunter2"));
}

#[test]
fn a_shared_only_copy_needs_a_default_user_for_postgres() {
    let folder = Path::new(FIXTURES).join("shared-only");
    let set = read_datagrip(&folder, &opts()).unwrap();
    assert_eq!(
        set.connections()
            .iter()
            .map(|c| c.name())
            .collect::<Vec<_>>(),
        ["local-db"]
    );
    assert_eq!(
        set.connections()[0].draft().path(),
        folder.join("local.db").to_str()
    );
    assert_eq!(set.skipped()[0].reason(), &SkipReason::NoUser);
    let set = read_datagrip(&folder, &opts().default_user(Some("analyst".into()))).unwrap();
    assert_eq!(set.connections()[0].draft().user(), Some("analyst"));
}

const NO_UUID: &str = r#"<project version="4"><component name="DataSourceManagerImpl">
<data-source source="LOCAL" name="One"><driver-ref>postgresql</driver-ref><jdbc-url>jdbc:postgresql://one.example/a</jdbc-url></data-source>
<data-source source="LOCAL" name="Two"><driver-ref>postgresql</driver-ref><jdbc-url>jdbc:postgresql://two.example/b</jdbc-url></data-source>
</component></project>"#;
const BLANK_LOCAL: &str = r#"<project version="4"><component name="dataSourceStorageLocal">
<data-source name="Stray"><user-name>leaked</user-name><ssl-config><enabled>true</enabled><mode>VERIFY_FULL</mode></ssl-config></data-source>
</component></project>"#;
const DUP: &str = r#"<project version="4"><component name="DataSourceManagerImpl">
<data-source source="LOCAL" name="One" uuid="cccccccc-0000-4000-8000-000000000001"><driver-ref>postgresql</driver-ref><jdbc-url>jdbc:postgresql://one.example/a</jdbc-url></data-source>
<data-source source="LOCAL" name="Two" uuid="cccccccc-0000-4000-8000-000000000001"><driver-ref>postgresql</driver-ref><jdbc-url>jdbc:postgresql://two.example/b</jdbc-url></data-source>
</component></project>"#;
const DUP_LOCAL: &str = r#"<project version="4"><component name="dataSourceStorageLocal">
<data-source name="Shared" uuid="cccccccc-0000-4000-8000-000000000001"><user-name>leaked</user-name><ssl-config><enabled>true</enabled><mode>VERIFY_FULL</mode></ssl-config></data-source>
</component></project>"#;

/// Reads a project of two Postgres sources with `local` beside it, and returns each draft's
/// user and TLS next to the same project read without the local file.
fn users_and_tls(sources: &str, local: &str) -> (Pairs, Pairs) {
    let read = |with_local: bool| {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("dataSources.xml"), sources).unwrap();
        if with_local {
            std::fs::write(tmp.path().join("dataSources.local.xml"), local).unwrap();
        }
        let set = read_datagrip(tmp.path(), &opts().default_user(Some("analyst".into()))).unwrap();
        set.connections()
            .iter()
            .map(|c| {
                (
                    c.draft().user().map(str::to_owned),
                    c.draft().tls().map(|t| format!("{t:?}")),
                )
            })
            .collect::<Vec<_>>()
    };
    (read(true), read(false))
}

#[test]
fn sources_without_a_uuid_never_pick_up_local_settings() {
    let (with, without) = users_and_tls(NO_UUID, BLANK_LOCAL);
    assert_eq!(with.len(), 2);
    assert_eq!(with, without);
    assert!(
        with.iter()
            .all(|(user, _)| user.as_deref() == Some("analyst"))
    );
}

#[test]
fn sources_sharing_a_uuid_never_pick_up_local_settings() {
    let (with, without) = users_and_tls(DUP, DUP_LOCAL);
    assert_eq!(with.len(), 2);
    assert_eq!(with, without);
}

#[test]
fn no_local_user_means_a_skip_not_a_borrowed_user() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("dataSources.xml"), NO_UUID).unwrap();
    std::fs::write(tmp.path().join("dataSources.local.xml"), BLANK_LOCAL).unwrap();
    let set = read_datagrip(tmp.path(), &opts()).unwrap();
    assert!(set.connections().is_empty());
    assert_eq!(set.skipped().len(), 2);
    assert!(
        set.skipped()
            .iter()
            .all(|s| s.reason() == &SkipReason::NoUser)
    );
}

#[test]
fn data_source_colours_become_tags_and_object_colours_still_warn() {
    let set = read_datagrip(&Path::new(FIXTURES).join("colored"), &opts()).unwrap();
    let colors: Vec<_> = set
        .connections()
        .iter()
        .map(|c| (c.name(), c.draft().color()))
        .collect();
    assert_eq!(
        colors,
        [
            ("orders-db", Some(Color::Red)),
            ("reporting-ro", Some(Color::Blue)),
            ("app-db", None)
        ]
    );
    let colour_warnings: Vec<_> = set
        .warnings()
        .iter()
        .filter(|w| {
            matches!(
                w.kind(),
                WarningKind::ColorUnknown(_) | WarningKind::ColorsNotImported
            )
        })
        .map(|w| (w.subject(), w.kind().clone()))
        .collect();
    assert_eq!(
        colour_warnings,
        [
            ("app.db", WarningKind::ColorUnknown("Mauve".into())),
            ("project", WarningKind::ColorsNotImported)
        ]
    );
}

#[test]
fn mysql_and_mariadb_sources_import_as_mysql_and_forks_are_skipped() {
    let set = read_datagrip(&Path::new(FIXTURES).join("mysql"), &opts()).unwrap();
    let names: Vec<&str> = set.connections().iter().map(|c| c.name()).collect();
    assert_eq!(names, ["shop-mysql", "maria-ro"]);
    let shop = set.connections()[0].draft();
    assert_eq!(
        (
            shop.engine(),
            shop.host(),
            shop.port(),
            shop.database(),
            shop.user(),
            shop.tls()
        ),
        (
            Engine::Mysql,
            Some("db.shop.example"),
            Some(3307),
            Some("shop"),
            Some("shop_app"),
            Some(TlsMode::Verify)
        )
    );
    assert_eq!(
        (shop.folder().map(|f| f.as_str()), shop.color()),
        (Some("backend"), Some(Color::Red))
    );
    let maria = set.connections()[1].draft();
    assert_eq!(
        (
            maria.engine(),
            maria.host(),
            maria.port(),
            maria.database(),
            maria.user()
        ),
        (
            Engine::Mysql,
            Some("localhost"),
            None,
            Some("inventory"),
            Some("maria")
        )
    );
    assert_eq!(
        (maria.read_only(), maria.tls(), maria.color()),
        (true, Some(TlsMode::Off), Some(Color::Blue))
    );
    let skipped: Vec<(&str, &SkipReason)> = set
        .skipped()
        .iter()
        .map(|s| (s.subject(), s.reason()))
        .collect();
    assert_eq!(
        skipped,
        [
            (
                "Aurora",
                &SkipReason::UnsupportedDriver("mysql_aurora_aws".into())
            ),
            ("TiDB", &SkipReason::UnsupportedDriver("tidb".into())),
            ("Cluster", &SkipReason::MultiHost),
            ("Tunnelled", &SkipReason::SshTunnel),
        ]
    );
    let warnings: Vec<(&str, &str)> = set
        .warnings()
        .iter()
        .map(|w| (w.subject(), w.kind().code()))
        .collect();
    assert_eq!(
        warnings,
        [
            ("Shop MySQL", "dropped_parameter"),
            ("Shop MySQL", "ca_cert_missing")
        ]
    );
    let ws = TempDir::new().unwrap();
    plan(ws.path(), &set, OnConflict::Skip)
        .unwrap()
        .apply()
        .unwrap();
    let opened = Workspace::open(ws.path()).unwrap();
    assert!(
        opened
            .connections()
            .iter()
            .all(|c| c.engine() == Engine::Mysql)
    );
    let again = plan(ws.path(), &set, OnConflict::Skip).unwrap();
    assert!(
        again.actions().iter().all(|a| a.code() == "unchanged"),
        "{:?}",
        again.actions()
    );
}
