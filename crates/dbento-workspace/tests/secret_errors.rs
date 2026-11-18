use dbento_workspace::{OpenError, Workspace};
use std::path::{Path, PathBuf};

fn open_err(toml: &str) -> OpenError {
    match Workspace::from_text(PathBuf::from("/ws"), Path::new("/ws/dbento.toml"), toml) {
        Ok(_) => panic!("opened: {toml}"),
        Err(e) => e,
    }
}

const MONGO: &str = "dbento = 1\n[connections.docs]\nengine = \"mongodb\"\n";
const DYNAMO: &str =
    "dbento = 1\n[connections.items]\nengine = \"dynamodb\"\nregion = \"eu-north-1\"\n";

#[test]
fn a_secret_in_a_mongodb_uri_is_a_secret_error_that_never_quotes_it() {
    for (uri, want) in [
        (
            "mongodb://app:hunter2@h/shop",
            "remove the password from `uri`; Dbento keeps it in the keychain. If it holds none, write any `@` after the host as `%40`",
        ),
        (
            "mongodb://h/?proxyPassword=hunter2",
            "remove `proxyPassword` from `uri`: it holds a secret",
        ),
        (
            "mongodb://h/?tlsCertificateKeyFilePassword=hunter2",
            "remove `tlsCertificateKeyFilePassword` from `uri`: it holds a secret",
        ),
        (
            "mongodb://h/?authMechanismProperties=AWS_SESSION_TOKEN:hunter2",
            "remove `authMechanismProperties` from `uri`: it holds a secret",
        ),
    ] {
        let e = open_err(&format!("{MONGO}uri = \"{uri}\"\n"));
        let OpenError::Secret {
            connection,
            key,
            message,
        } = &e
        else {
            panic!("{uri}: {e:?}");
        };
        assert_eq!(
            (connection.as_str(), key.as_str(), message.as_str()),
            ("docs", "uri", want),
            "{uri}"
        );
        assert_eq!(e.code(), "secret");
        assert_eq!(e.to_string(), format!("connection `docs`: {want}"));
        assert!(!format!("{e:?}").contains("hunter2"), "{uri}");
    }
}

#[test]
fn a_bad_scheme_or_escape_in_a_mongodb_uri_stays_invalid() {
    for uri in ["postgres://h/x", "mongodb://h/%FF", "mongodb://u:x/"] {
        let e = open_err(&format!("{MONGO}uri = \"{uri}\"\n"));
        assert!(matches!(e, OpenError::Invalid { .. }), "{uri}: {e:?}");
        assert_eq!(e.code(), "invalid");
    }
}

#[test]
fn userinfo_in_an_endpoint_is_a_secret_error_and_every_other_fault_stays_invalid() {
    let words = "`endpoint` must not hold a user, a path, a query or a fragment";
    for endpoint in [
        "https://u:hunter2@h",
        "https://u@h",
        "http://u:hunter2@localhost:8000",
        "https://u:hunter2@h/path",
    ] {
        let e = open_err(&format!("{DYNAMO}endpoint = \"{endpoint}\"\n"));
        let OpenError::Secret {
            connection,
            key,
            message,
        } = &e
        else {
            panic!("{endpoint}: {e:?}");
        };
        assert_eq!(
            (connection.as_str(), key.as_str(), message.as_str()),
            ("items", "endpoint", words)
        );
        assert!(!format!("{e:?}").contains("hunter2"), "{endpoint}");
    }
    // Parse and scheme come before userinfo, so these are not secrets.
    for endpoint in [
        "ftp://u:hunter2@h",
        "not a url",
        "https://h/path",
        "https://h?x=1",
        "https://h#f",
        "http://db.example.com",
    ] {
        let e = open_err(&format!("{DYNAMO}endpoint = \"{endpoint}\"\n"));
        let OpenError::Invalid {
            connection,
            message,
        } = &e
        else {
            panic!("{endpoint}: {e:?}");
        };
        assert_eq!(connection, "items");
        assert!(
            message.contains("`endpoint`") && !message.contains("hunter2"),
            "{endpoint}: {message}"
        );
    }
}
