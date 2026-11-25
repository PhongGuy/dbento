//! Redis through the SSH bastion, plain and under TLS that Dbento wraps around the channel.
#![cfg(feature = "redis")]

use std::time::Duration;

use crate::ssh_support::{Tunnel, engine_tunnel};
use crate::support::{read, text};
use dbento_core::ConnectError;
use dbento_testkit::{SshNet, named_tls, published_ips, ssh_net, valkey_on};

/// Connection `rd` logging in as the default user to `host:port` inside the bastion's network.
fn rd_tunnel(net: &SshNet, host: &str, port: u16, tls: &str, files: &[(&str, &str)]) -> Tunnel {
    let body = format!(
        "engine = \"redis\"\nhost = \"{host}\"\nport = {port}\ndatabase = \"0\"\nsecret = \"none\"\n{tls}"
    );
    let t = engine_tunnel(net, "rd", &body, files);
    t.trust(net);
    t
}

#[tokio::test]
async fn redis_answers_through_the_tunnel_which_closes_with_the_session() {
    let net = ssh_net().await;
    let (db, on) = valkey_on(net.network(), false).await;
    db.cmd(&["SET", "ssh:k", "tunneled"]).await;
    let t = rd_tunnel(&net, &on.alias, 6379, "tls = \"off\"", &[]);
    let mut s = t.session("rd", None).await.unwrap();
    assert_eq!(
        read(&mut s, "GET ssh:k").await.rows,
        vec![vec![text("tunneled")]]
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
async fn tls_around_the_channel_verifies_the_alias() {
    let net = ssh_net().await;
    let (db, on) = valkey_on(net.network(), true).await;
    let ips = published_ips(&on.id).await;
    assert!(
        ips.len() >= 2 && ips.iter().all(|ip| ip == "127.0.0.1"),
        "{ips:?}"
    );
    db.cmd(&["SET", "ssh:k", "secured"]).await;
    let ca = on.ca_pem.clone().unwrap();
    let tls = "tls = \"verify\"\ntls_ca = \"ca.pem\"";
    let t = rd_tunnel(&net, &on.alias, 6380, tls, &[("ca.pem", ca.as_str())]);
    let mut s = t.session("rd", None).await.unwrap();
    assert_eq!(
        read(&mut s, "GET ssh:k").await.rows,
        vec![vec![text("secured")]]
    );
    s.close().await.unwrap();
}

#[tokio::test]
async fn a_certificate_from_another_ca_fails_as_tls() {
    let net = ssh_net().await;
    let (_db, on) = valkey_on(net.network(), true).await;
    let stranger = named_tls(&[on.alias.as_str()]).ca_pem;
    let tls = "tls = \"verify\"\ntls_ca = \"ca.pem\"";
    let t = rd_tunnel(&net, &on.alias, 6380, tls, &[("ca.pem", stranger.as_str())]);
    match t.session("rd", None).await {
        Err(ConnectError::Tls { message, .. }) => assert!(message.contains("tls_ca"), "{message}"),
        other => panic!("{:?}", other.err()),
    }
}
