//! MySQL through the SSH bastion: each connection, the kill door's too, rides its own channel
//! behind a one-shot loopback listener, and TLS verifies the database's own name.
use std::time::Duration;

use crate::ssh_support::{Tunnel, engine_tunnel};
use crate::support::{Cell, run, text};
use dbento_core::{ConnectError, ExecuteError, Limits};
use dbento_testkit::{MyDb, SshNet, mysql_on, published_ips, ssh_net};

/// Connection `my` logging in as root to `host` inside the bastion's network.
fn my_tunnel(net: &SshNet, db: &MyDb, host: &str, tls: &str, files: &[(&str, &str)]) -> Tunnel {
    let body = format!(
        "engine = \"mysql\"\nhost = \"{host}\"\nport = 3306\ndatabase = \"{}\"\nuser = \"{}\"\nsecret = \"none\"\n{tls}",
        db.database(),
        db.user()
    );
    let t = engine_tunnel(net, "my", &body, files);
    t.trust(net);
    t
}

#[tokio::test]
async fn a_query_and_a_cancel_ride_the_tunnel_which_closes_with_the_session() {
    let net = ssh_net().await;
    let (db, on) = mysql_on(net.network(), false).await;
    let ips = published_ips(&on.id).await;
    assert!(
        !ips.is_empty() && ips.iter().all(|ip| ip == "127.0.0.1"),
        "{ips:?}"
    );
    let t = my_tunnel(&net, &db, &on.alias, "tls = \"off\"", &[]);
    let mut s = t.session("my", None).await.unwrap();
    let seven = run(&mut s, "SELECT 7", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(seven.rows, vec![vec![text("7")]]);
    let cancel = s.cancel_handle();
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        cancel.cancel();
    });
    let r = tokio::time::timeout(
        Duration::from_secs(20),
        run(&mut s, "SELECT SLEEP(30)", Limits::default()),
    )
    .await
    .expect("the kill never arrived");
    stopper.join().unwrap();
    assert!(
        matches!(r.last(), Some(Err(ExecuteError::Cancelled))),
        "{:?}",
        r.last().map(|r| r.as_ref().err())
    );
    drop(s);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while net.ssh_sessions().await > 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "SSH outlived its session"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn tls_through_the_tunnel_verifies_the_alias_the_certificate_names() {
    let net = ssh_net().await;
    let (db, on) = mysql_on(net.network(), true).await;
    let ca = on.ca_pem.clone().unwrap();
    let tls = "tls = \"verify\"\ntls_ca = \"ca.pem\"";
    let t = my_tunnel(&net, &db, &on.alias, tls, &[("ca.pem", ca.as_str())]);
    let mut s = t.session("my", None).await.unwrap();
    let status = run(
        &mut s,
        "SHOW SESSION STATUS LIKE 'Ssl_version'",
        Limits::default(),
    )
    .await
    .remove(0)
    .unwrap();
    assert!(
        matches!(&status.rows[0][1], Cell::Text(v) if v.starts_with("TLSv1")),
        "{:?}",
        status.rows
    );
    s.close().await.unwrap();
}

#[tokio::test]
async fn a_database_host_the_bastion_cannot_reach_is_a_tunnel_error() {
    let net = ssh_net().await;
    let (db, _on) = mysql_on(net.network(), false).await;
    let t = my_tunnel(&net, &db, "no-such-host.invalid", "tls = \"off\"", &[]);
    match t.session("my", None).await {
        Err(ConnectError::SshTunnel { message }) => {
            assert!(message.contains("no-such-host.invalid:3306"), "{message}");
        }
        other => panic!("{:?}", other.err()),
    }
}
