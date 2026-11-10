#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use dbento_core::{Color, Workspace};

fn example() -> PathBuf {
    xtask::workspace_root().join("examples/demo")
}

/// A copy of the committed demo with `shop.sqlite` built from its seed, as `make demo` makes.
fn built_copy(tmp: &Path) -> PathBuf {
    let dir = tmp.join("workspace");
    let status = Command::new("cp")
        .arg("-R")
        .arg(example())
        .arg(&dir)
        .status()
        .unwrap();
    assert!(status.success(), "the example workspace copies");
    xtask::demo::build_db(&dir.join("seed.sql"), &dir.join("shop.sqlite")).unwrap();
    dir
}

fn count(db: &rusqlite::Connection, table: &str) -> i64 {
    db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn the_seed_builds_the_shop_with_its_row_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = built_copy(tmp.path());
    let db = rusqlite::Connection::open(dir.join("shop.sqlite")).unwrap();
    for (table, rows) in [
        ("customers", 500),
        ("products", 120),
        ("orders", 4000),
        ("order_items", 10000),
        ("events", 200_000),
    ] {
        assert_eq!(count(&db, table), rows, "{table}");
    }
    // Unicode names and some products nobody ordered are what the demo queries show off.
    let cjk: i64 = db
        .query_row(
            "SELECT count(*) FROM customers WHERE name LIKE '%山田%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(cjk > 0);
    let unordered: i64 = db
        .query_row(
            "SELECT count(*) FROM products WHERE id NOT IN (SELECT product_id FROM order_items)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(unordered > 0);
}

#[test]
fn building_twice_replaces_the_database() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = built_copy(tmp.path());
    xtask::demo::build_db(&dir.join("seed.sql"), &dir.join("shop.sqlite")).unwrap();
    let db = rusqlite::Connection::open(dir.join("shop.sqlite")).unwrap();
    assert_eq!(count(&db, "customers"), 500);
}

#[test]
fn the_demo_workspace_opens_with_its_colours_and_saved_queries() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = built_copy(tmp.path());
    let ws = Workspace::open(&dir).unwrap();

    let shop = ws.connection("shop").unwrap();
    assert!(!shop.read_only());
    assert_eq!(shop.effective_color(), Some(Color::Teal));
    let prod = ws.connection("shop-prod").unwrap();
    assert!(prod.read_only());
    assert_eq!(prod.effective_color(), Some(Color::Red));
    assert_eq!(prod.sqlite_path(), shop.sqlite_path());
    assert_eq!(shop.sqlite_path(), Some(dir.join("shop.sqlite").as_path()));

    // Every saved query belongs to a connection and compiles against the built database.
    let db = rusqlite::Connection::open(dir.join("shop.sqlite")).unwrap();
    let saved = ws.saved_queries().unwrap();
    assert!(saved.len() >= 7, "{saved:?}");
    for q in saved {
        assert!(q.connection().is_some(), "{q:?}");
        let sql = std::fs::read_to_string(dir.join(q.path())).unwrap();
        db.prepare(&sql)
            .unwrap_or_else(|e| panic!("{}: {e}", q.path().display()));
    }
}

#[test]
fn demo_db_is_an_xtask_command() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("shop.sqlite");
    let status = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("demo-db")
        .arg(example().join("seed.sql"))
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success());
    let db = rusqlite::Connection::open(&out).unwrap();
    assert_eq!(count(&db, "products"), 120);
}

/// Runs `scripts/demo.sh` from `cwd` in dry-run mode with `env` on top of a clean environment.
fn demo_sh(cwd: &Path, env: &[(&str, &Path)]) -> std::process::Output {
    let mut cmd = Command::new("bash");
    cmd.arg(xtask::workspace_root().join("scripts/demo.sh"))
        .current_dir(cwd)
        .env_remove("XDG_CACHE_HOME")
        .env_remove("CARGO_TARGET_DIR")
        .env("DEMO_DRY_RUN", "1")
        .env("DBENTO_XTASK", env!("CARGO_BIN_EXE_xtask"));
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

#[test]
fn the_script_copies_the_demo_fresh_and_prints_the_launch_line() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().join("dbento-demo");
    // A file left by an earlier run must not survive the fresh copy.
    std::fs::create_dir_all(base.join("workspace")).unwrap();
    std::fs::write(base.join("workspace/stale.sql"), "x").unwrap();

    let out = demo_sh(tmp.path(), &[("XDG_CACHE_HOME", tmp.path())]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let app = xtask::workspace_root().join("target/release/dbento-app");
    let want = format!(
        "DBENTO_STATE_DIR={} {} {}",
        base.join("state").display(),
        app.display(),
        base.join("workspace").display()
    );
    assert_eq!(stdout.trim_end(), want);
    assert!(!base.join("workspace/stale.sql").exists());
    assert!(base.join("state").is_dir());
    let ws = Workspace::open(base.join("workspace")).unwrap();
    assert!(ws.connection("shop-prod").unwrap().read_only());
    let db = rusqlite::Connection::open(base.join("workspace/shop.sqlite")).unwrap();
    assert_eq!(count(&db, "events"), 200_000);
}

#[test]
fn the_script_refuses_a_cache_dir_that_is_not_absolute() {
    let tmp = tempfile::tempdir().unwrap();
    // A relative HOME makes the fallback cache relative, which the guard must refuse.
    let out = demo_sh(tmp.path(), &[("HOME", Path::new("relative-home"))]);
    assert!(!out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("refusing"),
        "{out:?}"
    );
    assert!(!tmp.path().join("relative-home").exists());
}
