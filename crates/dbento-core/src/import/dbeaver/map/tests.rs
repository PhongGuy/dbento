//! One raw DBeaver connection to a draft: engines, fields, URLs, users, TLS, skips and SQLite.
use super::*;
use crate::import::dbeaver::project::{RawConfig, RawConnection, RawHandler};
use crate::import::model::{SecretSetting, SkipReason, SourceTls, WarningKind};
use crate::{Engine, TlsMode};
use std::collections::BTreeMap;
use std::path::Path;

static NO_TYPES: BTreeMap<String, String> = BTreeMap::new();

fn ctx() -> MapContext<'static> {
    MapContext {
        root: Path::new("/proj"),
        default_user: None,
        connection_types: &NO_TYPES,
    }
}

fn conn(provider: &str, driver: &str, config: RawConfig) -> RawConnection {
    RawConnection {
        id: "c-1".into(),
        provider: provider.into(),
        driver: driver.into(),
        name: "Shop".into(),
        user: Some("clerk".into()),
        config,
        ..Default::default()
    }
}

fn manual(host: &str, port: &str, database: &str) -> RawConnection {
    let config = RawConfig {
        host: Some(host.into()),
        port: Some(port.into()),
        database: Some(database.into()),
        ..Default::default()
    };
    conn("postgresql", "postgres-jdbc", config)
}

fn url(u: &str) -> RawConnection {
    let config = RawConfig {
        url: Some(u.into()),
        configuration_type: Some("URL".into()),
        ..Default::default()
    };
    conn("postgresql", "postgres-jdbc", config)
}

fn handler(enabled: bool, props: &[(&str, &str)]) -> RawHandler {
    let properties = props
        .iter()
        .map(|(k, v)| ((*k).into(), (*v).into()))
        .collect();
    RawHandler {
        enabled,
        properties,
    }
}

fn props(p: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
    p.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect()
}

fn ok(c: &RawConnection) -> Mapped {
    map_connection(c, &ctx()).unwrap_or_else(|r| panic!("{r:?}"))
}

fn skip(c: &RawConnection) -> SkipReason {
    map_connection(c, &ctx()).expect_err("skipped")
}

#[test]
fn engines_come_from_the_provider_and_driver_allowlist() {
    let (pg, lite) = (Ok(Engine::Postgres), Ok(Engine::Sqlite));
    let no = |s: &str| Err(SkipReason::UnsupportedDriver(s.into()));
    for (p, d, want) in [
        ("postgresql", "postgres-jdbc", pg.clone()),
        ("postgresql", "postgresql", pg.clone()),
        ("generic", "postgresql", pg.clone()),
        ("sqlite", "sqlite_jdbc", lite.clone()),
        ("generic", "sqlite_jdbc", lite.clone()),
        ("generic", "sqlite_xerial", lite.clone()),
        ("generic", "sqlite_zentus", lite.clone()),
        ("generic", "sqlite_ch_werner", lite.clone()),
        (
            "postgresql",
            "postgres-timescale-jdbc",
            no("postgresql/postgres-timescale-jdbc"),
        ),
        (
            "postgresql",
            "postgres-redshift-jdbc",
            no("postgresql/postgres-redshift-jdbc"),
        ),
        (
            "greenplum",
            "greenplum-jdbc",
            no("greenplum/greenplum-jdbc"),
        ),
        ("sqlite", "libsql_jdbc", no("sqlite/libsql_jdbc")),
        ("mysql", "starRocks", no("mysql/starRocks")),
    ] {
        assert_eq!(detect_engine(p, d), want, "{p}/{d}");
    }
    assert_eq!(
        skip(&conn("mysql", "starRocks", RawConfig::default())),
        SkipReason::UnsupportedDriver("mysql/starRocks".into())
    );
}

#[test]
fn manual_postgres_uses_the_fields_and_ignores_a_stale_url() {
    let mut c = manual("", "6543", "shop");
    c.config.url = Some("jdbc:postgresql://stale.example:1/stale".into());
    c.read_only = true;
    c.folder = Some(" Prod / EU ".into());
    let m = ok(&c);
    let d = &m.draft;
    assert_eq!(
        (d.engine(), d.host(), d.port(), d.database(), d.user()),
        (
            Engine::Postgres,
            Some("localhost"),
            Some(6543),
            Some("shop"),
            Some("clerk")
        )
    );
    assert_eq!(
        (d.secret(), d.read_only(), d.folder().map(|f| f.as_str())),
        (Some(SecretSetting::Keychain), true, Some("Prod/EU"))
    );
    assert!(m.keep_secret);
    let d = ok(&manual("db.example", "", "")).draft;
    assert_eq!((d.port(), d.database()), (None, Some("clerk")));
    assert_eq!(
        skip(&manual("db.example", "54x", "shop")),
        SkipReason::InvalidUrl
    );
}

#[test]
fn url_postgres_merges_url_parameters_over_driver_properties() {
    let mut c = url("jdbc:postgresql://db.example:5433/shop?currentSchema=app&password=fake-pw-3");
    c.config.properties = props(&[
        ("ApplicationName", "dbento"),
        ("currentSchema", "other"),
        ("loggerLevel", "OFF"),
    ]);
    let m = ok(&c);
    let s = m.draft.settings();
    assert_eq!(
        (
            s.get("search_path").map(String::as_str),
            s.get("application_name").map(String::as_str)
        ),
        (Some("app"), Some("dbento"))
    );
    assert_eq!(
        m.warnings,
        [
            WarningKind::PasswordInUrl,
            WarningKind::DroppedParameter("loggerLevel".into()),
            WarningKind::TlsStricter {
                from: SourceTls::Prefer
            }
        ]
    );
    assert_eq!(
        (m.draft.host(), m.draft.port()),
        (Some("db.example"), Some(5433))
    );
    assert!(!format!("{m:?}").contains("fake-pw"));
    for (u, reason) in [
        ("", SkipReason::MissingUrl),
        ("jdbc:postgresql://a,b/shop", SkipReason::MultiHost),
        (
            "jdbc:postgresql:///shop?service=billing",
            SkipReason::ServiceFile,
        ),
        ("jdbc:postgresql://h:x/shop", SkipReason::InvalidUrl),
    ] {
        assert_eq!(skip(&url(u)), reason, "{u}");
    }
    let mut none = url("x");
    none.config.url = None;
    assert_eq!(skip(&none), SkipReason::MissingUrl);
}

#[test]
fn the_user_is_the_saved_one_then_the_url_one_then_the_default() {
    let mut c = url("jdbc:postgresql://db.example/?user=url_user");
    assert_eq!(ok(&c).draft.user(), Some("clerk"));
    c.user = None;
    let d = ok(&c).draft;
    assert_eq!(
        (d.user(), d.database()),
        (Some("url_user"), Some("url_user"))
    );
    let mut bare = manual("db.example", "", "shop");
    bare.user = None;
    assert_eq!(skip(&bare), SkipReason::NoUser);
    let with_default = MapContext {
        root: Path::new("/proj"),
        default_user: Some("fallback"),
        ..ctx()
    };
    assert_eq!(
        map_connection(&bare, &with_default).unwrap().draft.user(),
        Some("fallback")
    );
}

#[test]
fn ssh_then_proxy_then_placeholders_skip_the_connection() {
    let mut c = manual("db.example", "", "shop");
    c.config.handlers.insert(
        "ssh_tunnel".into(),
        handler(true, &[("host", "jump.example")]),
    );
    c.config
        .handlers
        .insert("socks_proxy".into(), handler(true, &[]));
    assert_eq!(skip(&c), SkipReason::SshTunnel);
    c.config
        .handlers
        .insert("ssh_tunnel".into(), handler(false, &[]));
    assert_eq!(skip(&c), SkipReason::Proxy);
    c.config.handlers.clear();
    assert!(map_connection(&c, &ctx()).is_ok());
    let macro_of = |m: RawConnection| skip(&m);
    let mut host = manual("${DB_HOST}", "", "shop");
    assert_eq!(
        macro_of(host),
        SkipReason::UnknownMacro("${DB_HOST}".into())
    );
    host = manual("db.example", "${PORT}", "shop");
    assert_eq!(macro_of(host), SkipReason::UnknownMacro("${PORT}".into()));
    host = manual("db.example", "", "shop_${ENV}_x");
    assert_eq!(macro_of(host), SkipReason::UnknownMacro("${ENV}".into()));
    assert_eq!(
        skip(&url("jdbc:postgresql://${HOST}/shop")),
        SkipReason::UnknownMacro("${HOST}".into())
    );
    let lite = conn(
        "sqlite",
        "sqlite_jdbc",
        RawConfig {
            database: Some("${HOME}/x.db".into()),
            ..Default::default()
        },
    );
    assert_eq!(skip(&lite), SkipReason::UnknownMacro("${HOME}".into()));
}

#[test]
fn an_enabled_ssl_handler_wins_and_without_a_mode_means_verify_full() {
    let dir = tempfile::TempDir::new().unwrap();
    let file = |n: &str| {
        let p = dir.path().join(n);
        std::fs::write(&p, "x").unwrap();
        p.to_string_lossy().into_owned()
    };
    let (ca, cert, key) = (file("ca.pem"), file("c.pem"), file("k.pem"));
    let with = |enabled: bool, hp: &[(&str, &str)], params: &[(&str, &str)]| {
        let mut c = manual("db.example", "", "shop");
        c.config
            .handlers
            .insert("postgre_ssl".into(), handler(enabled, hp));
        c.config.properties = props(params);
        ok(&c)
    };
    let m = with(
        true,
        &[("sslMode", "verify-ca"), ("ssl.ca.cert", ca.as_str())],
        &[("sslmode", "disable")],
    );
    assert_eq!(
        (m.draft.tls(), m.draft.tls_ca()),
        (Some(TlsMode::Verify), Some(ca.as_str()))
    );
    assert_eq!(
        m.warnings,
        [WarningKind::TlsStricter {
            from: SourceTls::VerifyCa
        }]
    );
    let m = with(
        true,
        &[
            ("rootCert", ca.as_str()),
            ("clientCert", cert.as_str()),
            ("ssl.client.key", key.as_str()),
        ],
        &[],
    );
    assert_eq!(
        (
            m.draft.tls(),
            m.draft.tls_ca(),
            m.draft.tls_cert(),
            m.draft.tls_key()
        ),
        (
            Some(TlsMode::Verify),
            Some(ca.as_str()),
            Some(cert.as_str()),
            Some(key.as_str())
        )
    );
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    let m = with(
        false,
        &[("sslMode", "verify-full")],
        &[("sslmode", "disable")],
    );
    assert_eq!(m.draft.tls(), Some(TlsMode::Off));
    let m = with(
        true,
        &[("sslMode", "require"), ("ssl.ca.cert", "relative/ca.pem")],
        &[],
    );
    assert_eq!(
        m.warnings,
        [
            WarningKind::TlsStricter {
                from: SourceTls::Require
            },
            WarningKind::CaCertMissing
        ]
    );
}

#[test]
fn a_non_native_auth_model_warns_and_keeps_no_secret() {
    let mut c = manual("db.example", "", "shop");
    c.config.auth_model = Some("native".into());
    let m = ok(&c);
    assert!(
        m.keep_secret
            && !m
                .warnings
                .iter()
                .any(|w| matches!(w, WarningKind::AuthModel(_)))
    );
    c.config.auth_model = Some("postgres_pgpass".into());
    let m = ok(&c);
    assert!(!m.keep_secret);
    assert_eq!(
        m.warnings.last(),
        Some(&WarningKind::AuthModel("postgres_pgpass".into()))
    );
    assert_eq!(m.draft.secret(), Some(SecretSetting::Keychain));
}

#[test]
fn sqlite_paths_come_from_the_database_field_or_the_url() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("data")).unwrap();
    std::fs::write(dir.path().join("data/app.db"), "").unwrap();
    let here = dir
        .path()
        .join("data/app.db")
        .to_string_lossy()
        .into_owned();
    let ctx = MapContext {
        root: dir.path(),
        ..ctx()
    };
    let lite = |cfg: RawConfig| conn("sqlite", "sqlite_jdbc", cfg);
    let run = |cfg: RawConfig| map_connection(&lite(cfg), &ctx);
    let m = run(RawConfig {
        database: Some("data/app.db".into()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        (m.draft.engine(), m.draft.path()),
        (Engine::Sqlite, Some(here.as_str()))
    );
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    let m = run(RawConfig {
        url: Some("jdbc:sqlite:data/app.db".into()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(m.draft.path(), Some(here.as_str()));
    let mut cfg = RawConfig {
        url: Some("jdbc:sqlite:/abs/missing.db?journal_mode=WAL&cache=shared".into()),
        configuration_type: Some("url".into()),
        database: Some("ignored.db".into()),
        ..Default::default()
    };
    cfg.properties = props(&[("busy_timeout", "5000")]);
    let m = run(cfg).unwrap();
    assert_eq!(m.draft.path(), Some("/abs/missing.db"));
    assert_eq!(
        m.warnings,
        [
            WarningKind::DroppedParameter("journal_mode".into()),
            WarningKind::DroppedParameter("cache".into()),
            WarningKind::DroppedParameter("busy_timeout".into()),
            WarningKind::FileMissing
        ]
    );
    for db in ["", ":memory:"] {
        let got = run(RawConfig {
            database: Some(db.into()),
            ..Default::default()
        });
        assert_eq!(got.expect_err("in memory"), SkipReason::InMemory, "{db:?}");
    }
    assert_eq!(
        run(RawConfig::default()).expect_err("no path"),
        SkipReason::InMemory
    );
    let win = run(RawConfig {
        database: Some(r"C:\data\app.db".into()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(win.draft.path(), Some(r"C:\data\app.db"));
}

#[test]
fn a_placeholder_skip_never_carries_field_text() {
    let reason = skip(&url("jdbc:postgresql://h/db?password=fake${pw-1&user=u"));
    let SkipReason::UnknownMacro(detail) = &reason else {
        panic!("{reason:?}");
    };
    assert!(
        !detail.contains("fake") && !detail.contains("pw-1"),
        "{detail}"
    );
    assert_eq!(detail, "${");
    let odd = skip(&manual("db.example", "", "a${b c}d"));
    assert_eq!(odd, SkipReason::UnknownMacro("${".into()));
}

fn my_manual(driver: &str, host: &str, port: &str, database: &str) -> RawConnection {
    let config = RawConfig {
        host: Some(host.into()),
        port: Some(port.into()),
        database: Some(database.into()),
        ..Default::default()
    };
    conn("mysql", driver, config)
}

fn tls_from(m: &Mapped) -> (Option<TlsMode>, Option<SourceTls>) {
    let from = m.warnings.iter().find_map(|w| match w {
        WarningKind::TlsStricter { from } | WarningKind::TlsWeaker { from } => Some(*from),
        _ => None,
    });
    (m.draft.tls(), from)
}

#[test]
fn mysql_family_drivers_are_mysql_and_other_mysql_drivers_are_not() {
    for d in ["mysql8", "mysql5", "mysql3", "mariaDB"] {
        assert_eq!(detect_engine("mysql", d), Ok(Engine::Mysql), "{d}");
    }
    for (p, d) in [
        ("mysql", "starRocks"),
        ("mysql", "mysql_ndb"),
        ("mysql", "mariadb"),
        ("tidb", "tidb"),
    ] {
        assert_eq!(
            detect_engine(p, d),
            Err(SkipReason::UnsupportedDriver(format!("{p}/{d}"))),
            "{p}/{d}"
        );
    }
}

#[test]
fn manual_mysql_uses_the_fields_and_without_a_handler_is_plain() {
    let m = ok(&my_manual("mysql8", "db.shop.example", "3307", "shop"));
    let d = &m.draft;
    assert_eq!(
        (d.engine(), d.host(), d.port(), d.database(), d.user()),
        (
            Engine::Mysql,
            Some("db.shop.example"),
            Some(3307),
            Some("shop"),
            Some("clerk")
        )
    );
    assert_eq!(d.tls(), Some(TlsMode::Off));
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    assert!(m.keep_secret);
    let blank = ok(&my_manual("mariaDB", "localhost", "", ""));
    assert_eq!((blank.draft.port(), blank.draft.database()), (None, None));
}

#[test]
fn url_mysql_parses_the_url_merges_properties_and_skips_what_it_cannot_use() {
    let config = RawConfig {
        url: Some("jdbc:mariadb://localhost:3308/shop?connectTimeout=5".into()),
        configuration_type: Some("URL".into()),
        properties: props(&[
            ("sslMode", "verify-full"),
            ("allowPublicKeyRetrieval", "true"),
        ]),
        ..Default::default()
    };
    let m = ok(&conn("mysql", "mariaDB", config));
    assert_eq!(
        (
            m.draft.host(),
            m.draft.port(),
            m.draft.database(),
            m.draft.tls()
        ),
        (
            Some("localhost"),
            Some(3308),
            Some("shop"),
            Some(TlsMode::Verify)
        )
    );
    assert_eq!(
        m.warnings,
        [
            WarningKind::DroppedParameter("connectTimeout".into()),
            WarningKind::DroppedParameter("allowPublicKeyRetrieval".into())
        ]
    );
    let empty = RawConfig {
        configuration_type: Some("URL".into()),
        ..Default::default()
    };
    assert_eq!(
        skip(&conn("mysql", "mysql8", empty)),
        SkipReason::MissingUrl
    );
    assert_eq!(
        skip(&my_manual("mysql8", "${db_host}", "", "")),
        SkipReason::UnknownMacro("${db_host}".into())
    );
}

#[test]
fn the_mysql_ssl_handler_maps_by_driver_and_properties_beat_it() {
    use SourceTls::*;
    let with = |driver: &str, h: RawHandler, p: &[(&str, &str)]| {
        let mut c = my_manual(driver, "db.example", "", "shop");
        c.config.handlers.insert("mysql_ssl".into(), h);
        c.config.properties = props(p);
        tls_from(&ok(&c))
    };
    let (on, off) = (Some(TlsMode::Verify), Some(TlsMode::Off));
    let verify = [("ssl.verify.server", "true")];
    let require = [("ssl.require", "true"), ("ssl.verify.server", "false")];
    assert_eq!(
        with("mysql8", handler(true, &verify), &[]),
        (on, Some(VerifyCa))
    );
    assert_eq!(
        with("mysql8", handler(true, &require), &[]),
        (on, Some(Require))
    );
    assert_eq!(with("mysql5", handler(true, &[]), &[]), (on, Some(Prefer)));
    assert_eq!(with("mariaDB", handler(true, &verify), &[]), (on, None));
    assert_eq!(
        with("mariaDB", handler(true, &[]), &[]),
        (on, Some(Require))
    );
    assert_eq!(with("mysql8", handler(false, &verify), &[]), (off, None));
    assert_eq!(
        with("mysql8", handler(true, &verify), &[("sslMode", "DISABLED")]),
        (off, None)
    );
}

#[test]
fn mysql_ssl_handler_paths_are_the_ca_cert_and_key() {
    let mut c = my_manual("mysql8", "db.example", "", "shop");
    let h = handler(
        true,
        &[
            ("ssl.verify.server", "true"),
            ("ssl.ca.cert", "/nonexistent/ca.pem"),
            ("ssl.client.cert", "/nonexistent/c.pem"),
            ("ssl.client.key", "/nonexistent/c.key"),
        ],
    );
    c.config.handlers.insert("mysql_ssl".into(), h);
    let m = ok(&c);
    assert_eq!((m.draft.tls_ca(), m.draft.tls_cert()), (None, None));
    assert_eq!(
        m.warnings,
        [
            WarningKind::TlsStricter {
                from: SourceTls::VerifyCa
            },
            WarningKind::CaCertMissing,
            WarningKind::ClientCertMissing
        ]
    );
}

#[test]
fn mysql_skips_tunnels_and_users_and_a_non_native_auth_model_keeps_no_secret() {
    let mut t = my_manual("mysql8", "db.example", "", "shop");
    t.config
        .handlers
        .insert("ssh_tunnel".into(), handler(true, &[]));
    assert_eq!(skip(&t), SkipReason::SshTunnel);
    let mut a = my_manual("mysql8", "db.example", "", "shop");
    a.config.auth_model = Some("mysql_iam".into());
    let m = ok(&a);
    assert!(!m.keep_secret);
    assert!(
        m.warnings
            .contains(&WarningKind::AuthModel("mysql_iam".into()))
    );
    let mut nobody = my_manual("mysql8", "db.example", "", "shop");
    nobody.user = None;
    assert_eq!(skip(&nobody), SkipReason::NoUser);
}
