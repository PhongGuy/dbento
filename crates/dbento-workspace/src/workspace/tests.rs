use super::OpenError;
use std::collections::BTreeSet;
use std::path::PathBuf;

// Its position in the enum; exhaustive, so a new variant fails to compile here first.
fn ordinal(e: &OpenError) -> usize {
    match e {
        OpenError::NotFound { .. } => 0,
        OpenError::Io { .. } => 1,
        OpenError::Parse { .. } => 2,
        OpenError::Password { .. } => 3,
        OpenError::UnsupportedVersion { .. } => 4,
        OpenError::InvalidName { .. } => 5,
        OpenError::Invalid { .. } => 6,
        OpenError::InvalidFolder { .. } => 7,
        OpenError::Secret { .. } => 8,
    }
}

fn every_variant() -> Vec<OpenError> {
    let s = String::new;
    vec![
        OpenError::NotFound {
            start: PathBuf::new(),
            parents: false,
        },
        OpenError::Io {
            path: PathBuf::new(),
            message: s(),
        },
        OpenError::Parse {
            path: PathBuf::new(),
            message: s(),
        },
        OpenError::Password { key: s() },
        OpenError::UnsupportedVersion { found: 2 },
        OpenError::InvalidName { name: s() },
        OpenError::Invalid {
            connection: s(),
            message: s(),
        },
        OpenError::InvalidFolder {
            folder: s(),
            message: s(),
        },
        OpenError::Secret {
            connection: s(),
            key: s(),
            message: s(),
        },
    ]
}

#[test]
fn every_open_error_variant_has_its_own_stable_code() {
    let all = every_variant();
    let ordinals: Vec<usize> = all.iter().map(ordinal).collect();
    assert_eq!(
        ordinals,
        (0..9).collect::<Vec<_>>(),
        "every_variant misses a variant"
    );
    let codes: Vec<&str> = all.iter().map(OpenError::code).collect();
    assert_eq!(
        codes,
        [
            "not-found",
            "io",
            "parse",
            "password",
            "unsupported-version",
            "invalid-name",
            "invalid",
            "invalid-folder",
            "secret",
        ]
    );
    let distinct: BTreeSet<&str> = codes.iter().copied().collect();
    assert_eq!(distinct.len(), codes.len(), "{codes:?}");
}

#[test]
fn a_secret_error_reads_exactly_as_an_invalid_one() {
    let secret = OpenError::Secret {
        connection: "docs".into(),
        key: "uri".into(),
        message: "m".into(),
    };
    let invalid = OpenError::Invalid {
        connection: "docs".into(),
        message: "m".into(),
    };
    assert_eq!(secret.to_string(), invalid.to_string());
    assert_eq!(secret.to_string(), "connection `docs`: m");
}
