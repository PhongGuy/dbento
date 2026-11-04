//! Report codes of the Beekeeper reasons and warnings.
use super::*;

#[test]
fn the_beekeeper_reasons_and_warnings_have_codes_and_no_detail() {
    for (reason, code) in [
        (SkipReason::CloudWorkspace, "cloud_workspace"),
        (SkipReason::EmptyQuery, "empty_query"),
    ] {
        assert_eq!((reason.code(), reason.detail()), (code, None));
    }
    for (kind, code) in [
        (WarningKind::KeyMissing, "key_missing"),
        (WarningKind::SecretUnreadable, "secret_unreadable"),
    ] {
        assert_eq!((kind.code(), kind.detail()), (code, None));
    }
}

#[test]
fn an_unknown_colour_carries_its_raw_value() {
    let kind = WarningKind::ColorUnknown("x".into());
    assert_eq!((kind.code(), kind.detail()), ("color_unknown", Some("x")));
}

fn my_fields() -> MysqlDraft {
    MysqlDraft {
        host: "db.example".into(),
        port: Some(3306),
        database: None,
        user: "app".into(),
        secret: SecretSetting::Keychain,
        tls: Some(TlsMode::Verify),
        tls_ca: None,
        tls_cert: None,
        tls_key: None,
    }
}

#[test]
fn a_mysql_draft_reads_through_the_shared_accessors_and_drops_the_default_port() {
    let d = ConnectionDraft::mysql(my_fields(), true);
    assert_eq!(d.engine(), Engine::Mysql);
    assert_eq!(
        (d.host(), d.port(), d.database(), d.user()),
        (Some("db.example"), None, None, Some("app"))
    );
    assert_eq!(
        (d.secret(), d.tls(), d.path(), d.read_only()),
        (
            Some(SecretSetting::Keychain),
            Some(TlsMode::Verify),
            None,
            true
        )
    );
    assert!(d.settings().is_empty());
    let other = MysqlDraft {
        port: Some(3307),
        database: Some("shop".into()),
        ..my_fields()
    };
    let other = ConnectionDraft::mysql(other, false);
    assert_eq!((other.port(), other.database()), (Some(3307), Some("shop")));
}

#[test]
fn a_unix_socket_skip_has_its_code_and_no_detail() {
    let r = SkipReason::UnixSocket;
    assert_eq!((r.code(), r.detail()), ("unix_socket", None));
}
