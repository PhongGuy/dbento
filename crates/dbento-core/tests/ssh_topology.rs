//! The sshd fixture's topology with russh alone: password and key logins, then a
//! direct-tcpip channel carrying a Postgres session to the database's network name.
use std::sync::Arc;

use dbento_testkit::{
    SSH_KEY_PASSPHRASE, SSH_KEY_USER, SSH_NOFWD_USER, SSH_PASSWORD, SSH_USER, SshNet, ssh_net,
};
use russh::client;
use russh::keys::{PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate, load_secret_key};

/// Accepts exactly the fixture's host key.
struct Pinned(PublicKey);

impl client::Handler for Pinned {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(matches!(key, PublicKeyOrCertificate::PublicKey { key, .. }
            if key.key_data() == self.0.key_data()))
    }
}

async fn login(net: &SshNet) -> client::Handle<Pinned> {
    let pinned = Pinned(PublicKey::from_openssh(net.host_key()).unwrap());
    let config = Arc::new(client::Config::default());
    client::connect(config, ("127.0.0.1", net.ssh_port()), pinned)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_channel_reaches_postgres_by_its_network_name() {
    let net = ssh_net().await;
    let mut ssh = login(&net).await;
    let auth = ssh
        .authenticate_password(SSH_USER, SSH_PASSWORD)
        .await
        .unwrap();
    assert!(auth.success(), "{auth:?}");
    let channel = ssh
        .channel_open_direct_tcpip(net.db_host(), u32::from(net.db_port()), "127.0.0.1", 0)
        .await
        .unwrap();
    let mut config = tokio_postgres::Config::new();
    config
        .user(net.pg().user())
        .password(net.pg().password())
        .dbname(net.pg().database());
    let (client, connection) = config
        .connect_raw(channel.into_stream(), tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(connection);
    let row = client
        .query_one("SELECT current_database()", &[])
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), net.pg().database());
}

#[tokio::test]
async fn the_key_user_logs_in_with_each_fixture_key_and_never_by_password() {
    let net = ssh_net().await;
    for (name, phrase) in [
        ("ed25519", None),
        ("ed25519_enc", Some(SSH_KEY_PASSPHRASE)),
        ("rsa", None),
    ] {
        let mut ssh = login(&net).await;
        let key = load_secret_key(net.client_key(name), phrase).unwrap();
        let hash = ssh.best_supported_rsa_hash().await.unwrap().flatten();
        let auth = ssh
            .authenticate_publickey(
                SSH_KEY_USER,
                PrivateKeyWithHashAlg::new(Arc::new(key), hash),
            )
            .await
            .unwrap();
        assert!(auth.success(), "{name}: {auth:?}");
    }
    assert!(load_secret_key(net.client_key("ed25519_enc"), None).is_err());
    let mut ssh = login(&net).await;
    let auth = ssh
        .authenticate_password(SSH_KEY_USER, SSH_PASSWORD)
        .await
        .unwrap();
    assert!(!auth.success(), "the key user must not accept a password");
    let mut ssh = login(&net).await;
    let stranger = load_secret_key(net.client_key("stranger1"), None).unwrap();
    let auth = ssh
        .authenticate_publickey(
            SSH_KEY_USER,
            PrivateKeyWithHashAlg::new(Arc::new(stranger), None),
        )
        .await
        .unwrap();
    assert!(!auth.success(), "a stranger key must be refused");
}

#[tokio::test]
async fn the_nofwd_user_is_refused_a_channel() {
    let net = ssh_net().await;
    let mut ssh = login(&net).await;
    let auth = ssh
        .authenticate_password(SSH_NOFWD_USER, SSH_PASSWORD)
        .await
        .unwrap();
    assert!(auth.success(), "{auth:?}");
    let refused = ssh
        .channel_open_direct_tcpip(net.db_host(), u32::from(net.db_port()), "127.0.0.1", 0)
        .await;
    assert!(
        matches!(refused, Err(russh::Error::ChannelOpenFailure(_))),
        "{:?}",
        refused.err()
    );
}
