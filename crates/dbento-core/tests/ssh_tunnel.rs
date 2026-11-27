//! Postgres sessions through the sshd fixture: plain and verified TLS queries, the unknown
//! key through `connect`, the SSH connection ending with its session, and connections a
//! tunnel does not serve.
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ssh_support::{password, password_login, scalar, tunnel, tunnel_with};
use dbento_core::{ConnectError, SessionOptions, SshEnv, SshHome, StatePaths, Workspace, connect};
use dbento_testkit::{SSH_PASSWORD, ssh_net, ssh_net_with_cert_name};

#[tokio::test]
async fn a_trusted_tunnel_runs_a_query_and_closes_with_its_session() {
    let net = ssh_net().await;
    let t = tunnel(&net, &password_login(), "tls = \"off\"");
    match t.connect(&net, t.env(), password(SSH_PASSWORD)).await {
        Err(ConnectError::SshHostKeyUnknown { key, .. }) => {
            assert_eq!(key.fingerprint(), net.fingerprint());
        }
        other => panic!("{:?}", other.err()),
    }
    t.trust(&net);
    let mut s = t
        .connect(&net, t.env(), password(SSH_PASSWORD))
        .await
        .unwrap();
    assert_eq!(
        scalar(&mut s, "SELECT current_database()").await,
        net.pg().database()
    );
    assert_eq!(net.ssh_sessions().await, 1);
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
async fn verified_tls_through_the_tunnel_checks_the_database_name() {
    let net = ssh_net().await;
    let ca = net.ca_pem().to_owned();
    let pg_extra = "tls = \"verify\"\ntls_ca = \"ca.pem\"";
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
    let sql = "SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()";
    assert_eq!(scalar(&mut s, sql).await, "t");
    drop(s);
    let elsewhere = ssh_net_with_cert_name("elsewhere.invalid").await;
    let ca = elsewhere.ca_pem().to_owned();
    let t = tunnel_with(
        &elsewhere,
        &password_login(),
        pg_extra,
        &[("ca.pem", ca.as_str())],
    );
    t.trust(&elsewhere);
    match t.connect(&elsewhere, t.env(), password(SSH_PASSWORD)).await {
        Err(ConnectError::Tls { host, .. }) => assert_eq!(host, elsewhere.db_host()),
        other => panic!("{:?}", other.err()),
    }
}

/// `text` as a workspace, with a temporary SSH home and state folder for its tunnel.
async fn connect_offline(text: &str, name: &str) -> Option<ConnectError> {
    let ws = Workspace::from_text(PathBuf::from("/w"), Path::new("/w/dbento.toml"), text).unwrap();
    let (home, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut opts = SessionOptions::default();
    let known = StatePaths::in_dir(state.path()).ssh_known_hosts;
    opts.ssh_env = Some(SshEnv::new(SshHome::new(home.path(), "tester"), known));
    connect(&ws, ws.declared(name).unwrap(), None, opts)
        .await
        .err()
}

// The workspace refuses every engine without a TCP server at load, so only a build lacking an
// engine's feature still reaches the session's refusal; a build with it dials the SSH server.
#[tokio::test]
async fn a_tunnelled_engine_this_build_lacks_refuses_before_any_network() {
    let text = "dbento = 1\n[connections.m]\nengine = \"surrealdb\"\nhost = \"db.internal\"\nnamespace = \"n\"\ndatabase = \"d\"\ntls = \"off\"\n[connections.m.ssh]\nhost = \"127.0.0.1\"\nport = 1\n";
    match connect_offline(text, "m").await {
        Some(e @ ConnectError::SshUnsupported { .. }) if !cfg!(feature = "surrealdb") => {
            assert!(e.to_string().contains("surrealdb"), "{e}");
        }
        Some(ConnectError::SshConnect { address, .. }) if cfg!(feature = "surrealdb") => {
            assert_eq!(address, "127.0.0.1:1");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_tunnel_without_a_database_host_is_refused_when_the_workspace_loads() {
    // No default host exists for the SSH server to reach, so the workspace refuses it.
    let text = "dbento = 1\n[connections.p]\nengine = \"postgres\"\ndatabase = \"d\"\nuser = \"u\"\n[connections.p.ssh]\nhost = \"127.0.0.1\"\nport = 1\n";
    let Err(e) = Workspace::from_text(PathBuf::from("/w"), Path::new("/w/dbento.toml"), text)
    else {
        panic!("a tunnel without a database host loaded");
    };
    assert!(e.to_string().contains("`host`"), "{e}");
}
