//! The sshd fixture alone: loopback-only ports, its host key and its client keys.
use dbento_testkit::ssh_net;

#[tokio::test]
async fn the_fixture_publishes_on_loopback_and_hands_out_its_keys() {
    let net = ssh_net().await;
    let ips = net.published_ips().await;
    assert!(
        ips.len() >= 2 && ips.iter().all(|ip| ip == "127.0.0.1"),
        "{ips:?}"
    );
    assert!(
        net.host_key().starts_with("ssh-ed25519 "),
        "{}",
        net.host_key()
    );
    assert_eq!(net.host_key().split(' ').count(), 2, "{}", net.host_key());
    assert!(
        net.fingerprint().starts_with("SHA256:"),
        "{}",
        net.fingerprint()
    );
    for name in [
        "ed25519",
        "ed25519_enc",
        "rsa",
        "stranger1",
        "stranger2",
        "stranger3",
    ] {
        let key = std::fs::read_to_string(net.client_key(name)).unwrap();
        assert!(
            key.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----"),
            "{name}"
        );
        assert!(
            net.client_key(name).with_extension("pub").exists(),
            "{name}.pub"
        );
    }
    assert_eq!(net.ssh_sessions().await, 0);
    assert_ne!(net.db_host(), "127.0.0.1");
}
