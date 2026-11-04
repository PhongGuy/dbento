//! One raw Beekeeper connection to a draft: engines, skips, fields, TLS, sockets, auth, SQLite.
use super::*;
use crate::import::beekeeper::db::RawConnection;
use crate::import::model::{SecretSetting, SkipReason, SourceTls, WarningKind};
use crate::{Engine, TlsMode};
use std::path::Path;
use tempfile::TempDir;

fn ctx() -> MapContext<'static> {
    MapContext {
        root: Path::new("/bk"),
        default_user: None,
    }
}

fn pg(host: &str) -> RawConnection {
    RawConnection {
        id: 1,
        name: "Shop".into(),
        connection_type: Some("postgresql".into()),
        host: Some(host.into()),
        username: Some("clerk".into()),
        default_database: Some("shop".into()),
        ..RawConnection::default()
    }
}

fn ok(c: &RawConnection) -> Mapped {
    map_connection(c, &ctx()).unwrap_or_else(|r| panic!("{r:?}"))
}

fn skip(c: &RawConnection) -> SkipReason {
    map_connection(c, &ctx()).expect_err("skipped")
}

fn tls(ssl: bool, ca: Option<&str>, cert: Option<&str>, key: Option<&str>, reject: bool) -> Mapped {
    ok(&RawConnection {
        ssl,
        ssl_ca_file: ca.map(Into::into),
        ssl_cert_file: cert.map(Into::into),
        ssl_key_file: key.map(Into::into),
        ssl_reject_unauthorized: reject,
        ..pg("db.example")
    })
}

#[test]
fn engines_are_postgres_with_its_aliases_and_sqlite() {
    for t in ["postgresql", "psql", "postgres"] {
        assert_eq!(engine(Some(t)), Ok(Engine::Postgres), "{t}");
    }
    assert_eq!(engine(Some("sqlite")), Ok(Engine::Sqlite));
    for t in [
        "redshift",
        "cockroachdb",
        "greengage",
        "libsql",
        "duckdb",
        "Postgresql",
    ] {
        assert_eq!(
            engine(Some(t)),
            Err(SkipReason::UnsupportedDriver(t.into())),
            "{t}"
        );
    }
    assert_eq!(
        engine(None),
        Err(SkipReason::UnsupportedDriver("unknown".into()))
    );
}

#[test]
fn skips_are_checked_cloud_then_driver_then_ssh() {
    let all = RawConnection {
        workspace_id: 7,
        connection_type: Some("redshift".into()),
        ssh_enabled: true,
        ..pg("db.example")
    };
    assert_eq!(skip(&all), SkipReason::CloudWorkspace);
    let local = RawConnection {
        workspace_id: -1,
        ..all.clone()
    };
    assert_eq!(
        skip(&local),
        SkipReason::UnsupportedDriver("redshift".into())
    );
    let ssh = RawConnection {
        connection_type: Some("postgresql".into()),
        ..local
    };
    assert_eq!(skip(&ssh), SkipReason::SshTunnel);
}

#[test]
fn postgres_fields_map_to_the_draft_with_folder_and_read_only() {
    let raw = RawConnection {
        port: Some(5433),
        read_only: true,
        folder: "Prod/EU".parse().ok(),
        ..pg(" db.example ")
    };
    let m = ok(&raw);
    let d = &m.draft;
    assert_eq!(
        (
            d.engine(),
            d.host(),
            d.port(),
            d.database(),
            d.user(),
            d.secret()
        ),
        (
            Engine::Postgres,
            Some("db.example"),
            Some(5433),
            Some("shop"),
            Some("clerk"),
            Some(SecretSetting::Keychain)
        )
    );
    assert!(d.read_only() && m.keep_secret);
    assert_eq!(d.folder().map(|f| f.as_str()), Some("Prod/EU"));
    let bare = ok(&RawConnection {
        host: None,
        default_database: None,
        ..pg("")
    });
    assert_eq!(
        (bare.draft.host(), bare.draft.port(), bare.draft.database()),
        (Some("localhost"), None, Some("clerk"))
    );
}

#[test]
fn a_port_outside_1_to_65535_is_invalid() {
    for port in [0, -1, 65536] {
        let raw = RawConnection {
            port: Some(port),
            ..pg("db.example")
        };
        assert_eq!(skip(&raw), SkipReason::InvalidUrl, "{port}");
    }
}

#[test]
fn tls_follows_ssl_the_cert_files_and_reject_unauthorized() {
    let stricter = WarningKind::TlsStricter {
        from: SourceTls::Require,
    };
    let off = tls(false, Some("/nonexistent/ca.pem"), None, None, true);
    assert_eq!(
        (off.draft.tls(), off.warnings),
        (Some(TlsMode::Off), vec![])
    );
    let req = tls(true, None, None, None, true);
    assert_eq!(
        (req.draft.tls(), req.warnings),
        (Some(TlsMode::Verify), vec![stricter.clone()])
    );
    // A blank cert path is no file, so it stays `require` rather than turning verify-full.
    let blank = tls(true, Some("  "), None, None, true);
    assert_eq!(
        (blank.draft.tls(), blank.draft.tls_ca(), blank.warnings),
        (Some(TlsMode::Verify), None, vec![stricter.clone()])
    );
    let full = tls(true, Some("/nonexistent/ca.pem"), None, None, true);
    assert_eq!(
        (full.draft.tls(), full.warnings),
        (Some(TlsMode::Verify), vec![WarningKind::CaCertMissing])
    );
    let loose = tls(
        true,
        None,
        Some("/nonexistent/c.pem"),
        Some("/nonexistent/c.key"),
        false,
    );
    assert_eq!(
        (loose.draft.tls(), loose.warnings),
        (
            Some(TlsMode::Verify),
            vec![stricter, WarningKind::ClientCertMissing]
        )
    );
}

#[test]
fn an_existing_absolute_ca_file_is_kept() {
    let dir = TempDir::new().unwrap();
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, b"fake").unwrap();
    let m = tls(true, ca.to_str(), None, None, true);
    assert_eq!((m.draft.tls_ca(), m.warnings), (ca.to_str(), vec![]));
}

#[test]
fn a_socket_path_becomes_the_host_with_tls_off_and_ssl_fields_ignored() {
    let raw = RawConnection {
        socket_path_enabled: true,
        ssl: true,
        ssl_ca_file: Some("/nonexistent/ca.pem".into()),
        port: Some(5433),
        ..pg("db.example")
    };
    let m = ok(&raw);
    assert_eq!(
        (m.draft.host(), m.draft.port(), m.draft.tls(), m.warnings),
        (
            Some("/var/run/postgresql"),
            Some(5433),
            Some(TlsMode::Off),
            vec![]
        )
    );
    let custom = ok(&RawConnection {
        socket_path: Some("/tmp/pg".into()),
        ..raw
    });
    assert_eq!(custom.draft.host(), Some("/tmp/pg"));
}

#[test]
fn iam_and_azure_auth_import_without_keeping_the_secret() {
    let iam = ok(&RawConnection {
        iam_enabled: true,
        ..pg("db.example")
    });
    assert!(!iam.keep_secret);
    assert_eq!(iam.warnings, [WarningKind::AuthModel("aws_iam".into())]);
    let azure = ok(&RawConnection {
        azure_enabled: true,
        ..pg("db.example")
    });
    assert!(!azure.keep_secret);
    assert_eq!(azure.warnings, [WarningKind::AuthModel("azure".into())]);
    assert_eq!(azure.draft.secret(), Some(SecretSetting::Keychain));
}

#[test]
fn iam_wins_over_azure_when_both_are_on() {
    let both = ok(&RawConnection {
        iam_enabled: true,
        azure_enabled: true,
        ..pg("db.example")
    });
    assert!(!both.keep_secret);
    assert_eq!(both.warnings, [WarningKind::AuthModel("aws_iam".into())]);
}

#[test]
fn an_empty_user_falls_back_to_the_default_user_or_is_no_user() {
    let raw = RawConnection {
        username: Some("  ".into()),
        ..pg("db.example")
    };
    assert_eq!(skip(&raw), SkipReason::NoUser);
    let ctx = MapContext {
        root: Path::new("/bk"),
        default_user: Some("fallback"),
    };
    assert_eq!(
        map_connection(&raw, &ctx).unwrap().draft.user(),
        Some("fallback")
    );
}

#[test]
fn sqlite_takes_its_path_from_default_database_against_the_data_folder() {
    let lite = |db: Option<&str>| RawConnection {
        id: 2,
        name: "Lite".into(),
        connection_type: Some("sqlite".into()),
        default_database: db.map(Into::into),
        read_only: true,
        ..RawConnection::default()
    };
    let rel = ok(&lite(Some("lite/shop.db")));
    assert_eq!(
        (rel.draft.engine(), rel.draft.path(), rel.draft.read_only()),
        (Engine::Sqlite, Some("/bk/lite/shop.db"), true)
    );
    assert_eq!(rel.warnings, [WarningKind::FileMissing]);
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("a.db");
    std::fs::write(&file, b"").unwrap();
    let abs = ok(&lite(file.to_str()));
    assert_eq!((abs.draft.path(), abs.warnings.len()), (file.to_str(), 0));
    for db in [None, Some(""), Some(":memory:")] {
        assert_eq!(skip(&lite(db)), SkipReason::InMemory, "{db:?}");
    }
}

fn my(kind: &str, host: &str) -> RawConnection {
    RawConnection {
        connection_type: Some(kind.into()),
        port: Some(3307),
        ..pg(host)
    }
}

#[test]
fn mysql_and_mariadb_are_mysql_and_their_forks_are_not() {
    assert_eq!(engine(Some("mysql")), Ok(Engine::Mysql));
    assert_eq!(engine(Some("mariadb")), Ok(Engine::Mysql));
    for t in ["tidb", "starrocks", "MySQL"] {
        assert_eq!(
            engine(Some(t)),
            Err(SkipReason::UnsupportedDriver(t.into())),
            "{t}"
        );
    }
}

#[test]
fn mysql_fields_map_to_the_draft_and_keep_the_secret() {
    let m = ok(&RawConnection {
        read_only: true,
        ..my("mysql", "db.shop.example")
    });
    let d = &m.draft;
    assert_eq!(
        (
            d.engine(),
            d.host(),
            d.port(),
            d.database(),
            d.user(),
            d.read_only(),
            d.tls()
        ),
        (
            Engine::Mysql,
            Some("db.shop.example"),
            Some(3307),
            Some("shop"),
            Some("clerk"),
            true,
            Some(TlsMode::Off)
        )
    );
    assert!(m.keep_secret);
    assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    let bare = ok(&RawConnection {
        port: None,
        default_database: Some(" ".into()),
        ..my("mariadb", "localhost")
    });
    assert_eq!((bare.draft.port(), bare.draft.database()), (None, None));
}

#[test]
fn mysql_tls_follows_ssl_and_reject_unauthorized_without_a_host_name_check() {
    let t = |ssl: bool, ca: Option<&str>, reject: bool| {
        let m = ok(&RawConnection {
            ssl,
            ssl_ca_file: ca.map(Into::into),
            ssl_reject_unauthorized: reject,
            ..my("mysql", "db.example")
        });
        let from = m.warnings.iter().find_map(|w| match w {
            WarningKind::TlsStricter { from } | WarningKind::TlsWeaker { from } => Some(*from),
            _ => None,
        });
        (m.draft.tls(), from)
    };
    let on = Some(TlsMode::Verify);
    assert_eq!(
        t(false, Some("/nonexistent/ca.pem"), true),
        (Some(TlsMode::Off), None)
    );
    assert_eq!(t(true, None, true), (on, Some(SourceTls::Require)));
    assert_eq!(
        t(true, Some("/nonexistent/ca.pem"), true),
        (on, Some(SourceTls::VerifyCa))
    );
    assert_eq!(
        t(true, Some("/nonexistent/ca.pem"), false),
        (on, Some(SourceTls::Require))
    );
}

#[test]
fn an_existing_mysql_ca_file_is_kept() {
    let dir = TempDir::new().unwrap();
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, "x").unwrap();
    let m = ok(&RawConnection {
        ssl: true,
        ssl_ca_file: ca.to_str().map(Into::into),
        ..my("mysql", "db.example")
    });
    assert_eq!(m.draft.tls_ca(), ca.to_str());
}

#[test]
fn mysql_sockets_iam_azure_tunnels_ports_and_missing_users() {
    let base = || my("mysql", "db.example");
    assert_eq!(
        skip(&RawConnection {
            socket_path_enabled: true,
            ..base()
        }),
        SkipReason::UnixSocket
    );
    assert_eq!(
        skip(&RawConnection {
            iam_enabled: true,
            ..base()
        }),
        SkipReason::UnsupportedDriver("aws_iam".into())
    );
    assert_eq!(
        skip(&RawConnection {
            azure_enabled: true,
            ..my("mariadb", "db.example")
        }),
        SkipReason::UnsupportedDriver("azure".into())
    );
    assert_eq!(
        skip(&RawConnection {
            ssh_enabled: true,
            socket_path_enabled: true,
            ..base()
        }),
        SkipReason::SshTunnel
    );
    assert_eq!(
        skip(&RawConnection {
            port: Some(70000),
            ..base()
        }),
        SkipReason::InvalidUrl
    );
    assert_eq!(
        skip(&RawConnection {
            username: Some(" ".into()),
            ..base()
        }),
        SkipReason::NoUser
    );
    let fallback = MapContext {
        default_user: Some("dflt"),
        ..ctx()
    };
    let m = map_connection(
        &RawConnection {
            username: None,
            ..base()
        },
        &fallback,
    )
    .unwrap();
    assert_eq!(m.draft.user(), Some("dflt"));
}
