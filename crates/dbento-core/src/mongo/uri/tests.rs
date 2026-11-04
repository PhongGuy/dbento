use super::{PASSWORD_REFUSED, check};

fn refused(uri: &str) -> String {
    check(uri).err().unwrap_or_else(|| panic!("{uri} accepted"))
}

#[test]
fn a_password_is_refused_without_the_driver() {
    for uri in [
        "mongodb://app:hunter2@h/shop",
        "mongodb://app:hunter2@h1,h2/?replicaSet=rs",
        "mongodb+srv://app:hunter2@cluster0.example.net/shop",
        "mongodb://app:@h/",
        "mongodb://app:hunter2@ss@h/",
        "mongodb://u:hunter2/ss@h/",
        // The driver would read `w@h` as options here; the userinfo rule runs first.
        "mongodb://u:hunter2?w@h",
        // An `@` in options with no `/`: a refused false positive (fail-safe).
        "mongodb://h:27017?appName=a@b",
    ] {
        assert_eq!(refused(uri), PASSWORD_REFUSED, "{uri}");
    }
}

#[test]
fn the_password_refusal_says_how_to_keep_an_at_in_options() {
    assert_eq!(
        PASSWORD_REFUSED,
        "remove the password from `uri`; Dbento keeps it in the keychain. If it holds none, write any `@` after the host as `%40`"
    );
}

#[test]
fn a_secret_option_is_refused_without_the_driver_and_is_never_quoted() {
    let tls = "remove `tlsCertificateKeyFilePassword` from `uri`: it holds a secret";
    let proxy = "remove `proxyPassword` from `uri`: it holds a secret";
    let aws = "remove `authMechanismProperties` from `uri`: it holds a secret";
    for (uri, want) in [
        ("mongodb://h/?tlsCertificateKeyFilePassword=hunter2", tls),
        ("mongodb://h/?TLSCERTIFICATEKEYFILEPASSWORD=hunter2", tls),
        // The driver lower-cases with Unicode rules, so a Kelvin sign reads as `k`.
        (
            "mongodb://h/?tlsCertificate\u{212A}eyFilePassword=hunter2",
            tls,
        ),
        ("mongodb://h?proxyPassword=hunter2", proxy),
        ("mongodb://u@h/d?appName=x&proxypassword=hunter2", proxy),
        (
            "mongodb://h/?authMechanismProperties=AWS_SESSION_TOKEN:hunter2",
            aws,
        ),
        (
            "mongodb://h/?authmechanismproperties=AWS%5FSESSION%5FTOKEN:hunter2",
            aws,
        ),
        (
            "mongodb://h/?authMechanismProperties=SERVICE_NAME:a,AWS_SESSION_TOKEN:hunter2",
            aws,
        ),
    ] {
        assert_eq!(refused(uri), want, "{uri}");
    }
}

#[test]
fn a_harmless_option_is_not_taken_for_a_secret() {
    for uri in [
        "mongodb://h/?authMechanismProperties=SERVICE_NAME:mongodb",
        "mongodb://h/?appName=proxyPassword",
    ] {
        check(uri).unwrap_or_else(|e| panic!("{uri}: {e}"));
    }
}

#[test]
fn a_password_holding_a_slash_and_a_question_mark_is_refused_without_the_driver() {
    for uri in [
        "mongodb://u:hunter?2/x@h",
        "mongodb://u:hunter/2?x@h",
        // Reads as a host and port to the driver, so only this rule can catch it.
        "mongodb://u:1234?appName=b/c@h",
        "mongodb://u:1234/b?appName=c@h",
    ] {
        assert_eq!(refused(uri), PASSWORD_REFUSED, "{uri}");
    }
}

#[test]
fn a_port_before_an_at_in_options_is_refused_until_it_is_escaped() {
    assert_eq!(
        refused("mongodb://u@h:27017/d?appName=a@b"),
        PASSWORD_REFUSED
    );
    let info = check("mongodb://u@h:27017/d?appName=a%40b").unwrap();
    assert_eq!(
        (info.user.as_deref(), info.database.as_str()),
        (Some("u"), "d")
    );
}

#[test]
fn user_and_database_come_from_the_uri_percent_decoded() {
    for (uri, user, database) in [
        (
            "mongodb://app@db.internal:27017/shop?authSource=admin",
            Some("app"),
            "shop",
        ),
        ("mongodb://localhost", None, "test"),
        (
            "mongodb://app%40corp@h1,h2/?replicaSet=rs",
            Some("app@corp"),
            "test",
        ),
        ("mongodb://h/my%2Ddb", None, "my-db"),
        (
            "mongodb+srv://app@cluster0.example.net/",
            Some("app"),
            "test",
        ),
        ("mongodb://u@h/d?appName=a@b", Some("u"), "d"),
        ("mongodb://[::1]:27017/", None, "test"),
        ("mongodb://u%3Ap@h", Some("u:p"), "test"),
    ] {
        let info = check(uri).unwrap_or_else(|e| panic!("{uri}: {e}"));
        assert_eq!(
            (info.user.as_deref(), info.database.as_str()),
            (user, database),
            "{uri}"
        );
    }
}

#[test]
fn a_bad_scheme_or_escape_is_refused() {
    let scheme = "`uri` must start with mongodb:// or mongodb+srv://";
    let utf8 = "`uri` holds percent-encoding that is not UTF-8";
    for (uri, want) in [
        ("postgres://h/x", scheme),
        ("h:27017", scheme),
        ("mongodb://%FF@h/", utf8),
        ("mongodb://h/%FF", utf8),
    ] {
        assert_eq!(refused(uri), want, "{uri}");
    }
}

#[cfg(feature = "mongodb")]
#[test]
fn the_driver_checks_the_syntax_and_agrees_on_user_and_database() {
    use mongodb::options::ConnectionString;
    let m = refused("mongodb://h:x/d");
    assert!(
        m.starts_with("`uri` is not a MongoDB connection string: ")
            && m.contains("the port must be an integer"),
        "{m}"
    );
    assert_eq!(
        refused("mongodb://u@h:x/d"),
        "`uri` is not a MongoDB connection string (the driver's reason is hidden because the text may hold a password)"
    );
    for uri in [
        "mongodb://app@db.internal:27017/shop?authSource=admin",
        "mongodb://localhost",
        "mongodb://app%40corp@h1,h2/?replicaSet=rs",
        "mongodb://h/my%2Ddb",
        "mongodb+srv://app@cluster0.example.net/",
        "mongodb://u@h/d?appName=a@b",
        "mongodb://[::1]:27017/",
        "mongodb://u%3Ap@h",
    ] {
        let info = check(uri).unwrap();
        let d = ConnectionString::parse(uri).unwrap();
        let user = d.credential.and_then(|c| c.username);
        let database = d.default_database.unwrap_or_else(|| "test".to_owned());
        assert_eq!((info.user, info.database), (user, database), "{uri}");
    }
}

#[cfg(feature = "mongodb")]
#[test]
fn a_driver_reason_that_can_quote_the_text_stays_hidden() {
    let hidden = "`uri` is not a MongoDB connection string (the driver's reason is hidden because the text may hold a password)";
    let port =
        "`uri` is not a MongoDB connection string: the port must be an integer between 1 and 65535";
    for (uri, want) in [
        ("mongodb://app:hunter2/shop", port),
        ("mongodb://app:hunter2%40h/", port),
        ("mongodb://[hunter2/", hidden),
        ("mongodb://h/?hunter2", hidden),
        ("mongodb://h/?readPreference=hunter2", hidden),
    ] {
        assert_eq!(refused(uri), want, "{uri}");
    }
}
