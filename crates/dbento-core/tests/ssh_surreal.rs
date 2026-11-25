//! SurrealDB through the SSH bastion: the WebSocket upgrade and the RPC ride one channel.
use std::time::Duration;

use crate::ssh_support::engine_tunnel;
use crate::support::run;
use dbento_core::{Flavor, Limits, Secret};
use dbento_testkit::{published_ips, ssh_net, surreal_on};

#[tokio::test]
async fn surrealdb_answers_through_the_tunnel() {
    let net = ssh_net().await;
    let (db, on) = surreal_on(net.network()).await;
    let ips = published_ips(&on.id).await;
    assert!(
        !ips.is_empty() && ips.iter().all(|ip| ip == "127.0.0.1"),
        "{ips:?}"
    );
    let body = format!(
        "engine = \"surrealdb\"\nhost = \"{}\"\nport = 8000\nnamespace = \"{}\"\ndatabase = \"{}\"\ntls = \"off\"\nuser = \"root\"\nauth_level = \"root\"",
        on.alias,
        db.namespace(),
        db.database()
    );
    let t = engine_tunnel(&net, "sr", &body, &[]);
    t.trust(&net);
    let mut s = t
        .session("sr", Some(Secret::new(db.password())))
        .await
        .unwrap();
    assert_eq!(s.server().unwrap().flavor, Flavor::SurrealDb);
    let ran = run(&mut s, "RETURN 1", Limits::default()).await;
    assert!(ran[0].is_ok(), "{:?}", ran[0].as_ref().err());
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
