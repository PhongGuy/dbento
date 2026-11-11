//! `read_dbeaver` on the committed DBeaver fixtures, then planned into a workspace.
use dbento_core::import::dbeaver::{DBeaverError, DBeaverOptions, read_dbeaver};
use dbento_core::import::{ItemKind, OnConflict, SkipReason, StoredSecrets, plan, store_secrets};
use dbento_core::{Color, Engine, MemoryStore, Secret, SecretKey, SecretStore, TlsMode, Workspace};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/dbeaver");

fn fixture(name: &str) -> PathBuf {
    Path::new(FIXTURES).join(name)
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(files(&path))
        } else {
            out.push(path)
        }
    }
    out
}

#[test]
fn the_modern_project_reads_into_named_drafts_skips_and_secrets() {
    let read = read_dbeaver(&fixture("modern"), &DBeaverOptions::new()).unwrap();
    let set = read.set();
    let names: Vec<(&str, &str)> = set
        .connections()
        .iter()
        .map(|c| (c.source_id(), c.name()))
        .collect();
    assert_eq!(
        names,
        [
            ("postgres-jdbc-a1", "orders-db"),
            ("postgres-jdbc-b2", "reporting-ro"),
            ("sqlite-jdbc-c3", "app-db"),
            ("sqlite_xerial-d4", "legacy-lite"),
            ("postgres-jdbc-i9", "orders-db-2")
        ]
    );
    let users: Vec<Option<&str>> = set.connections().iter().map(|c| c.draft().user()).collect();
    assert_eq!(
        users,
        [
            Some("orders_app"),
            Some("reporter"),
            None,
            None,
            Some("orders_ro")
        ]
    );
    let a1 = set.connections()[0].draft();
    assert_eq!(
        (
            a1.host(),
            a1.port(),
            a1.database(),
            a1.tls(),
            a1.folder().map(|f| f.as_str())
        ),
        (
            Some("db.orders.example"),
            Some(5433),
            Some("orders"),
            Some(TlsMode::Verify),
            Some("Prod/EU")
        )
    );
    assert_eq!(
        a1.settings().get("application_name").map(String::as_str),
        Some("dbento-fixture")
    );
    let b2 = set.connections()[1].draft();
    assert_eq!(
        (b2.host(), b2.port(), b2.read_only(), b2.tls()),
        (Some("localhost"), None, true, Some(TlsMode::Off))
    );
    assert_eq!(
        b2.settings().get("search_path").map(String::as_str),
        Some("reporting")
    );
    let skipped: Vec<(ItemKind, &str, &SkipReason)> = set
        .skipped()
        .iter()
        .map(|s| (s.kind(), s.subject(), s.reason()))
        .collect();
    let fork = SkipReason::UnsupportedDriver("postgresql/postgres-timescale-jdbc".into());
    assert_eq!(
        skipped,
        [
            (ItemKind::DataSource, "Tunnelled", &SkipReason::SshTunnel),
            (ItemKind::DataSource, "Proxied", &SkipReason::Proxy),
            (ItemKind::DataSource, "Timeseries", &fork)
        ]
    );
    let secrets: Vec<(&str, &str)> = read
        .secrets()
        .ids()
        .map(|id| (id, read.secrets().get(id).map(Secret::expose).unwrap()))
        .collect();
    assert_eq!(
        secrets,
        [
            ("postgres-jdbc-a1", "fake-pw-1"),
            ("postgres-jdbc-b2", "fake-pw-2"),
            ("postgres-jdbc-i9", "fake-pw-9")
        ]
    );
    let bound: Vec<(Option<&str>, String)> = set
        .queries()
        .iter()
        .map(|q| (q.connection(), q.relative().display().to_string()))
        .collect();
    assert_eq!(
        bound,
        [
            (None, "notes.sql".to_owned()),
            (Some("postgres-jdbc-a1"), "orders/top.sql".to_owned()),
            (None, "tunnel.sql".to_owned())
        ]
    );
}

#[test]
fn every_draft_opens_and_no_fake_password_reaches_a_file_or_debug_output() {
    let read = read_dbeaver(&fixture("modern"), &DBeaverOptions::new()).unwrap();
    let ws = TempDir::new().unwrap();
    plan(ws.path(), read.set(), OnConflict::Skip)
        .unwrap()
        .apply()
        .unwrap();
    assert_eq!(Workspace::open(ws.path()).unwrap().connections().len(), 5);
    let mut texts = vec![format!("{read:?}")];
    for path in files(ws.path()) {
        texts.push(String::from_utf8_lossy(&std::fs::read(path).unwrap()).into_owned());
    }
    for text in &texts {
        assert!(
            !text.contains("fake-pw") && !text.contains("fake-ssh"),
            "{text}"
        );
    }
}

#[test]
fn encrypted_malformed_and_workspace_roots_are_refused() {
    let opts = DBeaverOptions::new();
    assert!(matches!(
        read_dbeaver(&fixture("encrypted"), &opts),
        Err(DBeaverError::EncryptedProject { .. })
    ));
    assert!(matches!(
        read_dbeaver(&fixture("malformed"), &opts),
        Err(DBeaverError::Json { .. })
    ));
    match read_dbeaver(&fixture("workspace"), &opts) {
        Err(DBeaverError::NotAProject { projects, .. }) => {
            assert_eq!(projects, ["General", "Work"])
        }
        other => panic!("{other:?}"),
    }
    let general = read_dbeaver(&fixture("workspace").join("General"), &opts).unwrap();
    assert!(general.set().connections().is_empty());
}

/// Plans the modern fixture into `ws` with renames, applies it and stores its secrets in `store`.
fn import_with_secrets(ws: &Path, store: &MemoryStore) -> StoredSecrets {
    let read = read_dbeaver(&fixture("modern"), &DBeaverOptions::new()).unwrap();
    let p = plan(ws, read.set(), OnConflict::Rename).unwrap();
    let imported = p.imported().to_vec();
    p.apply().unwrap();
    store_secrets(ws, &imported, read.secrets(), store, &|_| None)
}

fn stored(ws: &Path, store: &MemoryStore, name: &str) -> Option<String> {
    let ws = Workspace::open(ws).unwrap();
    let key = SecretKey::for_connection(&ws, ws.declared(name).unwrap());
    store.get(&key).unwrap().map(|s| s.expose().to_owned())
}

#[test]
fn secrets_land_under_the_final_names_and_a_rerun_stores_and_replaces_nothing() {
    // A different `orders-db` already holds the user's own secret, so Orders DB is renamed past
    // it and the set's own `orders-db-2`; that secret must survive both runs.
    let ws = TempDir::new().unwrap();
    std::fs::write(
        ws.path().join("dbento.toml"),
        "dbento = 1\n[connections.orders-db]\nengine = \"postgres\"\nhost = \"mine.example\"\ndatabase = \"mine\"\nuser = \"me\"\n",
    )
    .unwrap();
    let store = MemoryStore::new();
    let own = Workspace::open(ws.path()).unwrap();
    let own_key = SecretKey::for_connection(&own, own.declared("orders-db").unwrap());
    store.set(&own_key, &Secret::new("users-own")).unwrap();

    let first = import_with_secrets(ws.path(), &store);
    assert_eq!(
        first.stored(),
        ["orders-db-3", "reporting-ro", "orders-db-2"]
    );
    assert!(first.needs_secret().is_empty());
    let expected = [
        ("orders-db", "users-own"),
        ("orders-db-3", "fake-pw-1"),
        ("reporting-ro", "fake-pw-2"),
        ("orders-db-2", "fake-pw-9"),
    ];
    for (name, pw) in expected {
        assert_eq!(
            stored(ws.path(), &store, name).as_deref(),
            Some(pw),
            "{name}"
        );
    }

    let second = import_with_secrets(ws.path(), &store);
    assert!(second.stored().is_empty(), "{second:?}");
    assert_eq!(
        second.present(),
        ["orders-db-3", "reporting-ro", "orders-db-2"]
    );
    for (name, pw) in expected {
        assert_eq!(
            stored(ws.path(), &store, name).as_deref(),
            Some(pw),
            "{name}"
        );
    }
}

#[test]
fn the_mysql_project_imports_both_flavours_and_stores_each_secret_once() {
    let read = read_dbeaver(&fixture("mysql"), &DBeaverOptions::new()).unwrap();
    let set = read.set();
    let names: Vec<(&str, &str)> = set
        .connections()
        .iter()
        .map(|c| (c.source_id(), c.name()))
        .collect();
    assert_eq!(names, [("my-a1", "shop-mysql"), ("my-b2", "maria-ro")]);
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
        (Some("Shop"), Some(Color::Red))
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
        (true, Some(TlsMode::Off), None)
    );
    let skipped: Vec<(ItemKind, &str, &SkipReason)> = set
        .skipped()
        .iter()
        .map(|s| (s.kind(), s.subject(), s.reason()))
        .collect();
    let stars = SkipReason::UnsupportedDriver("mysql/starRocks".into());
    assert_eq!(
        skipped,
        [
            (ItemKind::DataSource, "Stars", &stars),
            (ItemKind::DataSource, "Tunnelled", &SkipReason::SshTunnel)
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
            ("Shop MySQL", "tls_stricter")
        ]
    );
    let secrets: Vec<(&str, &str)> = read
        .secrets()
        .ids()
        .map(|id| (id, read.secrets().get(id).map(Secret::expose).unwrap()))
        .collect();
    assert_eq!(
        secrets,
        [("my-a1", "fake-pw-my-1"), ("my-b2", "fake-pw-my-2")]
    );

    let ws = TempDir::new().unwrap();
    let store = MemoryStore::new();
    for run in 0..2 {
        let p = plan(ws.path(), set, OnConflict::Skip).unwrap();
        let imported = p.imported().to_vec();
        p.apply().unwrap();
        let outcome = store_secrets(ws.path(), &imported, read.secrets(), &store, &|_| None);
        if run == 0 {
            assert_eq!(outcome.stored(), ["shop-mysql", "maria-ro"]);
        } else {
            assert!(outcome.stored().is_empty(), "{outcome:?}");
            assert_eq!(outcome.present(), ["shop-mysql", "maria-ro"]);
        }
    }
    assert_eq!(
        stored(ws.path(), &store, "shop-mysql").as_deref(),
        Some("fake-pw-my-1")
    );
    assert_eq!(
        stored(ws.path(), &store, "maria-ro").as_deref(),
        Some("fake-pw-my-2")
    );
    for path in files(ws.path()) {
        let text = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
        assert!(!text.contains("fake-pw"), "{}: {text}", path.display());
    }
}
