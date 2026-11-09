//! Read-only SQL Server through a reader principal: the session impersonates it, so the
//! database refuses writes whatever the batch does.

use crate::support::{ms_fixture, ms_off, ms_reader, run, text};
use dbento_core::{
    ConnectError, ExecuteError, Limits, ReadOnlyEnforcement, RelationRef, SessionOptions,
};

const T: &[&str] = &[
    "CREATE TABLE dbo.t (id int IDENTITY PRIMARY KEY, v nvarchar(20))",
    "INSERT dbo.t (v) VALUES (N'seed')",
];
const READER: &str = "read_only_user = \"dbento_reader\"";

#[tokio::test]
async fn a_reader_principal_is_enforced_by_the_database() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_reader(T, &format!("read_only = true\n{READER}")).await;
    let mut s = fx.session(SessionOptions::default()).await;
    assert_eq!(
        s.read_only_enforcement(),
        Some(ReadOnlyEnforcement::Database)
    );
    let who = run(&mut s, "SELECT USER_NAME() AS u", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(who.rows, vec![vec![text("dbento_reader")]]);
    for sql in [
        "INSERT dbo.t (v) VALUES (N'w')",
        "UPDATE dbo.t SET v = N'w' WHERE id = 1",
        "DELETE dbo.t WHERE id = 1",
        "TRUNCATE TABLE dbo.t",
        "CREATE TABLE dbo.n (a int)",
        "DROP TABLE dbo.t",
        "KILL 1",
    ] {
        let r = run(&mut s, sql, Limits::default()).await.remove(0);
        assert!(
            matches!(r, Err(ExecuteError::ReadOnlyViolation { .. })),
            "{sql}: {:?}",
            r.err()
        );
    }
    let via = run(
        &mut s,
        "EXEC('SELECT COUNT(*) AS n FROM dbo.t')",
        Limits::default(),
    )
    .await
    .remove(0)
    .unwrap();
    assert_eq!(via.rows, vec![vec![text("1")]]);
    assert!(matches!(
        run(&mut s, "BEGIN TRANSACTION", Limits::default())
            .await
            .remove(0),
        Err(ExecuteError::TransactionControl { .. })
    ));
    assert_eq!(
        s.columns(&RelationRef::new("dbo", "t"))
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        fx.db.scalar("SELECT COUNT(*) FROM dbo.t").await.as_deref(),
        Some("1")
    );
}

#[tokio::test]
async fn the_reader_applies_only_to_read_only_sessions() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_reader(T, READER).await;
    let mut writable = fx.session(SessionOptions::default()).await;
    assert_eq!(writable.read_only_enforcement(), None);
    let who = run(&mut writable, "SELECT USER_NAME() AS u", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(who.rows, vec![vec![text("dbo")]]);
    let mut forced = SessionOptions::default();
    forced.force_read_only = true;
    let mut ro = fx.session(forced).await;
    assert_eq!(
        ro.read_only_enforcement(),
        Some(ReadOnlyEnforcement::Database)
    );
    let who = run(&mut ro, "SELECT USER_NAME() AS u", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(who.rows, vec![vec![text("dbento_reader")]]);
}

// The session may step into the databases guest can enter, but no route gives back the login.
#[tokio::test]
async fn the_reader_cannot_be_shed_or_left_behind() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_reader(T, &format!("read_only = true\n{READER}")).await;
    let mut s = fx.session(SessionOptions::default()).await;
    for sql in [
        "REVERT",
        "EXECUTE AS LOGIN = N'sa'",
        "EXECUTE AS USER = N'dbo'",
        "SETUSER 'dbo'",
    ] {
        assert!(
            run(&mut s, sql, Limits::default()).await.remove(0).is_err(),
            "{sql}"
        );
    }
    run(&mut s, "USE master", Limits::default())
        .await
        .remove(0)
        .unwrap();
    let write = format!("INSERT [{}].dbo.t (v) VALUES (N'w')", fx.db.database());
    let r = run(&mut s, &write, Limits::default()).await.remove(0);
    assert!(
        matches!(r, Err(ExecuteError::ReadOnlyViolation { .. })),
        "{:?}",
        r.err()
    );
    assert_eq!(
        s.columns(&RelationRef::new("dbo", "t"))
            .await
            .unwrap()
            .len(),
        2
    );
    let back = format!("USE [{}]", fx.db.database());
    run(&mut s, &back, Limits::default())
        .await
        .remove(0)
        .unwrap();
    let who = run(&mut s, "SELECT USER_NAME() AS u", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(who.rows, vec![vec![text("dbento_reader")]]);
    assert_eq!(
        fx.db.scalar("SELECT COUNT(*) FROM dbo.t").await.as_deref(),
        Some("1")
    );
}

#[tokio::test]
async fn a_wrong_reader_is_a_connect_error_naming_the_key_never_a_fallback() {
    dbento_testkit::skip_without_mssql!();
    for name in ["nobody", "o'brien"] {
        let extra = format!("tls = \"off\"\nread_only = true\nread_only_user = \"{name}\"");
        let fx = ms_fixture(dbento_testkit::mssql_db().await, &extra);
        match fx.try_session(SessionOptions::default()).await {
            Err(ConnectError::Open { message, .. }) => assert!(
                message.contains("read_only_user")
                    && message.contains(name)
                    && message.contains("15517"),
                "{message}"
            ),
            other => panic!("{name}: {:?}", other.err()),
        }
    }
}

// Each of these can write, so reporting the database's enforcement would be a lie, and with
// layer 3 off a hidden commit would keep what they wrote.
#[tokio::test]
async fn a_principal_that_can_write_is_a_connect_error_never_database_enforcement() {
    dbento_testkit::skip_without_mssql!();
    for (name, setup, holds) in [
        ("dbo", "", "db_owner"),
        ("r", "ALTER ROLE db_owner ADD MEMBER r", "db_owner"),
        ("r", "ALTER ROLE db_datawriter ADD MEMBER r", "INSERT"),
        ("r", "ALTER ROLE db_ddladmin ADD MEMBER r", "ALTER ANY"),
        (
            "r",
            "ALTER ROLE db_securityadmin ADD MEMBER r",
            "ALTER ANY ROLE",
        ),
        (
            "r",
            "ALTER ROLE db_accessadmin ADD MEMBER r",
            "ALTER ANY USER",
        ),
        ("r", "GRANT ALTER TO r", "ALTER"),
    ] {
        let db = dbento_testkit::mssql_db().await;
        if !setup.is_empty() {
            db.scalar(&format!("CREATE USER r WITHOUT LOGIN; {setup}"))
                .await;
        }
        let extra = format!("tls = \"off\"\nread_only = true\nread_only_user = \"{name}\"");
        let fx = ms_fixture(db, &extra);
        match fx.try_session(SessionOptions::default()).await {
            Err(ConnectError::Open { message, .. }) => assert!(
                message.contains("read_only_user") && message.contains(holds),
                "{setup}: {message}"
            ),
            other => panic!("{setup}: {:?}", other.err()),
        }
    }
}

#[tokio::test]
async fn a_write_role_undone_by_db_denydatawriter_is_a_reader() {
    dbento_testkit::skip_without_mssql!();
    let db = dbento_testkit::mssql_db().await;
    db.scalar(
        "CREATE USER r WITHOUT LOGIN; ALTER ROLE db_datawriter ADD MEMBER r; \
         ALTER ROLE db_denydatawriter ADD MEMBER r",
    )
    .await;
    let extra = "tls = \"off\"\nread_only = true\nread_only_user = \"r\"";
    let fx = ms_fixture(db, extra);
    let s = fx.session(SessionOptions::default()).await;
    assert_eq!(
        s.read_only_enforcement(),
        Some(ReadOnlyEnforcement::Database)
    );
}

#[tokio::test]
async fn the_set_up_reader_cannot_change_its_own_default_schema() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_reader(T, &format!("read_only = true\n{READER}")).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let sql = "ALTER USER dbento_reader WITH DEFAULT_SCHEMA = sys EXEC('COMMIT COMMIT BEGIN TRAN')";
    let r = run(&mut s, sql, Limits::default()).await.remove(0);
    assert!(
        matches!(r, Err(ExecuteError::ReadOnlyViolation { .. })),
        "{:?}",
        r.err()
    );
    let kept = "SELECT default_schema_name FROM sys.database_principals \
                WHERE name = N'dbento_reader'";
    assert_eq!(fx.db.scalar(kept).await.as_deref(), Some("dbo"));
}

#[tokio::test]
async fn a_read_only_database_refuses_as_a_violation_in_any_session() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    fx.db.scalar("CREATE TABLE dbo.t (a int)").await;
    fx.db
        .scalar(&format!(
            "USE master; ALTER DATABASE [{}] SET READ_ONLY WITH ROLLBACK IMMEDIATE",
            fx.db.database()
        ))
        .await;
    let mut s = fx.session(SessionOptions::default()).await;
    match run(&mut s, "INSERT dbo.t VALUES (1)", Limits::default())
        .await
        .remove(0)
    {
        Err(ExecuteError::ReadOnlyViolation { message }) => {
            assert!(message.contains("read-only"), "{message}")
        }
        other => panic!("{:?}", other.err()),
    }
}

#[tokio::test]
async fn a_reader_connection_states_the_level_its_session_reports() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_reader(T, &format!("read_only = true\n{READER}")).await;
    let s = fx.session(SessionOptions::default()).await;
    assert_eq!(
        Some(fx.conn().read_only_enforcement()),
        s.read_only_enforcement()
    );
}
