//! Every fixture under `fixtures/` through `Workspace::from_text`, checked against its expect file.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![warn(clippy::wildcard_enum_match_arm)]

use dbento_workspace::{Color, OpenError, TEMPLATE, Workspace};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn cases(kind: &str) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root().join(kind))
        .unwrap_or_else(|e| panic!("fixtures/{kind}: {e}"))
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    assert!(!dirs.is_empty(), "fixtures/{kind} holds no case");
    dirs
}

fn load(case: &Path) -> (Result<Workspace, OpenError>, toml::Table) {
    let text = std::fs::read_to_string(case.join("dbento.toml"))
        .unwrap_or_else(|e| panic!("{}: dbento.toml: {e}", case.display()));
    let expect = std::fs::read_to_string(case.join("expect.toml"))
        .unwrap_or_else(|e| panic!("{} has no expect.toml: {e}", case.display()));
    let expect: toml::Table = expect
        .parse()
        .unwrap_or_else(|e| panic!("{}: expect.toml: {e}", case.display()));
    let ws = Workspace::from_text(
        PathBuf::from("/fixture"),
        Path::new("/fixture/dbento.toml"),
        &text,
    );
    (ws, expect)
}

fn word(c: Color) -> &'static str {
    match c {
        Color::Red => "red",
        Color::Orange => "orange",
        Color::Yellow => "yellow",
        Color::Green => "green",
        Color::Teal => "teal",
        Color::Blue => "blue",
        Color::Purple => "purple",
        Color::Gray => "gray",
    }
}

// The dotted key and the connection an error names, where it names them.
fn named(e: &OpenError) -> (Option<&str>, Option<&str>) {
    match e {
        OpenError::Password { key } => (Some(key), None),
        OpenError::Secret {
            connection, key, ..
        } => (Some(key), Some(connection)),
        OpenError::Invalid { connection, .. } => (None, Some(connection)),
        OpenError::InvalidName { name } => (None, Some(name)),
        OpenError::NotFound { .. }
        | OpenError::Io { .. }
        | OpenError::Parse { .. }
        | OpenError::UnsupportedVersion { .. }
        | OpenError::InvalidFolder { .. }
        | _ => (None, None),
    }
}

#[test]
fn every_valid_fixture_opens_with_its_expected_effective_values() {
    for case in cases("valid") {
        let (ws, expect) = load(&case);
        let ws = ws.unwrap_or_else(|e| panic!("{}: {e}", case.display()));
        let empty = toml::Table::new();
        let want = expect
            .get("connections")
            .and_then(toml::Value::as_table)
            .unwrap_or(&empty);
        assert_eq!(
            want.len(),
            ws.connections().len(),
            "{}: connection count",
            case.display()
        );
        for conn in ws.connections() {
            let w = want
                .get(conn.name())
                .and_then(toml::Value::as_table)
                .unwrap_or_else(|| panic!("{}: no expect for {}", case.display(), conn.name()));
            for key in w.keys() {
                assert!(
                    ["read_only", "color"].contains(&key.as_str()),
                    "{}: {}: unknown expect key {key}",
                    case.display(),
                    conn.name()
                );
            }
            let read_only = w
                .get("read_only")
                .and_then(toml::Value::as_bool)
                .unwrap_or_else(|| panic!("{}: {} needs read_only", case.display(), conn.name()));
            assert_eq!(
                conn.read_only(),
                read_only,
                "{}: {}",
                case.display(),
                conn.name()
            );
            let color = w.get("color").and_then(toml::Value::as_str);
            assert_eq!(
                conn.effective_color().map(word),
                color,
                "{}: {}",
                case.display(),
                conn.name()
            );
        }
    }
}

#[test]
fn every_invalid_fixture_fails_with_its_expected_code() {
    for case in cases("invalid") {
        let (ws, expect) = load(&case);
        let Err(e) = ws else {
            panic!("{}: opened", case.display());
        };
        for key in expect.keys() {
            assert!(
                ["code", "key", "connection"].contains(&key.as_str()),
                "{}: unknown expect key {key}",
                case.display()
            );
        }
        let code = expect
            .get("code")
            .and_then(toml::Value::as_str)
            .unwrap_or_else(|| panic!("{}: expect.toml needs code", case.display()));
        assert_eq!(e.code(), code, "{}: {e}", case.display());
        let (key, connection) = named(&e);
        if let Some(want) = expect.get("key") {
            assert_eq!(key, want.as_str(), "{}: key", case.display());
        }
        if let Some(want) = expect.get("connection") {
            assert_eq!(connection, want.as_str(), "{}: connection", case.display());
        }
    }
}

#[test]
fn the_seed_fixture_and_init_are_exactly_the_template() {
    let seed = std::fs::read_to_string(root().join("valid/seed/dbento.toml")).unwrap();
    assert_eq!(seed, TEMPLATE);
    let dir = tempfile::tempdir().unwrap();
    Workspace::init(dir.path()).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("dbento.toml")).unwrap(),
        TEMPLATE
    );
    assert!(dir.path().join("queries").is_dir());
}
