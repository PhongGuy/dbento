//! The SQL Server harness on the gate server: fresh databases and the test-CA certificate.
use dbento_testkit::{mssql_db, mssql_tls};
use tokio_util::compat::TokioAsyncWriteCompatExt;

#[tokio::test]
async fn each_database_is_fresh_and_on_sql_server_2022_or_later() {
    dbento_testkit::skip_without_mssql!();
    let a = mssql_db().await;
    assert_eq!(
        a.scalar("SELECT DB_NAME()").await.as_deref(),
        Some(a.database())
    );
    assert!(a.database().starts_with("dbento_t_"), "{}", a.database());
    let major = a
        .scalar("SELECT CAST(SERVERPROPERTY('ProductMajorVersion') AS nvarchar(8))")
        .await
        .unwrap();
    assert!(major.parse::<u32>().unwrap() >= 16, "{major}");
    let mut c = a.client().await;
    c.simple_query("CREATE TABLE dbo.marker (x int)")
        .await
        .unwrap()
        .into_results()
        .await
        .unwrap();
    assert_eq!(
        a.scalar("SELECT COUNT(*) FROM sys.tables WHERE name = 'marker'")
            .await
            .as_deref(),
        Some("1")
    );
    let toml = a.connection_toml("ms", "tls = \"off\"");
    assert!(
        toml.contains("engine = \"mssql\"")
            && toml.contains(&format!("database = \"{}\"", a.database())),
        "{toml}"
    );
    assert!(
        toml.contains("DBENTO_TEST_MS_SECRET") && toml.ends_with("tls = \"off\"\n"),
        "{toml}"
    );
}

#[tokio::test]
async fn the_tls_server_presents_the_test_ca_certificate_over_localhost() {
    dbento_testkit::skip_without_mssql!();
    let (db, ca) = mssql_tls().await;
    assert_eq!(db.host(), "localhost");
    assert!(ca.starts_with("-----BEGIN CERTIFICATE-----"), "{ca}");
    let encrypted = db
        .scalar("SELECT encrypt_option FROM sys.dm_exec_connections WHERE session_id = @@SPID")
        .await;
    assert_eq!(encrypted.as_deref(), Some("TRUE"));
    // Verified against the returned CA only: the stock X.509 v1 certificate would be refused.
    let mut config = tiberius::Config::new();
    config.host("localhost");
    config.port(db.port());
    config.database(db.database());
    config.authentication(tiberius::AuthMethod::sql_server(db.user(), db.password()));
    config.encryption(tiberius::EncryptionLevel::Required);
    config.trust_cert_ca_bundle(ca.into_bytes());
    let tcp = tokio::net::TcpStream::connect(config.get_addr())
        .await
        .unwrap();
    let mut client = tiberius::Client::connect(config, tcp.compat_write())
        .await
        .unwrap();
    let row = client
        .simple_query("SELECT DB_NAME()")
        .await
        .unwrap()
        .into_row()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.get::<&str, _>(0), Some(db.database()));
}
