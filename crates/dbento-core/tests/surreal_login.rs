//! Who signed in: the login's level and role, what that makes the session, and the existence
//! pre-check that runs before any `use`.
mod support;

use dbento_core::{ConnectError, ReadOnlyEnforcement, Session, SessionOptions};
use dbento_testkit::{SURREAL_TAG, surreal_db, surreal_server};
use support::{run, sr_session, sr_try, text};

fn forced() -> SessionOptions {
    let mut opts = SessionOptions::default();
    opts.force_read_only = true;
    opts
}

fn open_message(e: Result<Session, ConnectError>) -> String {
    match e {
        Err(ConnectError::Open { message }) => message,
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("connected"),
    }
}

#[tokio::test]
async fn every_level_signs_in_and_lands_on_the_target() {
    let db = surreal_db().await;
    for login in [("ro", "root"), ("ne", "namespace"), ("de", "database")] {
        let mut s = sr_session(&db, Some(login), "", SessionOptions::default()).await;
        let ran = run(
            &mut s,
            "RETURN session::ns(); RETURN session::db()",
            Default::default(),
        )
        .await;
        assert_eq!(ran[0].as_ref().unwrap().rows, [vec![text(db.namespace())]]);
        assert_eq!(ran[1].as_ref().unwrap().rows, [vec![text(db.database())]]);
    }
}

#[tokio::test]
async fn a_guest_is_refused_and_says_why() {
    let guests = surreal_server(SURREAL_TAG, &[("SURREAL_CAPS_ALLOW_GUESTS", "true")]).await;
    match sr_try(&guests, None, "", SessionOptions::default()).await {
        Err(ConnectError::Unsupported { reason }) => {
            assert!(reason.contains("guest"), "{reason}");
        }
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("a guest connected"),
    }
}

#[tokio::test]
async fn anonymous_on_an_authenticating_server_is_refused() {
    let db = surreal_db().await;
    let e = sr_try(&db, None, "", SessionOptions::default()).await;
    assert!(
        matches!(
            e,
            Err(ConnectError::Unsupported { .. } | ConnectError::Open { .. })
        ),
        "{:?}",
        e.err()
    );
}

#[tokio::test]
async fn anonymous_on_an_unauthenticated_server_counts_as_owner() {
    let open = surreal_server(SURREAL_TAG, &[("SURREAL_UNAUTHENTICATED", "true")]).await;
    let s = sr_session(&open, None, "", SessionOptions::default()).await;
    assert_eq!(s.read_only_enforcement(), None);
    let s = sr_session(&open, None, "", forced()).await;
    assert_eq!(
        s.read_only_enforcement(),
        Some(ReadOnlyEnforcement::BestEffort)
    );
}

#[tokio::test]
async fn a_read_only_level_follows_the_role_and_a_viewer_is_always_read_only() {
    let db = surreal_db().await;
    for (login, requested, want) in [
        (("ro", "root"), false, None),
        (("ne", "namespace"), false, None),
        (("ro", "root"), true, Some(ReadOnlyEnforcement::BestEffort)),
        (
            ("ne", "namespace"),
            true,
            Some(ReadOnlyEnforcement::BestEffort),
        ),
        (
            ("de", "database"),
            true,
            Some(ReadOnlyEnforcement::BestEffort),
        ),
        (("rv", "root"), false, Some(ReadOnlyEnforcement::Database)),
        (
            ("nv", "namespace"),
            false,
            Some(ReadOnlyEnforcement::Database),
        ),
        (
            ("dv", "database"),
            false,
            Some(ReadOnlyEnforcement::Database),
        ),
        (
            ("dv", "database"),
            true,
            Some(ReadOnlyEnforcement::Database),
        ),
    ] {
        let extra = if requested { "read_only = true" } else { "" };
        let s = sr_session(&db, Some(login), extra, SessionOptions::default()).await;
        assert_eq!(s.read_only_enforcement(), want, "{login:?} {requested}");
        assert_eq!(s.is_read_only(), want.is_some(), "{login:?}");
    }
}

#[tokio::test]
async fn several_roles_take_the_strongest_in_any_case() {
    let db = surreal_db().await;
    db.admin(&format!(
        "DEFINE USER both ON DATABASE PASSWORD '{}' ROLES VIEWER, EDITOR",
        db.password()
    ))
    .await;
    let s = sr_session(
        &db,
        Some(("both", "database")),
        "",
        SessionOptions::default(),
    )
    .await;
    assert_eq!(s.read_only_enforcement(), None);
}

#[tokio::test]
async fn a_missing_target_fails_connect_and_creates_nothing() {
    let db = surreal_db().await;
    let before = support::sr_targets(&db).await;
    for login in [Some(("ro", "root")), Some(("ne", "namespace"))] {
        let dir = tempfile::tempdir().unwrap();
        let toml = db.connection_toml("sr", login, "").replace(
            &format!("database = \"{}\"", db.database()),
            "database = \"nosuchdb\"",
        );
        std::fs::write(
            dir.path().join("dbento.toml"),
            format!("dbento = 1\n{toml}"),
        )
        .unwrap();
        let ws = dbento_core::Workspace::open(dir.path()).unwrap();
        let conn = ws.connection("sr").unwrap();
        let secret = Some(dbento_core::Secret::new(db.password()));
        let message =
            open_message(dbento_core::connect(&ws, conn, secret, SessionOptions::default()).await);
        assert!(message.contains("nosuchdb"), "{message}");
    }
    let dir = tempfile::tempdir().unwrap();
    let toml = db.connection_toml("sr", Some(("ro", "root")), "").replace(
        &format!("namespace = \"{}\"", db.namespace()),
        "namespace = \"nosuchns\"",
    );
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!("dbento = 1\n{toml}"),
    )
    .unwrap();
    let ws = dbento_core::Workspace::open(dir.path()).unwrap();
    let conn = ws.connection("sr").unwrap();
    let secret = Some(dbento_core::Secret::new(db.password()));
    let message =
        open_message(dbento_core::connect(&ws, conn, secret, SessionOptions::default()).await);
    assert!(message.contains("nosuchns"), "{message}");
    assert_eq!(support::sr_targets(&db).await, before);
}
