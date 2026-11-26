//! MongoDB through the SSH bastion: the driver's SOCKS5 proxy reaching members by name.
#![cfg(feature = "mongodb")]

use std::time::Duration;

use crate::ssh_support::{Tunnel, engine_tunnel, password};
use crate::support::read;
use dbento_core::ssh_channel_check;
use dbento_testkit::{SSH_PASSWORD, SshNet, mongo_on, published_ips, ssh_net};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Connection `mg` on `uri` through the bastion, with `ca` written as the workspace's `ca.pem`.
pub(crate) fn mg_tunnel(net: &SshNet, uri: &str, ca: &str) -> Tunnel {
    let body = format!("engine = \"mongodb\"\nuri = \"{uri}\"\n");
    let t = engine_tunnel(net, "mg", &body, &[("ca.pem", ca)]);
    t.trust(net);
    t
}

#[tokio::test]
async fn the_members_publish_nothing_and_the_bastion_reaches_them_by_name() {
    let net = ssh_net().await;
    let mg = mongo_on(net.network(), 2).await;
    for id in mg.ids() {
        assert!(published_ips(id).await.is_empty(), "{id}");
    }
    let states = mg
        .eval(
            0,
            "print(rs.status().members.map(m => m.stateStr).join(','))",
        )
        .await;
    assert_eq!(states.trim(), "PRIMARY,SECONDARY");
    let uri = format!("mongodb://{}:27017/app", mg.name(0));
    let t = mg_tunnel(&net, &uri, mg.ca_pem());
    let conn = t.ws.declared("mg").unwrap();
    for name in mg.names() {
        ssh_channel_check(&t.ws, conn, &t.env(), &password(SSH_PASSWORD), name, 27017)
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

fn tls_uri(host: &str, extra: &str) -> String {
    format!(
        "mongodb://{host}:27017/app?tls=true&tlsCAFile=ca.pem&serverSelectionTimeoutMS=4000{extra}"
    )
}

#[tokio::test]
async fn a_single_node_answers_over_tls_and_ssh_closes_with_the_session() {
    let net = ssh_net().await;
    let mg = mongo_on(net.network(), 1).await;
    mg.eval(0, "db.getSiblingDB('app').ssh.insertOne({k: 'tunneled'})")
        .await;
    let t = mg_tunnel(&net, &tls_uri(mg.name(0), ""), mg.ca_pem());
    let mut s = t.session("mg", None).await.unwrap();
    assert_eq!(
        read(&mut s, "db.ssh.find({k: 'tunneled'})")
            .await
            .rows
            .len(),
        1
    );
    // Rootless podman forwards from the container's own address, so one link can count twice.
    assert!(
        net.ssh_sessions().await > 0,
        "no SSH link while the session is open"
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
async fn a_host_the_certificate_does_not_name_fails() {
    let net = ssh_net().await;
    let mg = mongo_on(net.network(), 1).await;
    let t = mg_tunnel(&net, &tls_uri(mg.ip(0), ""), mg.ca_pem());
    let err = t
        .session("mg", None)
        .await
        .err()
        .expect("a wrong TLS name connected");
    let text = err.to_string().to_lowercase();
    assert!(text.contains("certificate"), "{text}");
}

#[tokio::test]
async fn a_secondary_the_seed_reports_is_reached_through_the_tunnel() {
    let net = ssh_net().await;
    let mg = mongo_on(net.network(), 2).await;
    mg.eval(
        0,
        "db.getSiblingDB('app').ssh.insertOne({k: 'r'}, {writeConcern: {w: 2}})",
    )
    .await;
    let uri = tls_uri(mg.name(0), "&replicaSet=rs0&readPreference=secondary");
    let t = mg_tunnel(&net, &uri, mg.ca_pem());
    let mut s = t.session("mg", None).await.unwrap();
    assert_eq!(read(&mut s, "db.ssh.find()").await.rows.len(), 1);
}

#[tokio::test]
async fn the_sessions_socks_port_refuses_a_wrong_login() {
    let net = ssh_net().await;
    let mg = mongo_on(net.network(), 1).await;
    let t = mg_tunnel(&net, &tls_uri(mg.name(0), ""), mg.ca_pem());
    let s = t.session("mg", None).await.unwrap();
    let port = s
        .mongo_proxy_port()
        .expect("a tunnelled MongoDB session has a SOCKS port");
    let mut raw = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    raw.write_all(&[5, 1, 2]).await.unwrap();
    let mut chosen = [0u8; 2];
    raw.read_exact(&mut chosen).await.unwrap();
    assert_eq!(chosen, [5, 2]);
    let mut login: Vec<u8> = vec![1, 6];
    login.extend_from_slice(b"dbento");
    login.push(5);
    login.extend_from_slice(b"wrong");
    raw.write_all(&login).await.unwrap();
    let mut rest = Vec::new();
    raw.read_to_end(&mut rest).await.unwrap();
    assert_eq!(rest, [1, 1]);
}
