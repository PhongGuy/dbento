//! The MySQL and MariaDB URL parser and draft builder, on neutral inputs.
use super::*;
use crate::TlsMode;
use crate::import::model::{SecretSetting, SkipReason, SourceTls, WarningKind};

fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
    p.iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn input<'a>(host: Option<&str>, driver: Driver) -> MysqlInput<'a> {
    MysqlInput {
        host: host.map(str::to_owned),
        port: None,
        database: None,
        params: Vec::new(),
        user: Some("clerk"),
        default_user: None,
        tls_first: None,
        tls_fallback: None,
        driver,
        tls_ca: None,
        tls_cert: None,
        tls_key: None,
        secret: SecretSetting::Keychain,
    }
}

#[test]
fn urls_split_into_host_port_database_and_parameters() {
    let u = parse_url("jdbc:mysql://db.example:3307/shop?connectTimeout=5&sslMode=VERIFY_IDENTITY")
        .unwrap();
    assert_eq!(
        (u.host.as_deref(), u.port, u.database.as_deref()),
        (Some("db.example"), Some(3307), Some("shop"))
    );
    assert_eq!(
        u.params,
        pairs(&[("connectTimeout", "5"), ("sslMode", "VERIFY_IDENTITY")])
    );
    let m = parse_url("jdbc:mariadb://localhost/my%20db").unwrap();
    assert_eq!(
        (m.host.as_deref(), m.port, m.database.as_deref()),
        (Some("localhost"), None, Some("my db"))
    );
    let bare = parse_url("mysql://").unwrap();
    assert_eq!((bare.host, bare.port, bare.database), (None, None, None));
    let v6 = parse_url("jdbc:mysql://[::1]:3307").unwrap();
    assert_eq!(
        (v6.host.as_deref(), v6.port, v6.database),
        (Some("::1"), Some(3307), None)
    );
    assert_eq!(parse_url("jdbc:mysql://h/").unwrap().database, None);
    let q = parse_url("jdbc:mysql://h?sslMode=REQUIRED").unwrap();
    assert_eq!(
        (q.host.as_deref(), q.database, q.params),
        (Some("h"), None, pairs(&[("sslMode", "REQUIRED")]))
    );
}

#[test]
fn userinfo_gives_the_user_and_flags_a_password_without_keeping_it() {
    let u = parse_url("jdbc:mysql://ann:fake-pw-ui@db.example/shop").unwrap();
    assert_eq!(u.host.as_deref(), Some("db.example"));
    assert_eq!(u.params, pairs(&[("user", "ann"), ("password", "")]));
    assert!(!format!("{u:?}").contains("fake-pw"));
    let only_user = parse_url("jdbc:mysql://ann@db.example").unwrap();
    assert_eq!(only_user.params, pairs(&[("user", "ann")]));
    let v6 = parse_url("jdbc:mysql://ann@[::1]:3307/d").unwrap();
    assert_eq!((v6.host.as_deref(), v6.port), (Some("::1"), Some(3307)));
}

#[test]
fn unusable_urls_are_skipped_with_a_reason() {
    use SkipReason::{InvalidUrl, MultiHost};
    for (url, reason) in [
        ("jdbc:postgresql://h/d", InvalidUrl),
        ("jdbc:mysql:h/d", InvalidUrl),
        ("jdbc:mysqlx://h/d", InvalidUrl),
        ("jdbc:mysql:aws://h:3306/d", InvalidUrl),
        ("jdbc:mysql://address=(host=h)(port=3306)/d", InvalidUrl),
        ("jdbc:mysql://(host=h,port=3306)/d", InvalidUrl),
        ("jdbc:mysql://h/a/b", InvalidUrl),
        ("jdbc:mysql://h:0/d", InvalidUrl),
        ("jdbc:mysql://h:99999/d", InvalidUrl),
        ("jdbc:mysql://h%41/d", InvalidUrl),
        ("jdbc:mysql://a:3306,b:3306/d", MultiHost),
        ("jdbc:mysql://u:p@[a:1,b:2]/d", MultiHost),
        ("jdbc:mysql:loadbalance://a,b/d", MultiHost),
        ("jdbc:mysql:replication://a,b/d", MultiHost),
        ("jdbc:mysql+srv://_mysql._tcp.example/d", MultiHost),
        ("jdbc:mariadb:sequential://a,b/d", MultiHost),
        ("jdbc:mariadb:load-balance-read://a/d", MultiHost),
    ] {
        assert_eq!(parse_url(url), Err(reason), "{url}");
    }
}

#[test]
fn mysql_and_mariadb_ssl_modes_read_in_any_case() {
    use SourceTls::*;
    for (v, want) in [
        ("DISABLED", Disable),
        ("disable", Disable),
        ("0", Disable),
        ("false", Disable),
        ("PREFERRED", Prefer),
        ("allow", Allow),
        ("REQUIRED", Require),
        ("trust", Require),
        ("REQUIRE", Require),
        ("VERIFY_CA", VerifyCa),
        ("verify-ca", VerifyCa),
        ("VERIFY_IDENTITY", VerifyFull),
        ("verify-full", VerifyFull),
        ("VERIFY_FULL", VerifyFull),
        ("1", VerifyFull),
        ("TRUE", VerifyFull),
        (" Required ", Require),
        ("sometimes", Unrecognised),
    ] {
        assert_eq!(parse_mode(v), want, "{v}");
    }
}

#[test]
fn the_draft_takes_the_host_port_database_user_and_secret() {
    let (d, w) = build(MysqlInput {
        port: Some(3307),
        database: Some("shop".into()),
        secret: SecretSetting::None,
        ..input(Some("db.example"), Driver::Mysql)
    })
    .unwrap();
    assert_eq!(
        (
            d.host.as_str(),
            d.port,
            d.database.as_deref(),
            d.user.as_str(),
            d.secret
        ),
        (
            "db.example",
            Some(3307),
            Some("shop"),
            "clerk",
            SecretSetting::None
        )
    );
    assert_eq!(
        w,
        [WarningKind::TlsStricter {
            from: SourceTls::Prefer
        }]
    );
    let (local, _) = build(MysqlInput {
        database: Some(" ".into()),
        ..input(None, Driver::Mysql)
    })
    .unwrap();
    assert_eq!(
        (local.host.as_str(), local.database, local.tls),
        ("localhost", None, Some(TlsMode::Off))
    );
}

#[test]
fn the_user_comes_from_the_source_then_the_parameters_then_the_default() {
    let with =
        |user: Option<&'static str>, params: &[(&str, &str)], default: Option<&'static str>| {
            build(MysqlInput {
                user,
                params: pairs(params),
                default_user: default,
                ..input(Some("h"), Driver::Mysql)
            })
            .map(|(d, _)| d.user)
        };
    assert_eq!(
        with(Some("saved"), &[("user", "url")], Some("dflt")),
        Ok("saved".to_owned())
    );
    assert_eq!(
        with(Some("  "), &[("user", "url")], Some("dflt")),
        Ok("url".to_owned())
    );
    assert_eq!(with(None, &[], Some("dflt")), Ok("dflt".to_owned()));
    assert_eq!(with(None, &[("user", "")], None), Err(SkipReason::NoUser));
}

#[test]
fn parameters_warn_once_by_name_and_a_password_never_lands() {
    let (_, w) = build(MysqlInput {
        params: pairs(&[
            ("password", "fake-pw"),
            ("connectTimeout", "5"),
            ("connectTimeout", "6"),
            ("useSSL", "false"),
            ("password", "x"),
            ("sslmode", "disabled"),
        ]),
        ..input(Some("db.example"), Driver::Mysql)
    })
    .unwrap();
    assert_eq!(
        w,
        [
            WarningKind::PasswordInUrl,
            WarningKind::DroppedParameter("connectTimeout".into())
        ]
    );
}

#[test]
fn a_socket_host_or_socket_parameter_is_a_unix_socket() {
    let sock = build(input(Some("/var/run/mysqld/mysqld.sock"), Driver::Mysql));
    assert_eq!(sock.map(|_| ()), Err(SkipReason::UnixSocket));
    let by_param = MysqlInput {
        params: pairs(&[("socket", "/tmp/mysql.sock")]),
        ..input(Some("localhost"), Driver::Mariadb)
    };
    assert_eq!(build(by_param).map(|_| ()), Err(SkipReason::UnixSocket));
}

#[test]
fn tls_first_then_ssl_mode_then_legacy_then_fallback_then_the_driver_default() {
    use Driver::{Mariadb, Mysql};
    use SourceTls::*;
    let tls = |driver: Driver,
               first: Option<SourceTls>,
               params: &[(&str, &str)],
               fallback: Option<SourceTls>| {
        let (d, w) = build(MysqlInput {
            tls_first: first,
            tls_fallback: fallback,
            params: pairs(params),
            ..input(Some("db.example"), driver)
        })
        .unwrap();
        let from = w.iter().find_map(|w| match w {
            WarningKind::TlsStricter { from } | WarningKind::TlsWeaker { from } => Some(*from),
            WarningKind::PasswordInUrl
            | WarningKind::DroppedParameter(_)
            | WarningKind::CaCertMissing
            | WarningKind::ClientCertMissing
            | WarningKind::MachineSpecificPath
            | WarningKind::FileMissing
            | WarningKind::NotUtf8
            | WarningKind::ColorsNotImported
            | WarningKind::BoundByFolder
            | WarningKind::AuthModel(_)
            | WarningKind::KeyMissing
            | WarningKind::SecretUnreadable
            | WarningKind::ColorUnknown(_) => None,
        });
        (d.tls, from)
    };
    let (on, off) = (Some(TlsMode::Verify), Some(TlsMode::Off));
    // With nothing set Connector/J prefers TLS and MariaDB 3.x disables it.
    assert_eq!(tls(Mysql, None, &[], None), (on, Some(Prefer)));
    assert_eq!(tls(Mariadb, None, &[], None), (off, None));
    assert_eq!(
        tls(
            Mysql,
            Some(Disable),
            &[("sslMode", "VERIFY_IDENTITY")],
            None
        ),
        (off, None)
    );
    assert_eq!(
        tls(
            Mysql,
            None,
            &[("sslMode", "DISABLED"), ("useSSL", "true")],
            Some(VerifyFull)
        ),
        (off, None)
    );
    assert_eq!(
        tls(Mariadb, None, &[("sslMode", "REQUIRED")], None),
        (on, Some(Require))
    );
    assert_eq!(
        tls(Mysql, None, &[("useSSL", "false")], Some(VerifyFull)),
        (off, None)
    );
    assert_eq!(
        tls(Mysql, None, &[("useSSL", "true")], None),
        (on, Some(Prefer))
    );
    assert_eq!(
        tls(
            Mysql,
            None,
            &[("useSSL", "true"), ("requireSSL", "true")],
            None
        ),
        (on, Some(Require))
    );
    assert_eq!(
        tls(
            Mysql,
            None,
            &[("useSSL", "true"), ("verifyServerCertificate", "true")],
            None
        ),
        (on, Some(VerifyCa))
    );
    assert_eq!(tls(Mariadb, None, &[("useSsl", "")], None), (on, None));
    assert_eq!(
        tls(
            Mariadb,
            None,
            &[("useSSL", "1"), ("trustServerCertificate", "true")],
            None
        ),
        (on, Some(Require))
    );
    assert_eq!(
        tls(
            Mariadb,
            None,
            &[
                ("useSSL", "true"),
                ("disableSslHostnameVerification", "true")
            ],
            None
        ),
        (on, Some(VerifyCa))
    );
    assert_eq!(tls(Mariadb, None, &[], Some(VerifyFull)), (on, None));
    assert_eq!(
        tls(Mysql, None, &[("connectTimeout", "5")], Some(Disable)),
        (off, None)
    );
}

#[test]
fn certificate_paths_go_through_the_shared_tls_rule() {
    let dir = tempfile::TempDir::new().unwrap();
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, "x").unwrap();
    let ca = ca.to_str().unwrap();
    let (d, w) = build(MysqlInput {
        tls_first: Some(SourceTls::VerifyFull),
        tls_ca: Some(ca),
        tls_cert: Some("/nonexistent/c.pem"),
        tls_key: Some("/nonexistent/c.key"),
        ..input(Some("db.example"), Driver::Mysql)
    })
    .unwrap();
    assert_eq!(
        (d.tls_ca.as_deref(), d.tls_cert, d.tls_key),
        (Some(ca), None, None)
    );
    assert_eq!(w, [WarningKind::ClientCertMissing]);
}

#[test]
fn mariadb_local_socket_and_pipe_parameters_are_unix_sockets() {
    for (name, value) in [
        ("localSocket", "/tmp/mysql.sock"),
        ("PIPE", "MySQL"),
        ("localsocket", "x"),
    ] {
        let by_param = MysqlInput {
            params: pairs(&[(name, value)]),
            ..input(Some("db.example"), Driver::Mariadb)
        };
        assert_eq!(
            build(by_param).map(|_| ()),
            Err(SkipReason::UnixSocket),
            "{name}"
        );
    }
    let blank = MysqlInput {
        params: pairs(&[("localSocket", " "), ("pipe", "")]),
        ..input(Some("db.example"), Driver::Mariadb)
    };
    assert!(build(blank).is_ok());
}

#[test]
fn parameter_names_that_differ_only_in_case_warn_once() {
    let (_, w) = build(MysqlInput {
        params: pairs(&[
            ("password", "fake-pw"),
            ("Password", "x"),
            ("connectTimeout", "5"),
            ("CONNECTTIMEOUT", "6"),
        ]),
        ..input(Some("db.example"), Driver::Mysql)
    })
    .unwrap();
    assert_eq!(
        w,
        [
            WarningKind::PasswordInUrl,
            WarningKind::DroppedParameter("connectTimeout".into()),
            WarningKind::TlsStricter {
                from: SourceTls::Prefer
            }
        ]
    );
}
