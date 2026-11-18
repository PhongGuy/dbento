use crate::common::{RULES, history, summary};
use potluck_kind::{Blob, Change, Code, Role};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn cases(kind: &str) -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../dbento-workspace/fixtures")
        .join(kind);
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("{}: {e}", root.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    assert!(!dirs.is_empty(), "{} holds no case", root.display());
    dirs
}

fn read(case: &Path, file: &str) -> String {
    std::fs::read_to_string(case.join(file))
        .unwrap_or_else(|e| panic!("{}/{file}: {e}", case.display()))
}

// A key-path segment, bare when it can be, else quoted as a TOML string;
// fixtures hold no control chars, so no escapes beyond backslash and quote.
fn segment(name: &str) -> String {
    let bare = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if bare {
        name.to_owned()
    } else {
        format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

#[test]
fn every_valid_fixture_passes_as_history_and_as_an_unchanged_tip() {
    for case in cases("valid") {
        let text = read(&case, "dbento.toml");
        assert!(
            history("dbento.toml", &text).is_empty(),
            "{}",
            case.display()
        );
        let blob = Some(Blob::Bytes(text.as_bytes()));
        let change = Change::tip("dbento.toml", blob, blob, Role::Write);
        assert!(RULES.check(&change).is_empty(), "{}", case.display());
        assert_eq!(
            RULES.detect_format(text.as_bytes()).as_deref(),
            Some("1"),
            "{}",
            case.display()
        );
    }
}

#[test]
fn every_invalid_fixture_gives_one_finding_derived_from_its_expect_file() {
    for case in cases("invalid") {
        let expect: toml::Table = read(&case, "expect.toml").parse().unwrap();
        let field = |k: &str| {
            expect
                .get(k)
                .and_then(toml::Value::as_str)
                .unwrap_or_else(|| panic!("{}: expect.toml needs {k}", case.display()))
                .to_owned()
        };
        let code = field("code");
        let want = match code.as_str() {
            "password" => ("password", Code::SecretValue, Some(field("key"))),
            "secret" => (
                "secret",
                Code::SecretValue,
                Some(format!(
                    "connections.{}.{}",
                    segment(&field("connection")),
                    field("key")
                )),
            ),
            "invalid" => (
                "invalid",
                Code::SecretValue,
                Some(format!("connections.{}", segment(&field("connection")))),
            ),
            "invalid-name" => (
                "invalid_name",
                Code::SecretValue,
                Some(format!("connections.{}", segment(&field("connection")))),
            ),
            "parse" => ("parse", Code::SecretValue, None),
            "unsupported-version" => ("unsupported_version", Code::FormatUnsupported, None),
            other => panic!("{}: no derivation for code {other}", case.display()),
        };
        let got = summary(&history("dbento.toml", &read(&case, "dbento.toml")));
        assert_eq!(got, vec![want], "{}", case.display());
    }
}

// Every key at any depth into `keys`, every string value into `strings`.
fn walk(value: &toml::Value, keys: &mut BTreeSet<String>, strings: &mut BTreeSet<String>) {
    match value {
        toml::Value::String(s) => {
            strings.insert(s.clone());
        }
        toml::Value::Array(items) => {
            for item in items {
                walk(item, keys, strings);
            }
        }
        toml::Value::Table(table) => {
            for (k, v) in table {
                keys.insert(k.clone());
                walk(v, keys, strings);
            }
        }
        _ => {}
    }
}

#[test]
fn no_detail_carries_a_string_value_from_its_fixture() {
    let mut details = 0;
    for case in cases("valid").into_iter().chain(cases("invalid")) {
        let text = read(&case, "dbento.toml");
        let (mut keys, mut strings) = (BTreeSet::new(), BTreeSet::new());
        if let Ok(table) = text.parse::<toml::Table>() {
            walk(&toml::Value::Table(table), &mut keys, &mut strings);
        }
        for finding in history("dbento.toml", &text) {
            let Some(detail) = finding.detail() else {
                continue;
            };
            details += 1;
            for value in strings.difference(&keys) {
                assert!(
                    value.is_empty() || !detail.contains(value.as_str()),
                    "{}: detail {detail:?} carries {value:?}",
                    case.display()
                );
            }
        }
    }
    assert!(details > 0, "no fixture produced a detail to check");
}
