//! MongoDB through the SSH bastion: the driver's SOCKS5 proxy reaching members by name.
#![cfg(feature = "mongodb")]

use crate::ssh_support::{Tunnel, engine_tunnel, password};
use dbento_core::ssh_channel_check;
use dbento_testkit::{SSH_PASSWORD, SshNet, mongo_on, published_ips, ssh_net};

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
