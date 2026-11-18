use super::{BAD_PORT, PASSWORD_REFUSED, UriFault, check};

fn refused(uri: &str) -> String {
    match check(uri) {
        Ok(_) => panic!("{uri} accepted"),
        Err(UriFault::Secret(m) | UriFault::Invalid(m)) => m,
    }
}

#[test]
fn each_uri_fault_says_whether_it_is_a_secret() {
    for uri in ["mongodb://app:x@h/", "mongodb://h/?proxyPassword=x"] {
        assert!(matches!(check(uri), Err(UriFault::Secret(_))), "{uri}");
    }
    for uri in ["postgres://h", "mongodb://h/%FF", "mongodb://u:x/"] {
        assert!(matches!(check(uri), Err(UriFault::Invalid(_))), "{uri}");
    }
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
        check(uri).unwrap_or_else(|e| panic!("{uri}: {e:?}"));
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
        let info = check(uri).unwrap_or_else(|e| panic!("{uri}: {e:?}"));
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

#[test]
fn a_uri_the_driver_alone_would_refuse_passes_the_pure_check() {
    // The driver's syntax check runs at connect and on save, so open accepts this text.
    let info = check("mongodb://h/?readPreference=secret").unwrap();
    assert_eq!((info.user, info.database.as_str()), (None, "test"));
}

#[test]
fn a_port_outside_1_to_65535_is_refused_without_quoting_the_uri() {
    for uri in [
        "mongodb://u:secret/",
        "mongodb://u:secret",
        "mongodb://h:0",
        "mongodb://h:65536",
        "mongodb://h:",
        "mongodb://h:+27017",
        "mongodb://a:27017,b:x/db",
        "mongodb://[::1]:99999/",
        "mongodb://app@h:secret/d?authSource=admin",
        "mongodb+srv://cluster0.example.net:secret/",
    ] {
        let m = refused(uri);
        assert_eq!(m, BAD_PORT, "{uri}");
        assert!(!m.contains("secret") && !m.contains("99999"), "{uri}: {m}");
    }
}

#[test]
fn ports_in_range_bracketed_ipv6_and_host_lists_are_accepted() {
    for uri in [
        "mongodb://h:1",
        "mongodb://h:65535",
        "mongodb://a:27017,b:27018/db",
        "mongodb://a,b:27018,c/db?replicaSet=rs",
        "mongodb://[::1]:27017/",
        "mongodb://[::1]/",
        "mongodb://[::1]:27017,[::2]:27018/db",
        "mongodb+srv://cluster0.example.net/",
        "mongodb://u@h:27017/d?appName=a%40b",
    ] {
        check(uri).unwrap_or_else(|e| panic!("{uri}: {e:?}"));
    }
}
