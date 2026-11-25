//! Cancel through the tunnel in both TLS modes: the request travels a fresh channel, and
//! the session survives it.
use std::time::Duration;

use crate::ssh_support::{password, password_login, scalar, tunnel_with};
use crate::support::run;
use dbento_core::{ExecuteError, Limits};
use dbento_testkit::{SSH_PASSWORD, ssh_net};

#[tokio::test]
async fn both_tls_modes_cancel_through_the_tunnel() {
    let net = ssh_net().await;
    let ca = net.ca_pem().to_owned();
    for pg_extra in ["tls = \"off\"", "tls = \"verify\"\ntls_ca = \"ca.pem\""] {
        let t = tunnel_with(
            &net,
            &password_login(),
            pg_extra,
            &[("ca.pem", ca.as_str())],
        );
        t.trust(&net);
        let mut s = t
            .connect(&net, t.env(), password(SSH_PASSWORD))
            .await
            .unwrap();
        let cancel = s.cancel_handle();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            cancel.cancel();
        });
        let r = tokio::time::timeout(
            Duration::from_secs(20),
            run(&mut s, "SELECT pg_sleep(30)", Limits::default()),
        )
        .await
        .expect("the cancel never reached the server through the tunnel");
        canceller.join().unwrap();
        assert!(
            matches!(r[0], Err(ExecuteError::Cancelled)),
            "{pg_extra}: {:?}",
            r[0].as_ref().err()
        );
        assert_eq!(
            scalar(&mut s, "SELECT 1").await,
            "1",
            "the session survives its cancel"
        );
    }
}
