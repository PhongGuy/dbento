//! The shared pgjdbc parser and Postgres draft builder, on neutral inputs.
use super::*;
use crate::TlsMode;
use crate::import::model::{SecretSetting, SkipReason, SourceTls, WarningKind};

fn input<'a>(host: Option<&str>) -> PgInput<'a> {
    PgInput {
        host: host.map(str::to_owned),
        port: None,
        database: "shop".to_owned(),
        params: Vec::new(),
        user: Some("clerk"),
        default_user: None,
        tls_first: None,
        tls_fallback: None,
        tls_ca: None,
        tls_cert: None,
        tls_key: None,
        secret: SecretSetting::Keychain,
    }
}

fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
    p.iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn urls_split_into_host_port_database_and_decoded_parameters() {
    let u = parse_url("jdbc:postgresql://db.example:6543/my+shop?currentSchema=a%2Cb&ssl").unwrap();
    assert_eq!(
        (u.host.as_deref(), u.port, u.database.as_str()),
        (Some("db.example"), Some(6543), "my shop")
    );
    assert_eq!(u.params, pairs(&[("currentSchema", "a,b"), ("ssl", "")]));
    let bare = parse_url("postgresql:shop").unwrap();
    assert_eq!(
        (bare.host, bare.port, bare.database.as_str()),
        (None, None, "shop")
    );
    let v6 = parse_url("jdbc:postgresql://[::1]:5433/x").unwrap();
    assert_eq!((v6.host.as_deref(), v6.port), (Some("::1"), Some(5433)));
}

#[test]
fn unusable_urls_are_skipped_with_a_reason() {
    for (url, reason) in [
        ("jdbc:mysql://h/d", SkipReason::InvalidUrl),
        ("jdbc:postgresql://u:fake-pw-at@h/d", SkipReason::InvalidUrl),
        ("jdbc:postgresql://a,b/d", SkipReason::MultiHost),
        ("jdbc:postgresql://h:0/d", SkipReason::InvalidUrl),
        ("jdbc:postgresql://h:99999/d", SkipReason::InvalidUrl),
        ("jdbc:postgresql://h/d/e", SkipReason::InvalidUrl),
        ("jdbc:postgresql://h%40x/d", SkipReason::InvalidUrl),
    ] {
        assert_eq!(parse_url(url).err(), Some(reason), "{url}");
    }
}

#[test]
fn ports_are_u16_above_zero_and_blank_means_default() {
    assert_eq!(parse_port(""), Ok(None));
    assert_eq!(parse_port("6543"), Ok(Some(6543)));
    for bad in ["0", "65536", "54x", "-1", " 5432"] {
        assert_eq!(parse_port(bad), Err(SkipReason::InvalidUrl), "{bad}");
    }
}

#[test]
fn the_first_value_of_a_name_wins_and_each_name_warns_once() {
    let mut i = input(Some("db.example"));
    i.params = pairs(&[
        ("password", "fake-pw-a"),
        ("loggerLevel", "OFF"),
        ("currentSchema", "app"),
        ("password", "fake-pw-b"),
        ("currentSchema", "other"),
        ("ApplicationName", "dbento"),
        ("loggerLevel", "TRACE"),
    ]);
    let (pg, warnings) = build(i).unwrap();
    assert_eq!(
        warnings,
        [
            WarningKind::PasswordInUrl,
            WarningKind::DroppedParameter("loggerLevel".into()),
            WarningKind::TlsStricter {
                from: SourceTls::Prefer
            }
        ]
    );
    assert_eq!(
        pg.settings.get("search_path").map(String::as_str),
        Some("app")
    );
    assert_eq!(
        pg.settings.get("application_name").map(String::as_str),
        Some("dbento")
    );
    assert!(!format!("{pg:?}").contains("fake-pw"));
}

#[test]
fn the_user_comes_from_the_source_then_the_parameters_then_the_default() {
    let mut i = input(None);
    i.user = None;
    i.params = pairs(&[("user", "url_user")]);
    i.database = String::new();
    let (pg, _) = build(i).unwrap();
    assert_eq!(
        (pg.host.as_str(), pg.user.as_str(), pg.database.as_str()),
        ("localhost", "url_user", "url_user")
    );
    let mut i = input(None);
    i.user = Some("  ");
    i.default_user = Some("fallback");
    assert_eq!(build(i).unwrap().0.user, "fallback");
    let mut i = input(None);
    i.user = None;
    assert_eq!(build(i).err(), Some(SkipReason::NoUser));
    let mut i = input(None);
    i.params = pairs(&[("service", "billing")]);
    assert_eq!(build(i).err(), Some(SkipReason::ServiceFile));
}

#[test]
fn tls_first_beats_the_parameters_which_beat_the_fallback() {
    let tls = |first: Option<SourceTls>, params: &[(&str, &str)], fallback: Option<SourceTls>| {
        let mut i = input(Some("db.example"));
        i.tls_first = first;
        i.params = pairs(params);
        i.tls_fallback = fallback;
        let (pg, w) = build(i).unwrap();
        (pg.tls, w.last().cloned())
    };
    let (verify, off) = (Some(TlsMode::Verify), Some(TlsMode::Off));
    let stricter = |from| Some(WarningKind::TlsStricter { from });
    assert_eq!(
        tls(Some(SourceTls::VerifyFull), &[("sslmode", "disable")], None),
        (verify, None)
    );
    assert_eq!(
        tls(None, &[("sslmode", "disable")], Some(SourceTls::Require)),
        (off, None)
    );
    assert_eq!(
        tls(None, &[("ssl", "TRUE")], Some(SourceTls::Disable)),
        (verify, None)
    );
    assert_eq!(
        tls(None, &[("ssl", "false")], Some(SourceTls::Disable)),
        (off, None)
    );
    assert_eq!(
        tls(None, &[], Some(SourceTls::Require)),
        (verify, stricter(SourceTls::Require))
    );
    assert_eq!(tls(None, &[], None), (verify, stricter(SourceTls::Prefer)));
    let (pg, w) = build(input(Some("localhost"))).unwrap();
    assert_eq!(
        (pg.tls, w),
        (
            off,
            vec![WarningKind::TlsWeaker {
                from: SourceTls::Prefer
            }]
        )
    );
    assert_eq!(parse_mode(" VERIFY_CA "), SourceTls::VerifyCa);
    assert_eq!(parse_mode("bogus"), SourceTls::Unrecognised);
}

#[test]
fn certificate_paths_are_kept_only_under_verify_when_absolute_and_present() {
    let dir = tempfile::TempDir::new().unwrap();
    let file = |n: &str| {
        let p = dir.path().join(n);
        std::fs::write(&p, "x").unwrap();
        p.to_string_lossy().into_owned()
    };
    let (ca, cert, key) = (file("ca.pem"), file("c.pem"), file("k.pem"));
    let mut i = input(Some("db.example"));
    i.tls_first = Some(SourceTls::VerifyFull);
    (i.tls_ca, i.tls_cert, i.tls_key) =
        (Some(ca.as_str()), Some(cert.as_str()), Some(key.as_str()));
    let (pg, w) = build(i).unwrap();
    assert_eq!(
        (
            pg.tls_ca.as_deref(),
            pg.tls_cert.as_deref(),
            pg.tls_key.as_deref()
        ),
        (Some(ca.as_str()), Some(cert.as_str()), Some(key.as_str()))
    );
    assert!(w.is_empty(), "{w:?}");

    let mut i = input(Some("db.example"));
    i.params = pairs(&[("sslmode", "disable")]);
    (i.tls_ca, i.tls_cert, i.tls_key) =
        (Some(ca.as_str()), Some(cert.as_str()), Some(key.as_str()));
    let (pg, w) = build(i).unwrap();
    assert_eq!((pg.tls_ca, pg.tls_cert, pg.tls_key), (None, None, None));
    assert_eq!(
        w,
        [WarningKind::CaCertMissing, WarningKind::ClientCertMissing]
    );

    let mut i = input(Some("db.example"));
    i.tls_first = Some(SourceTls::VerifyFull);
    (i.tls_ca, i.tls_cert, i.tls_key) = (Some("relative/ca.pem"), Some(cert.as_str()), Some("  "));
    let (pg, w) = build(i).unwrap();
    assert_eq!((pg.tls_ca, pg.tls_cert), (None, None));
    assert_eq!(
        w,
        [WarningKind::CaCertMissing, WarningKind::ClientCertMissing]
    );
}
