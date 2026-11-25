//! SQL Server through the SSH bastion: the login and its TLS ride one channel, verified under
//! the name the certificate carries.
use std::time::Duration;

use crate::ssh_support::engine_tunnel;
use crate::support::{run, text};
use dbento_core::{Limits, Secret};
use dbento_testkit::{MS_SECRET_VAR, mssql_on, published_ips, ssh_net};

#[tokio::test]
async fn sql_server_logs_in_through_the_tunnel_with_tls_verified_under_the_alias() {
    dbento_testkit::skip_without_mssql!();
    let net = ssh_net().await;
    let (db, on) = mssql_on(net.network(), true).await;
    let ips = published_ips(&on.id).await;
    assert!(
        !ips.is_empty() && ips.iter().all(|ip| ip == "127.0.0.1"),
        "{ips:?}"
    );
    let body = format!(
        "engine = \"mssql\"\nhost = \"{}\"\nport = 1433\ndatabase = \"{}\"\nuser = \"{}\"\nsecret = {{ env = \"{MS_SECRET_VAR}\" }}\ntls = \"verify\"\ntls_ca = \"ca.pem\"",
        on.alias,
        db.database(),
        db.user()
    );
    let t = engine_tunnel(
        &net,
        "ms",
        &body,
        &[("ca.pem", on.ca_pem.as_deref().unwrap())],
    );
    t.trust(&net);
    let mut s = t
        .session("ms", Some(Secret::new(db.password())))
        .await
        .unwrap();
    let sql = "SELECT encrypt_option FROM sys.dm_exec_connections WHERE session_id = @@SPID";
    let ran = run(&mut s, sql, Limits::default()).await.remove(0).unwrap();
    assert_eq!(ran.rows, vec![vec![text("TRUE")]]);
    s.close().await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while net.ssh_sessions().await > 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "SSH outlived its session"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
