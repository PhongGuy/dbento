//! The SSH link against the sshd fixture with password auth: channels, the unknown-key
//! gate, and failures that name the SSH side.
use std::path::{Path, PathBuf};

use crate::ssh_support::{password, password_login, tunnel};
use dbento_core::{
    ConnectError, SshEnv, SshHome, SshSecrets, StatePaths, Workspace, ssh_channel_check,
};
use dbento_testkit::{SSH_NOFWD_USER, SSH_PASSWORD, ssh_net};

#[tokio::test]
async fn a_trusted_password_login_opens_a_channel_to_the_database() {
    let net = ssh_net().await;
    let t = tunnel(&net, &password_login(), "tls = \"off\"");
    t.trust(&net);
    t.channel(&password(SSH_PASSWORD), net.db_host(), net.db_port())
        .await
        .unwrap();
}

#[tokio::test]
async fn an_unknown_host_key_refuses_and_writes_nothing() {
    let net = ssh_net().await;
    let t = tunnel(&net, &password_login(), "tls = \"off\"");
    match t
        .channel(&password(SSH_PASSWORD), net.db_host(), net.db_port())
        .await
    {
        Err(ConnectError::SshHostKeyUnknown { host, port, key }) => {
            assert_eq!((host.as_str(), port), ("127.0.0.1", net.ssh_port()));
            assert_eq!(key.fingerprint(), net.fingerprint());
        }
        other => panic!("{other:?}"),
    }
    assert!(!t.paths().ssh_known_hosts.exists());
}

#[tokio::test]
async fn a_wrong_or_missing_password_is_an_ssh_auth_error() {
    let net = ssh_net().await;
    let t = tunnel(&net, &password_login(), "tls = \"off\"");
    t.trust(&net);
    match t
        .channel(&password("wrong"), net.db_host(), net.db_port())
        .await
    {
        Err(ConnectError::SshAuth { message }) => {
            assert!(message.contains("password"), "{message}")
        }
        other => panic!("{other:?}"),
    }
    match t
        .channel(&SshSecrets::default(), net.db_host(), net.db_port())
        .await
    {
        Err(ConnectError::SshAuth { message }) => {
            assert!(
                message.contains("dbento secret set pg --ssh-password"),
                "{message}"
            );
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_refused_or_unreachable_channel_is_a_tunnel_error() {
    let net = ssh_net().await;
    let nofwd = format!("user = \"{SSH_NOFWD_USER}\"\nauth = \"password\"\n");
    let t = tunnel(&net, &nofwd, "tls = \"off\"");
    t.trust(&net);
    match t
        .channel(&password(SSH_PASSWORD), net.db_host(), net.db_port())
        .await
    {
        Err(e @ ConnectError::SshTunnel { .. }) => {
            assert!(e.to_string().contains(net.db_host()), "{e}");
        }
        other => panic!("{other:?}"),
    }
    let t = tunnel(&net, &password_login(), "tls = \"off\"");
    t.trust(&net);
    match t
        .channel(&password(SSH_PASSWORD), "no-such-host.invalid", 5432)
        .await
    {
        Err(e @ ConnectError::SshTunnel { .. }) => {
            assert!(e.to_string().contains("no-such-host.invalid:5432"), "{e}");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_closed_ssh_port_names_the_ssh_address_not_the_database() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let text = format!(
        "dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"db.internal\"\ndatabase = \"d\"\nuser = \"u\"\ntls = \"off\"\n[connections.pg.ssh]\nhost = \"127.0.0.1\"\nport = {port}\nuser = \"u\"\nauth = \"password\"\n"
    );
    let ws = Workspace::from_text(PathBuf::from("/w"), Path::new("/w/dbento.toml"), &text).unwrap();
    let (home, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let env = SshEnv::new(
        SshHome::new(home.path(), "tester"),
        StatePaths::in_dir(state.path()).ssh_known_hosts,
    );
    let conn = ws.declared("pg").unwrap();
    match ssh_channel_check(&ws, conn, &env, &password("x"), "db.internal", 5432).await {
        Err(e @ ConnectError::SshConnect { .. }) => {
            let ConnectError::SshConnect { address, .. } = &e else {
                unreachable!()
            };
            assert_eq!(address, &format!("127.0.0.1:{port}"));
            assert!(!e.to_string().contains("db.internal"), "{e}");
        }
        other => panic!("{other:?}"),
    }
}
