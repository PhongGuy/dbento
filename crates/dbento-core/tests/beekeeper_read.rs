//! `read_beekeeper` on the committed SQL fixtures, built into app.db in a tempdir.
use dbento_core::import::beekeeper::{BeekeeperOptions, read_beekeeper};
use dbento_core::import::{ItemKind, OnConflict, SkipReason, plan, store_secrets};
use dbento_core::{Color, Engine, MemoryStore, SecretKey, SecretStore, TlsMode, Workspace};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/beekeeper");
const FIXTURE_KEY: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";

/// `<tmp>/beekeeper-studio` with app.db built from the fixture `sql` and the fixture `.key`.
fn data_folder(sql: &str) -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("beekeeper-studio");
    std::fs::create_dir(&dir).unwrap();
    let script = std::fs::read_to_string(Path::new(FIXTURES).join(sql)).unwrap();
    rusqlite::Connection::open(dir.join("app.db"))
        .unwrap()
        .execute_batch(&script)
        .unwrap();
    std::fs::copy(Path::new(FIXTURES).join(".key"), dir.join(".key")).unwrap();
    (tmp, dir)
}

/// Every file below `dir`, path to bytes.
fn files(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(files(&path));
        } else {
            let bytes = std::fs::read(&path).unwrap();
            out.insert(path, bytes);
        }
    }
    out
}

fn stored(ws: &Path, store: &MemoryStore, name: &str) -> Option<String> {
    let ws = Workspace::open(ws).unwrap();
    let key = SecretKey::for_connection(&ws, ws.connection(name).unwrap());
    store.get(&key).unwrap().map(|s| s.expose().to_owned())
}

#[test]
fn the_modern_fixture_reads_into_named_drafts_skips_warnings_and_secrets() {
    let (_tmp, dir) = data_folder("modern.sql");
    let read = read_beekeeper(&dir, &BeekeeperOptions::new()).unwrap();
    let set = read.set();
    let names: Vec<(&str, &str)> = set
        .connections()
        .iter()
        .map(|c| (c.source_id(), c.name()))
        .collect();
    assert_eq!(
        names,
        [
            ("1", "orders-db"),
            ("2", "reporting-ro"),
            ("3", "local-socket"),
            ("4", "client-certs"),
            ("5", "iam"),
            ("6", "shop-lite"),
            ("7", "abs-lite"),
            ("11", "broken-secret"),
            ("13", "orders-db-2")
        ]
    );
    let d: Vec<_> = set.connections().iter().map(|c| c.draft()).collect();
    assert_eq!(
        (
            d[0].host(),
            d[0].port(),
            d[0].database(),
            d[0].user(),
            d[0].tls(),
            d[0].folder().map(|f| f.as_str())
        ),
        (
            Some("db.orders.example"),
            Some(5433),
            Some("orders"),
            Some("orders_app"),
            Some(TlsMode::Verify),
            Some("Prod/EU")
        )
    );
    assert_eq!(
        (d[1].host(), d[1].port(), d[1].read_only(), d[1].tls()),
        (Some("localhost"), None, true, Some(TlsMode::Verify))
    );
    assert_eq!(
        (d[2].host(), d[2].tls()),
        (Some("/var/run/postgresql"), Some(TlsMode::Off))
    );
    // Only Orders DB has a label colour; the rest are `default`.
    let colors: Vec<_> = d.iter().map(|d| d.color()).collect();
    assert_eq!(colors[0], Some(Color::Red));
    assert!(colors[1..].iter().all(Option::is_none), "{colors:?}");
    let shop = dir.join("lite/shop.db");
    assert_eq!(d[5].path(), shop.to_str());
    assert_eq!(
        (d[6].path(), d[6].read_only()),
        (Some("/nonexistent/dbento-fixture/abs.db"), true)
    );
    assert_eq!(
        (d[8].folder().map(|f| f.as_str()), d[8].tls()),
        (Some("Prod"), Some(TlsMode::Off))
    );

    let app = dir.join("app.db").display().to_string();
    let skipped: Vec<(ItemKind, String, SkipReason)> = set
        .skipped()
        .iter()
        .map(|s| (s.kind(), s.subject().to_owned(), s.reason().clone()))
        .collect();
    let ds = ItemKind::DataSource;
    assert_eq!(
        skipped,
        [
            (
                ds,
                "Cockroach".to_owned(),
                SkipReason::UnsupportedDriver("cockroachdb".into())
            ),
            (ds, "Tunnelled".to_owned(), SkipReason::SshTunnel),
            (ds, "Cloud".to_owned(), SkipReason::CloudWorkspace),
            (ds, "No user".to_owned(), SkipReason::NoUser),
            (
                ItemKind::Query,
                format!("{app}#favorite_query/5"),
                SkipReason::EmptyQuery
            ),
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
            ("Orders DB", "ca_cert_missing"),
            ("Reporting (RO)", "tls_stricter"),
            ("Client certs", "tls_stricter"),
            ("Client certs", "client_cert_missing"),
            ("IAM", "auth_model"),
            ("Shop lite", "file_missing"),
            ("Abs lite", "file_missing"),
            ("Broken secret", "secret_unreadable"),
        ]
    );
    let secrets: Vec<(&str, &str)> = read
        .secrets()
        .ids()
        .map(|id| (id, read.secrets().get(id).unwrap().expose()))
        .collect();
    assert_eq!(
        secrets,
        [
            ("1", "fake-pw-orders"),
            ("13", "fake-pw-orders-2"),
            ("2", "fake-pw-reports-é"),
            ("4", "fake-pw-certs")
        ]
    );
    let queries: Vec<String> = set
        .queries()
        .iter()
        .map(|q| q.relative().display().to_string())
        .collect();
    assert_eq!(
        queries,
        [
            "Reports/Top orders.sql",
            "Reports/Top orders.sql",
            "Reports/Top orders.sql",
            "Reports/Monthly _ Q_/a_b_ c_.sql",
            "_CON.sql",
            "query.sql"
        ]
    );
    assert!(set.queries().iter().all(|q| q.connection().is_none()));
}

#[test]
fn a_default_user_imports_the_connection_that_names_none() {
    let (_tmp, dir) = data_folder("modern.sql");
    let opts = BeekeeperOptions::new().default_user(Some("fallback".into()));
    let read = read_beekeeper(&dir, &opts).unwrap();
    let no_user = read
        .set()
        .connections()
        .iter()
        .find(|c| c.source_id() == "12")
        .unwrap();
    assert_eq!(
        (no_user.name(), no_user.draft().user()),
        ("no-user", Some("fallback"))
    );
}

#[test]
fn drafts_open_secrets_are_stored_and_no_password_or_key_reaches_a_file_or_debug_output() {
    let (_tmp, dir) = data_folder("modern.sql");
    let source_before = files(&dir);
    let read = read_beekeeper(&dir, &BeekeeperOptions::new()).unwrap();
    let ws = TempDir::new().unwrap();
    let p = plan(ws.path(), read.set(), OnConflict::Skip).unwrap();
    let imported = p.imported().to_vec();
    p.apply().unwrap();
    assert_eq!(Workspace::open(ws.path()).unwrap().connections().len(), 9);
    let store = MemoryStore::new();
    let outcome = store_secrets(ws.path(), &imported, read.secrets(), &store, &|_| None);
    assert_eq!(
        outcome.stored(),
        ["orders-db", "reporting-ro", "client-certs", "orders-db-2"]
    );
    assert_eq!(
        stored(ws.path(), &store, "reporting-ro").as_deref(),
        Some("fake-pw-reports-é")
    );
    assert_eq!(stored(ws.path(), &store, "local-socket"), None);
    let second = std::fs::read(ws.path().join("queries/Reports/Top orders-2.sql")).unwrap();
    assert_eq!(second, b"select count(*) from orders;");
    let mut texts = vec![format!("{read:?}")];
    texts.extend(
        files(ws.path())
            .values()
            .map(|b| String::from_utf8_lossy(b).into_owned()),
    );
    for text in &texts {
        assert!(
            !text.contains("fake-pw") && !text.contains("fake-ssh") && !text.contains(FIXTURE_KEY),
            "{text}"
        );
    }
    assert_eq!(files(&dir), source_before);
}

#[test]
fn a_rerun_changes_nothing_and_stores_nothing() {
    let (_tmp, dir) = data_folder("modern.sql");
    let read = read_beekeeper(&dir, &BeekeeperOptions::new()).unwrap();
    let ws = TempDir::new().unwrap();
    let store = MemoryStore::new();
    let first = plan(ws.path(), read.set(), OnConflict::Skip).unwrap();
    let imported = first.imported().to_vec();
    first.apply().unwrap();
    store_secrets(ws.path(), &imported, read.secrets(), &store, &|_| None);
    let before = files(ws.path());
    let again = plan(ws.path(), read.set(), OnConflict::Skip).unwrap();
    assert!(
        again.actions().iter().all(|a| a.code() == "unchanged"),
        "{:?}",
        again.actions()
    );
    let imported = again.imported().to_vec();
    again.apply().unwrap();
    let outcome = store_secrets(ws.path(), &imported, read.secrets(), &store, &|_| None);
    assert!(outcome.stored().is_empty());
    assert_eq!(
        outcome.present(),
        ["orders-db", "reporting-ro", "client-certs", "orders-db-2"]
    );
    assert_eq!(files(ws.path()), before);
}

#[test]
fn the_old_fixture_reads_with_migration_defaults() {
    let (_tmp, dir) = data_folder("old.sql");
    let read = read_beekeeper(&dir, &BeekeeperOptions::new()).unwrap();
    let c = read.set().connections();
    assert_eq!(
        c.iter().map(|c| c.name()).collect::<Vec<_>>(),
        ["legacy-pg", "legacy-lite"]
    );
    let pg = c[0].draft();
    assert_eq!(
        (pg.port(), pg.tls(), pg.read_only(), pg.folder()),
        (None, Some(TlsMode::Off), false, None)
    );
    assert_eq!(
        read.secrets().get("1").map(|s| s.expose()),
        Some("fake-pw-legacy")
    );
    let queries: Vec<String> = read
        .set()
        .queries()
        .iter()
        .map(|q| q.relative().display().to_string())
        .collect();
    assert_eq!(queries, ["Old query.sql"]);
}

#[test]
fn without_a_key_file_nothing_is_decrypted() {
    let (_tmp, dir) = data_folder("modern.sql");
    std::fs::remove_file(dir.join(".key")).unwrap();
    let read = read_beekeeper(&dir, &BeekeeperOptions::new()).unwrap();
    assert!(read.secrets().is_empty());
    let first = &read.set().warnings()[0];
    assert_eq!(
        (first.subject(), first.kind().code()),
        (
            dir.join(".key").display().to_string().as_str(),
            "key_missing"
        )
    );
    assert!(
        read.set()
            .warnings()
            .iter()
            .all(|w| w.kind().code() != "secret_unreadable")
    );
}

#[test]
fn the_mysql_fixture_imports_both_flavours_and_stores_each_secret_once() {
    let (_tmp, dir) = data_folder("mysql.sql");
    let read = read_beekeeper(&dir, &BeekeeperOptions::new()).unwrap();
    let set = read.set();
    let names: Vec<(&str, &str)> = set
        .connections()
        .iter()
        .map(|c| (c.source_id(), c.name()))
        .collect();
    assert_eq!(names, [("1", "shop-mysql"), ("2", "maria-ro")]);
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
            maria.read_only(),
            maria.tls()
        ),
        (
            Engine::Mysql,
            Some("localhost"),
            None,
            Some("inventory"),
            true,
            Some(TlsMode::Off)
        )
    );
    let skipped: Vec<(String, SkipReason)> = set
        .skipped()
        .iter()
        .map(|s| (s.subject().to_owned(), s.reason().clone()))
        .collect();
    assert_eq!(
        skipped,
        [
            ("Socket".to_owned(), SkipReason::UnixSocket),
            (
                "TiDB".to_owned(),
                SkipReason::UnsupportedDriver("tidb".into())
            ),
            (
                "IAM".to_owned(),
                SkipReason::UnsupportedDriver("aws_iam".into())
            ),
            ("No user".to_owned(), SkipReason::NoUser),
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
            ("Shop MySQL", "tls_stricter"),
            ("Shop MySQL", "ca_cert_missing")
        ]
    );
    let secrets: Vec<(&str, &str)> = read
        .secrets()
        .ids()
        .map(|id| (id, read.secrets().get(id).unwrap().expose()))
        .collect();
    assert_eq!(secrets, [("1", "fake-pw-orders"), ("2", "fake-pw-certs")]);

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
            assert!(outcome.stored().is_empty());
            assert_eq!(outcome.present(), ["shop-mysql", "maria-ro"]);
        }
    }
    assert_eq!(
        stored(ws.path(), &store, "maria-ro").as_deref(),
        Some("fake-pw-certs")
    );
    for (path, bytes) in files(ws.path()) {
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("fake-pw"), "{}: {text}", path.display());
    }
}
