//! Data source to draft: engine, URL, user, parameters, TLS, SQLite paths and names.
use super::*;
use crate::import::datagrip::xml::{RawLocal, RawSource, RawSsl};
use crate::import::model::{SecretSetting, SkipReason, SourceTls, WarningKind};
use crate::{Engine, TlsMode};
use std::path::Path;

fn src(driver: &str, url: &str) -> RawSource {
    RawSource {
        uuid: "u".into(),
        name: "Src".into(),
        driver_ref: Some(driver.into()),
        url: Some(url.into()),
        ..Default::default()
    }
}
fn user(name: &str) -> RawLocal {
    RawLocal {
        user_name: Some(name.into()),
        ..Default::default()
    }
}
fn ctx() -> MapContext<'static> {
    MapContext {
        root: Path::new("/proj"),
        home: Some(Path::new("/home/u")),
        default_user: None,
    }
}
fn ok(s: &RawSource, local: Option<&RawLocal>) -> Mapped {
    map_source(s, local, &ctx()).unwrap_or_else(|r| panic!("{r:?}"))
}
fn skip(s: &RawSource, local: Option<&RawLocal>) -> SkipReason {
    map_source(s, local, &ctx()).expect_err("skipped")
}
fn tls_warning(m: &Mapped) -> Option<WarningKind> {
    m.warnings
        .iter()
        .find(|w| {
            matches!(
                w,
                WarningKind::TlsStricter { .. } | WarningKind::TlsWeaker { .. }
            )
        })
        .cloned()
}

#[test]
fn engine_from_driver_ref_then_url_then_driver_class() {
    let raw = |d: Option<&str>, j: Option<&str>, u: &str| RawSource {
        driver_ref: d.map(str::to_owned),
        jdbc_driver: j.map(str::to_owned),
        url: Some(u.to_owned()),
        ..Default::default()
    };
    let (pg, lite) = (Ok(Engine::Postgres), Ok(Engine::Sqlite));
    let cases = [
        (
            raw(Some("postgresql"), None, "jdbc:postgresql://h/d"),
            pg.clone(),
        ),
        (
            raw(Some("postgres"), None, "jdbc:postgresql://h/d"),
            pg.clone(),
        ),
        (
            raw(Some("sqlite.xerial"), None, "jdbc:sqlite:/x.db"),
            lite.clone(),
        ),
        (raw(Some("sqlite"), None, "jdbc:sqlite:/x.db"), lite.clone()),
        (raw(Some("custom"), None, "postgresql://h/d"), pg.clone()),
        (raw(None, None, "jdbc:sqlite:x.db"), lite.clone()),
        (
            raw(None, Some("org.postgresql.Driver"), "jdbc:other:x"),
            pg.clone(),
        ),
        (
            raw(Some("mysql.8"), None, "jdbc:mysql://h/d"),
            Ok(Engine::Mysql),
        ),
        (
            raw(None, None, "mongodb://h"),
            Err(SkipReason::UnsupportedDriver("unknown".into())),
        ),
    ];
    for (s, want) in cases {
        assert_eq!(detect_engine(&s), want, "{:?}", s.driver_ref);
    }
}

#[test]
fn postgres_url_forms() {
    let cases = [
        (
            "jdbc:postgresql://db.example:6543/orders",
            "db.example",
            Some(6543),
            "orders",
        ),
        (
            "jdbc:postgresql://db.example:5432/orders",
            "db.example",
            None,
            "orders",
        ),
        ("jdbc:postgresql://db.example/", "db.example", None, "alice"),
        ("jdbc:postgresql://db.example", "db.example", None, "alice"),
        ("jdbc:postgresql://[::1]:5433/x", "::1", Some(5433), "x"),
        (
            "jdbc:postgresql://db.example/my%20db",
            "db.example",
            None,
            "my db",
        ),
        ("jdbc:postgresql:shop", "localhost", None, "shop"),
        ("jdbc:postgresql:", "localhost", None, "alice"),
        ("postgresql://h/d", "h", None, "d"),
    ];
    for (url, host, port, db) in cases {
        let m = ok(&src("postgresql", url), Some(&user("alice")));
        let d = &m.draft;
        assert_eq!(
            (d.host(), d.port(), d.database(), d.user()),
            (Some(host), port, Some(db), Some("alice")),
            "{url}"
        );
    }
}

#[test]
fn user_secret_and_read_only() {
    let s = src("postgresql", "jdbc:postgresql://h/d?user=from_url");
    assert_eq!(ok(&s, Some(&user("local"))).draft.user(), Some("local"));
    assert_eq!(ok(&s, None).draft.user(), Some("from_url"));
    let bare = src("postgresql", "jdbc:postgresql://h/d");
    let with_default = MapContext {
        default_user: Some("dflt"),
        ..ctx()
    };
    assert_eq!(
        map_source(&bare, None, &with_default).unwrap().draft.user(),
        Some("dflt")
    );
    assert_eq!(skip(&bare, None), SkipReason::NoUser);
    assert_eq!(
        ok(&bare, Some(&user("a"))).draft.secret(),
        Some(SecretSetting::Keychain)
    );
    let no_auth = RawLocal {
        auth_provider: Some("no-auth".into()),
        ..user("a")
    };
    let m = ok(
        &RawSource {
            read_only: true,
            ..bare
        },
        Some(&no_auth),
    );
    assert_eq!(
        (m.draft.secret(), m.draft.read_only()),
        (Some(SecretSetting::None), true)
    );
}

#[test]
fn unusable_postgres_sources_are_skipped_with_a_reason() {
    let u = user("a");
    let pg = |url: &str| src("postgresql", url);
    assert_eq!(
        skip(&pg("jdbc:postgresql://h1:5432,h2:5432/d"), Some(&u)),
        SkipReason::MultiHost
    );
    assert_eq!(
        skip(&pg("jdbc:postgresql://?service=prod"), Some(&u)),
        SkipReason::ServiceFile
    );
    assert_eq!(
        skip(&pg("jdbc:postgresql://h:99999/d"), Some(&u)),
        SkipReason::InvalidUrl
    );
    assert_eq!(
        skip(&pg("jdbc:postgresql://a:pw@h/d"), Some(&u)),
        SkipReason::InvalidUrl
    );
    assert_eq!(
        skip(&pg("jdbc:postgresql://h/d%ff"), Some(&u)),
        SkipReason::InvalidUrl
    );
    assert_eq!(
        skip(
            &RawSource {
                url: None,
                ..pg("")
            },
            Some(&u)
        ),
        SkipReason::MissingUrl
    );
    assert_eq!(skip(&pg("  "), Some(&u)), SkipReason::MissingUrl);
    let ssh = RawLocal {
        ssh_enabled: true,
        ..user("a")
    };
    assert_eq!(
        skip(&pg("jdbc:postgresql://h/d"), Some(&ssh)),
        SkipReason::SshTunnel
    );
}

#[test]
fn parameters_map_to_settings_and_the_rest_are_dropped_by_name() {
    let mut s = src(
        "postgresql",
        "jdbc:postgresql://h/d?currentSchema=sales&password=hunter2-url&loginTimeout=10&ApplicationName=url-app",
    );
    s.properties = vec![
        ("ApplicationName".into(), "prop-app".into()),
        ("password".into(), "hunter2-prop".into()),
        ("tcpKeepAlive".into(), "true".into()),
    ];
    let m = ok(&s, Some(&user("a")));
    let settings: Vec<(&str, &str)> = m
        .draft
        .settings()
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        settings,
        [("application_name", "url-app"), ("search_path", "sales")]
    );
    assert_eq!(
        m.warnings[..3],
        [
            WarningKind::PasswordInUrl,
            WarningKind::DroppedParameter("loginTimeout".into()),
            WarningKind::DroppedParameter("tcpKeepAlive".into())
        ]
    );
    let text = format!("{:?} {:?}", m.draft, m.warnings);
    assert!(!text.contains("hunter2"), "{text}");
}

#[test]
fn tls_maps_to_the_mode_at_least_as_strict_except_local_prefer() {
    use SourceTls::*;
    use WarningKind::{TlsStricter, TlsWeaker};
    let (on, off) = (Some(TlsMode::Verify), Some(TlsMode::Off));
    let remote = |q: &str| format!("jdbc:postgresql://db.example/d{q}");
    let cases = [
        (remote("?sslmode=disable"), off, None),
        (remote("?sslmode=verify-full"), on, None),
        (remote("?ssl=true"), on, None),
        (remote("?ssl="), on, None),
        (
            remote("?sslmode=verify-ca"),
            on,
            Some(TlsStricter { from: VerifyCa }),
        ),
        (
            remote("?sslmode=require"),
            on,
            Some(TlsStricter { from: Require }),
        ),
        (
            remote("?sslmode=REQUIRE"),
            on,
            Some(TlsStricter { from: Require }),
        ),
        (
            remote("?sslmode=allow"),
            on,
            Some(TlsStricter { from: Allow }),
        ),
        (remote(""), on, Some(TlsStricter { from: Prefer })),
        (remote("?ssl=false"), on, Some(TlsStricter { from: Prefer })),
        (
            remote("?sslmode=bogus"),
            on,
            Some(TlsStricter { from: Unrecognised }),
        ),
        (
            "jdbc:postgresql://localhost/d".to_owned(),
            off,
            Some(TlsWeaker { from: Prefer }),
        ),
        (
            "jdbc:postgresql://127.0.0.1/d?sslmode=allow".to_owned(),
            off,
            Some(TlsWeaker { from: Allow }),
        ),
        (
            "jdbc:postgresql://[::1]/d".to_owned(),
            off,
            Some(TlsWeaker { from: Prefer }),
        ),
        (
            "jdbc:postgresql:d".to_owned(),
            off,
            Some(TlsWeaker { from: Prefer }),
        ),
    ];
    for (url, tls, warning) in cases {
        let m = ok(&src("postgresql", &url), Some(&user("a")));
        assert_eq!((m.draft.tls(), tls_warning(&m)), (tls, warning), "{url}");
    }
}

#[test]
fn ssl_config_enabled_precedence_and_case() {
    let local = |enabled: Option<bool>, mode: Option<&str>| RawLocal {
        ssl: Some(RawSsl {
            enabled,
            mode: mode.map(str::to_owned),
            ca_cert: None,
            ..Default::default()
        }),
        ..user("a")
    };
    let tls = |url: &str, l: &RawLocal| {
        let m = ok(&src("postgresql", url), Some(l));
        (m.draft.tls(), tls_warning(&m))
    };
    let (on, off) = (Some(TlsMode::Verify), Some(TlsMode::Off));
    let plain = "jdbc:postgresql://db.example/d";
    let stricter = |from| Some(WarningKind::TlsStricter { from });
    // An unchecked "Use SSL" is no ssl-config at all: pgjdbc's `prefer` applies.
    assert_eq!(
        tls(plain, &local(Some(false), Some("REQUIRE"))),
        (on, stricter(SourceTls::Prefer))
    );
    let weaker = Some(WarningKind::TlsWeaker {
        from: SourceTls::Prefer,
    });
    assert_eq!(
        tls(
            "jdbc:postgresql://localhost/d",
            &local(Some(false), Some("VERIFY_FULL"))
        ),
        (off, weaker)
    );
    assert_eq!(
        tls(plain, &local(Some(true), Some("VERIFY_FULL"))),
        (on, None)
    );
    assert_eq!(
        tls(plain, &local(None, Some("verify_ca"))),
        (on, stricter(SourceTls::VerifyCa))
    );
    assert_eq!(
        tls(plain, &local(Some(true), None)),
        (on, stricter(SourceTls::Prefer))
    );
    assert_eq!(
        tls(
            &format!("{plain}?sslmode=verify-full"),
            &local(Some(true), Some("REQUIRE"))
        ),
        (on, None)
    );
    assert_eq!(
        tls(
            &format!("{plain}?sslmode=disable"),
            &local(Some(true), Some("VERIFY_FULL"))
        ),
        (off, None)
    );
    assert_eq!(
        tls(
            &format!("{plain}?sslmode=require"),
            &local(Some(false), None)
        ),
        (on, stricter(SourceTls::Require))
    );
    let mut by_property = src("postgresql", plain);
    by_property.properties = vec![("sslmode".into(), "Verify-Full".into())];
    assert_eq!(
        ok(&by_property, Some(&local(Some(true), Some("REQUIRE"))))
            .draft
            .tls(),
        on
    );
}

#[test]
fn a_ca_path_is_kept_only_when_it_exists_here() {
    let dir = tempfile::TempDir::new().unwrap();
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, "x").unwrap();
    let local = |path: &str| RawLocal {
        ssl: Some(RawSsl {
            enabled: Some(true),
            mode: Some("VERIFY_FULL".into()),
            ca_cert: Some(path.into()),
            ..Default::default()
        }),
        ..user("a")
    };
    let s = src("postgresql", "jdbc:postgresql://db.example/d");
    let m = ok(&s, Some(&local(ca.to_str().unwrap())));
    assert_eq!(m.draft.tls_ca(), ca.to_str());
    assert!(!m.warnings.contains(&WarningKind::CaCertMissing));
    let unchecked = RawLocal {
        ssl: Some(RawSsl {
            enabled: Some(false),
            mode: None,
            ca_cert: ca.to_str().map(str::to_owned),
            ..Default::default()
        }),
        ..user("a")
    };
    assert_eq!(ok(&s, Some(&unchecked)).draft.tls_ca(), None);
    for missing in [
        "/nonexistent/ca.pem",
        "C:\\Users\\x\\ca.crt",
        "relative/ca.pem",
    ] {
        let m = ok(&s, Some(&local(missing)));
        assert_eq!(m.draft.tls_ca(), None, "{missing}");
        assert!(
            m.warnings.contains(&WarningKind::CaCertMissing),
            "{missing}"
        );
    }
}

#[test]
fn sqlite_paths_expand_macros_and_resolve_against_the_working_dir() {
    let lite = |url: &str, wd: Option<&str>| RawSource {
        working_dir: wd.map(str::to_owned),
        ..src("sqlite.xerial", url)
    };
    let path = |s: &RawSource| ok(s, None).draft.path().map(str::to_owned);
    let some = |p: &str| Some(p.to_owned());
    assert_eq!(
        path(&lite("jdbc:sqlite:$PROJECT_DIR$/data/app.db", None)),
        some("/proj/data/app.db")
    );
    assert_eq!(
        path(&lite("jdbc:sqlite:$ProjectFileDir$/a.db", None)),
        some("/proj/a.db")
    );
    assert_eq!(
        path(&lite("jdbc:sqlite:$USER_HOME$/dbs/b.db", None)),
        some("/home/u/dbs/b.db")
    );
    assert_eq!(
        path(&lite("jdbc:sqlite:c.db", Some("$ProjectFileDir$/sub"))),
        some("/proj/sub/c.db")
    );
    assert_eq!(
        path(&lite("jdbc:sqlite:c.db", Some("rel"))),
        some("/proj/rel/c.db")
    );
    assert_eq!(path(&lite("jdbc:sqlite:c.db", None)), some("/proj/c.db"));
    assert_eq!(
        path(&lite("jdbc:sqlite:/abs/d.db", Some("/elsewhere"))),
        some("/abs/d.db")
    );
    assert_eq!(
        path(&lite("jdbc:sqlite:D:\\data\\e.db", None)),
        some("D:\\data\\e.db")
    );
}

#[test]
fn sqlite_skips_and_warnings() {
    let lite = |url: &str| src("sqlite", url);
    for url in [
        "jdbc:sqlite::memory:",
        "jdbc:sqlite:",
        "jdbc:sqlite::resource:db/x.db",
    ] {
        assert_eq!(skip(&lite(url), None), SkipReason::InMemory, "{url}");
    }
    assert_eq!(
        skip(&lite("jdbc:sqlite:$MODULE_DIR$/x.db"), None),
        SkipReason::UnknownMacro("$MODULE_DIR$".into())
    );
    let no_home = MapContext {
        home: None,
        ..ctx()
    };
    assert_eq!(
        map_source(&lite("jdbc:sqlite:$USER_HOME$/x.db"), None, &no_home).err(),
        Some(SkipReason::UnknownMacro("$USER_HOME$".into()))
    );
    let m = ok(
        &lite("jdbc:sqlite:/nonexistent/f.db?journal_mode=WAL&busy_timeout=5000"),
        None,
    );
    assert_eq!(m.draft.path(), Some("/nonexistent/f.db"));
    assert_eq!(
        m.warnings,
        [
            WarningKind::DroppedParameter("journal_mode".into()),
            WarningKind::DroppedParameter("busy_timeout".into()),
            WarningKind::FileMissing
        ]
    );
    let dir = tempfile::TempDir::new().unwrap();
    let db = dir.path().join("g.db");
    std::fs::write(&db, "").unwrap();
    let m = ok(&lite(&format!("jdbc:sqlite:{}", db.display())), None);
    assert!(m.warnings.is_empty());
    assert_eq!(
        (m.draft.secret(), m.draft.tls(), m.draft.settings().len()),
        (None, None, 0)
    );
}

#[test]
fn sqlite_file_uris_map_to_their_path() {
    let lite = |url: &str| src("sqlite.xerial", url);
    let path = |url: &str| ok(&lite(url), None).draft.path().map(str::to_owned);
    let some = |p: &str| Some(p.to_owned());
    assert_eq!(path("jdbc:sqlite:file:/abs/a.db"), some("/abs/a.db"));
    assert_eq!(path("jdbc:sqlite:file:///abs/b.db"), some("/abs/b.db"));
    assert_eq!(
        path("jdbc:sqlite:file://localhost/abs/c.db"),
        some("/abs/c.db")
    );
    assert_eq!(path("jdbc:sqlite:file:rel/d.db"), some("/proj/rel/d.db"));
    assert_eq!(
        path("jdbc:sqlite:file:/abs/my%20e.db"),
        some("/abs/my e.db")
    );
    assert_eq!(
        path("jdbc:sqlite:file:$PROJECT_DIR$/f.db?mode=ro"),
        some("/proj/f.db")
    );
    let m = ok(
        &lite("jdbc:sqlite:file:/nonexistent/g.db?cache=shared"),
        None,
    );
    assert_eq!(
        m.warnings,
        [
            WarningKind::DroppedParameter("cache".into()),
            WarningKind::FileMissing
        ]
    );
    for url in [
        "jdbc:sqlite:file::memory:",
        "jdbc:sqlite:file::memory:?cache=shared",
        "jdbc:sqlite:file:x.db?mode=memory",
    ] {
        assert_eq!(skip(&lite(url), None), SkipReason::InMemory, "{url}");
    }
}

#[test]
fn a_dollar_in_an_existing_sqlite_filename_is_not_a_macro() {
    let dir = tempfile::TempDir::new().unwrap();
    for name in ["price$list.db", "a$b$c.db", "$tmp$.db"] {
        let db = dir.path().join(name);
        std::fs::write(&db, "").unwrap();
        let m = ok(
            &src("sqlite", &format!("jdbc:sqlite:{}", db.display())),
            None,
        );
        assert_eq!(m.draft.path(), Some(db.to_str().unwrap()), "{name}");
        assert!(m.warnings.is_empty(), "{name}: {:?}", m.warnings);
    }
    // A macro-shaped name that resolves to no file is still an unknown macro.
    let gone = format!("jdbc:sqlite:{}/$MODULE_DIR$/x.db", dir.path().display());
    assert_eq!(
        skip(&src("sqlite", &gone), None),
        SkipReason::UnknownMacro("$MODULE_DIR$".into())
    );
}

#[test]
fn names_are_sanitized() {
    for (raw, want) in [
        ("Orders DB", "orders-db"),
        ("\u{200B}Reporting (RO)", "reporting-ro"),
        ("\u{FEFF}a__b", "a__b"),
        ("--x--", "x"),
        ("日本", "datasource"),
        ("", "datasource"),
        ("app.db", "app-db"),
    ] {
        assert_eq!(base_name(raw), want, "{raw:?}");
    }
    // File order "App", "app", "App-2": the third base is already taken by the second.
    let mut used = std::collections::HashSet::new();
    let names: Vec<String> = ["App", "app", "App-2"]
        .iter()
        .map(|n| assign_name(&base_name(n), &mut used))
        .collect();
    assert_eq!(names, ["app", "app-2", "app-2-2"]);
}

#[test]
fn an_at_sign_anywhere_after_the_scheme_refuses_the_url() {
    // A password holding `/` or `?` must not slip past the userinfo check into a field.
    for url in [
        "jdbc:postgresql://u:5432?hunter2@h/db",
        "jdbc:postgresql://u:/secret@h/db",
        "jdbc:postgresql://u:12/secret@h/db",
    ] {
        let result = map_source(&src("postgresql", url), Some(&user("a")), &ctx());
        assert_eq!(
            result.as_ref().err(),
            Some(&SkipReason::InvalidUrl),
            "{url}"
        );
        let text = format!("{result:?}");
        assert!(
            !text.contains("hunter2") && !text.contains("secret"),
            "{text}"
        );
    }
}

#[test]
fn plus_decodes_to_a_space_as_in_pgjdbc() {
    // pgjdbc decodes the database and parameter values with java.net.URLDecoder.
    let m = ok(
        &src("postgresql", "jdbc:postgresql://h/my+db?currentSchema=a+b"),
        Some(&user("a")),
    );
    assert_eq!(m.draft.database(), Some("my db"));
    assert_eq!(
        m.draft.settings().get("search_path").map(String::as_str),
        Some("a b")
    );
    assert_eq!(
        ok(&src("postgresql", "jdbc:postgresql:x+y"), Some(&user("a")))
            .draft
            .database(),
        Some("x y")
    );
}

#[test]
fn malformed_ports_and_paths_are_invalid() {
    let u = user("a");
    for url in [
        "jdbc:postgresql://h:0/d",
        "jdbc:postgresql://h:+12/d",
        "jdbc:postgresql://[::1]:+12/d",
        "jdbc:postgresql:/d",
        "jdbc:postgresql://h/a/b",
    ] {
        assert_eq!(
            skip(&src("postgresql", url), Some(&u)),
            SkipReason::InvalidUrl,
            "{url}"
        );
    }
}

#[test]
fn a_relative_ca_path_is_dropped_even_when_it_exists_from_the_current_dir() {
    // Tests run in the crate folder, where Cargo.toml exists as a relative path.
    assert!(Path::new("Cargo.toml").exists());
    let local = RawLocal {
        ssl: Some(RawSsl {
            enabled: Some(true),
            mode: Some("VERIFY_FULL".into()),
            ca_cert: Some("Cargo.toml".into()),
            ..Default::default()
        }),
        ..user("a")
    };
    let m = ok(
        &src("postgresql", "jdbc:postgresql://db.example/d"),
        Some(&local),
    );
    assert_eq!(m.draft.tls_ca(), None);
    assert!(m.warnings.contains(&WarningKind::CaCertMissing));
}

#[test]
fn a_host_with_a_percent_or_other_non_hostname_char_is_invalid() {
    // An encoded password must never be stored verbatim as the host.
    let u = user("a");
    for url in [
        "jdbc:postgresql://u%3Ahunter2%40h/db",
        "jdbc:postgresql://h%2Fx:5432/db",
        "jdbc:postgresql://h st/db",
    ] {
        let result = map_source(&src("postgresql", url), Some(&u), &ctx());
        assert_eq!(
            result.as_ref().err(),
            Some(&SkipReason::InvalidUrl),
            "{url}"
        );
        assert!(!format!("{result:?}").contains("hunter2"), "{url}");
    }
    let m = ok(
        &src("postgresql", "jdbc:postgresql://db_1.example-2/d"),
        Some(&u),
    );
    assert_eq!(m.draft.host(), Some("db_1.example-2"));
}

#[test]
fn a_client_cert_and_key_are_kept_together_only_when_both_exist_here() {
    let dir = tempfile::TempDir::new().unwrap();
    let (cert, key) = (dir.path().join("me.crt"), dir.path().join("me.key"));
    std::fs::write(&cert, "x").unwrap();
    std::fs::write(&key, "x").unwrap();
    let local = |mode: &str, cert: Option<&str>, key: Option<&str>| RawLocal {
        ssl: Some(RawSsl {
            enabled: Some(true),
            mode: Some(mode.into()),
            client_cert: cert.map(str::to_owned),
            client_key: key.map(str::to_owned),
            ..Default::default()
        }),
        ..user("a")
    };
    let s = src("postgresql", "jdbc:postgresql://db.example/d");
    let (c, k) = (cert.to_str().unwrap(), key.to_str().unwrap());
    let m = ok(&s, Some(&local("VERIFY_FULL", Some(c), Some(k))));
    assert_eq!((m.draft.tls_cert(), m.draft.tls_key()), (Some(c), Some(k)));
    assert!(!m.warnings.contains(&WarningKind::ClientCertMissing));
    // Half a pair, a missing file or a mode that ends up off drops both, with a warning.
    for (mode, cert, key) in [
        ("VERIFY_FULL", Some(c), None),
        ("VERIFY_FULL", None, Some(k)),
        ("VERIFY_FULL", Some(c), Some("/nonexistent/me.key")),
        ("VERIFY_FULL", Some("relative/me.crt"), Some(k)),
    ] {
        let m = ok(&s, Some(&local(mode, cert, key)));
        assert_eq!(
            (m.draft.tls_cert(), m.draft.tls_key()),
            (None, None),
            "{cert:?} {key:?}"
        );
        assert!(
            m.warnings.contains(&WarningKind::ClientCertMissing),
            "{cert:?} {key:?}"
        );
    }
    let m = ok(&s, Some(&user("a")));
    assert!(!m.warnings.contains(&WarningKind::ClientCertMissing));
}

#[test]
fn colour_values_map_by_name_or_hex_whatever_their_case() {
    use crate::Color;
    let cases = [
        ("Red", Ok(Some(Color::Red))),
        ("rose", Ok(Some(Color::Red))),
        ("ORANGE", Ok(Some(Color::Orange))),
        ("Yellow", Ok(Some(Color::Yellow))),
        ("Green", Ok(Some(Color::Green))),
        ("lime", Ok(Some(Color::Green))),
        ("Aqua", Ok(Some(Color::Teal))),
        ("Teal", Ok(Some(Color::Teal))),
        ("blue", Ok(Some(Color::Blue))),
        ("BLUE", Ok(Some(Color::Blue))),
        ("Violet", Ok(Some(Color::Purple))),
        ("Purple", Ok(Some(Color::Purple))),
        ("Gray", Ok(Some(Color::Gray))),
        ("grey", Ok(Some(Color::Gray))),
        ("no", Ok(None)),
        ("Inherit", Ok(None)),
        ("", Ok(None)),
        ("  ", Ok(None)),
        ("17295e", Ok(Some(Color::Blue))),
        ("#17295E", Ok(Some(Color::Blue))),
        ("ffffff", Ok(None)),
        ("facfcf", Err(WarningKind::ColorUnknown("facfcf".into()))),
        ("Mauve", Err(WarningKind::ColorUnknown("Mauve".into()))),
        ("12345", Err(WarningKind::ColorUnknown("12345".into()))),
        ("ééé", Err(WarningKind::ColorUnknown("ééé".into()))),
    ];
    for (value, want) in cases {
        assert_eq!(color(value), want, "{value}");
    }
}

#[test]
fn mysql_family_driver_refs_urls_and_classes_detect_mysql_and_forks_are_skipped() {
    let raw = |d: Option<&str>, j: Option<&str>, u: &str| RawSource {
        driver_ref: d.map(str::to_owned),
        jdbc_driver: j.map(str::to_owned),
        url: Some(u.to_owned()),
        ..Default::default()
    };
    let my = Ok(Engine::Mysql);
    let no = |s: &str| Err(SkipReason::UnsupportedDriver(s.into()));
    for (s, want) in [
        (raw(Some("mysql.8"), None, "jdbc:mysql://h/d"), my.clone()),
        (raw(Some("mysql"), None, "jdbc:mysql://h"), my.clone()),
        (raw(Some("mariadb"), None, "jdbc:mariadb://h/d"), my.clone()),
        (raw(None, None, "jdbc:mariadb://h/d"), my.clone()),
        (raw(Some("custom"), None, "jdbc:mysql://h/d"), my.clone()),
        (
            raw(None, Some("com.mysql.cj.jdbc.Driver"), "jdbc:other:x"),
            my.clone(),
        ),
        (
            raw(None, Some("com.mysql.jdbc.Driver"), "jdbc:other:x"),
            my.clone(),
        ),
        (
            raw(None, Some("org.mariadb.jdbc.Driver"), "jdbc:other:x"),
            my.clone(),
        ),
        (
            raw(Some("mysql_aurora_aws"), None, "jdbc:mysql:aws://h:3306/d"),
            no("mysql_aurora_aws"),
        ),
        (
            raw(Some("custom"), None, "jdbc:mysql:aws://h:3306/d"),
            no("mysql:aws"),
        ),
        (
            raw(
                Some("mysql_aurora"),
                Some("org.mariadb.jdbc.Driver"),
                "jdbc:mariadb://h/d",
            ),
            no("mysql_aurora"),
        ),
        (
            raw(
                Some("tidb"),
                Some("com.mysql.cj.jdbc.Driver"),
                "jdbc:mysql://h:4000/test",
            ),
            no("tidb"),
        ),
    ] {
        assert_eq!(detect_engine(&s), want, "{:?}", s.driver_ref);
    }
}

#[test]
fn mysql_sources_map_host_port_database_user_and_tls() {
    let s = src(
        "mysql.8",
        "jdbc:mysql://db.example:3307/shop?sslMode=VERIFY_IDENTITY&connectTimeout=5",
    );
    let m = ok(&s, Some(&user("ann")));
    let d = &m.draft;
    assert_eq!(
        (
            d.engine(),
            d.host(),
            d.port(),
            d.database(),
            d.user(),
            d.tls(),
            d.secret()
        ),
        (
            Engine::Mysql,
            Some("db.example"),
            Some(3307),
            Some("shop"),
            Some("ann"),
            Some(TlsMode::Verify),
            Some(SecretSetting::Keychain)
        )
    );
    assert_eq!(
        m.warnings,
        [WarningKind::DroppedParameter("connectTimeout".into())]
    );
    let maria = ok(
        &src("mariadb", "jdbc:mariadb://localhost:3306"),
        Some(&user("ann")),
    );
    assert_eq!(
        (
            maria.draft.port(),
            maria.draft.database(),
            maria.draft.tls()
        ),
        (None, None, Some(TlsMode::Off))
    );
    assert!(maria.warnings.is_empty(), "{:?}", maria.warnings);
    let remote = ok(
        &src("mysql.8", "jdbc:mysql://db.example/shop"),
        Some(&user("ann")),
    );
    assert_eq!(
        (remote.draft.tls(), tls_warning(&remote)),
        (
            Some(TlsMode::Verify),
            Some(WarningKind::TlsStricter {
                from: SourceTls::Prefer
            })
        )
    );
}

#[test]
fn mysql_user_falls_back_to_the_url_then_the_default_and_no_auth_keeps_no_secret() {
    let s = src("mysql.8", "jdbc:mysql://h/d?user=from_url");
    assert_eq!(ok(&s, Some(&user("local"))).draft.user(), Some("local"));
    assert_eq!(ok(&s, None).draft.user(), Some("from_url"));
    let bare = src("mysql.8", "jdbc:mysql://h/d");
    assert_eq!(skip(&bare, None), SkipReason::NoUser);
    let with_default = MapContext {
        default_user: Some("dflt"),
        ..ctx()
    };
    assert_eq!(
        map_source(&bare, None, &with_default).unwrap().draft.user(),
        Some("dflt")
    );
    let no_auth = RawLocal {
        auth_provider: Some("no-auth".into()),
        ..user("a")
    };
    assert_eq!(
        ok(&bare, Some(&no_auth)).draft.secret(),
        Some(SecretSetting::None)
    );
}

#[test]
fn a_mysql_url_mode_beats_driver_properties_and_the_ssl_config() {
    let local = |enabled: bool, mode: &str| RawLocal {
        ssl: Some(RawSsl {
            enabled: Some(enabled),
            mode: Some(mode.into()),
            ..Default::default()
        }),
        ..user("a")
    };
    let tls = |s: &RawSource, l: &RawLocal| {
        let m = ok(s, Some(l));
        (m.draft.tls(), tls_warning(&m))
    };
    let (on, off) = (Some(TlsMode::Verify), Some(TlsMode::Off));
    let plain = src("mysql.8", "jdbc:mysql://db.example/d");
    assert_eq!(
        tls(&plain, &local(true, "REQUIRE")),
        (
            on,
            Some(WarningKind::TlsStricter {
                from: SourceTls::Require
            })
        )
    );
    let disabled = src("mysql.8", "jdbc:mysql://db.example/d?sslMode=DISABLED");
    assert_eq!(tls(&disabled, &local(true, "VERIFY_FULL")), (off, None));
    let mut by_property = src("mariadb", "jdbc:mariadb://db.example/d");
    by_property.properties = vec![("sslMode".into(), "verify-full".into())];
    assert_eq!(tls(&by_property, &local(true, "REQUIRE")), (on, None));
    let maria = src("mariadb", "jdbc:mariadb://db.example/d");
    assert_eq!(tls(&maria, &local(false, "VERIFY_FULL")), (off, None));
}

#[test]
fn unusable_mysql_sources_are_skipped_with_a_reason() {
    for (url, reason) in [
        ("jdbc:mysql://a,b/d", SkipReason::MultiHost),
        ("jdbc:mysql:replication://a,b/d", SkipReason::MultiHost),
        ("jdbc:mysql://address=(host=a)/d", SkipReason::InvalidUrl),
        (
            "jdbc:mysql://localhost/d?socket=/tmp/mysql.sock",
            SkipReason::UnixSocket,
        ),
    ] {
        assert_eq!(
            skip(&src("mysql.8", url), Some(&user("a"))),
            reason,
            "{url}"
        );
    }
    let tunnelled = RawLocal {
        ssh_enabled: true,
        ..user("a")
    };
    assert_eq!(
        skip(&src("mysql.8", "jdbc:mysql://h/d"), Some(&tunnelled)),
        SkipReason::SshTunnel
    );
}

#[test]
fn a_mysql_password_in_userinfo_or_parameters_warns_and_is_dropped() {
    for url in [
        "jdbc:mysql://ann:fake-pw@db.example/d",
        "jdbc:mysql://db.example/d?password=fake-pw",
    ] {
        let m = ok(&src("mysql.8", url), Some(&user("ann")));
        assert!(m.warnings.contains(&WarningKind::PasswordInUrl), "{url}");
        assert!(!format!("{m:?}").contains("fake-pw"), "{url}");
    }
}
