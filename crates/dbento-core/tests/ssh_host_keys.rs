//! The host-key gate against the fixture: probe, trust and forget, and every refusal
//! (changed of the same or another algorithm, revoked, unreadable) from both files.
use crate::ssh_support::{password, password_login, scalar, tunnel};
use dbento_core::{
    ConnectError, HostKeyProblem, HostKeyStatus, forget_host_key, probe_host_key, trust_host_key,
};
use dbento_testkit::{SSH_PASSWORD, ssh_net};

const B: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILY84+Y52dK/APwNyrey10YmkTK3XSb2Yat+Dg2fiIr5";
const C: &str = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBOmyhsyFCAd8Gt+y7BUI2rA3ox6iEgcB+GL43Ke1tTam6G5RvUIgWAfKTIbKA60M9w+Jd2eFubQ9LNBd4FEfDNw=";

#[tokio::test]
async fn an_unknown_key_is_probed_trusted_and_forgotten() {
    let net = ssh_net().await;
    let t = tunnel(&net, &password_login(), "tls = \"off\"");
    let conn = t.ws.declared("pg").unwrap();
    let key = match probe_host_key(&t.ws, conn, &t.env()).await.unwrap() {
        HostKeyStatus::Unknown(key) => key,
        other @ (HostKeyStatus::Trusted | HostKeyStatus::Changed { .. } | _) => panic!("{other:?}"),
    };
    assert_eq!(key.fingerprint(), net.fingerprint());
    assert_eq!((key.host(), key.port()), ("127.0.0.1", net.ssh_port()));
    assert!(
        !t.paths().ssh_known_hosts.exists(),
        "a probe trusts nothing"
    );
    trust_host_key(&t.paths(), &key).unwrap();
    let status = probe_host_key(&t.ws, conn, &t.env()).await.unwrap();
    assert_eq!(status, HostKeyStatus::Trusted);
    let mut s = t
        .connect(&net, t.env(), password(SSH_PASSWORD))
        .await
        .unwrap();
    assert_eq!(scalar(&mut s, "SELECT 1").await, "1");
    drop(s);
    assert_eq!(
        forget_host_key(&t.paths(), "127.0.0.1", net.ssh_port()).unwrap(),
        1
    );
    let status = probe_host_key(&t.ws, conn, &t.env()).await.unwrap();
    assert!(matches!(status, HostKeyStatus::Unknown(_)), "{status:?}");
    let ssh_dir = std::fs::read_dir(t.home.path().join(".ssh")).unwrap();
    assert_eq!(ssh_dir.count(), 0, "~/.ssh is never written");
}

#[tokio::test]
async fn changed_revoked_and_unreadable_keys_refuse_from_either_file() {
    let net = ssh_net().await;
    let t = tunnel(&net, &password_login(), "tls = \"off\"");
    let conn = t.ws.declared("pg").unwrap();
    let id = format!("[127.0.0.1]:{}", net.ssh_port());
    let fixture = net.host_key().to_owned();
    let user_file = t.home.path().join(".ssh/known_hosts");
    let own = t.paths().ssh_known_hosts;
    let cases = [
        (
            String::new(),
            format!("# mine\n{id} {B}\n"),
            HostKeyProblem::Changed,
            &own,
            2,
        ),
        (
            format!("{id} {C}\n"),
            String::new(),
            HostKeyProblem::Changed,
            &user_file,
            1,
        ),
        (
            format!("@revoked * {fixture}\n"),
            format!("{id} {fixture}\n"),
            HostKeyProblem::Revoked,
            &user_file,
            1,
        ),
        (
            String::new(),
            format!("{id} ssh-ed25519 !!!\n"),
            HostKeyProblem::Unreadable,
            &own,
            1,
        ),
    ];
    for (user, mine, problem, file, line) in cases {
        std::fs::write(&user_file, &user).unwrap();
        std::fs::write(&own, &mine).unwrap();
        match probe_host_key(&t.ws, conn, &t.env()).await.unwrap() {
            HostKeyStatus::Changed {
                problem: p,
                file: f,
                line: l,
                fingerprint,
                ..
            } => {
                assert_eq!((p, &f, l), (problem, file, line), "{user}{mine}");
                assert_eq!(fingerprint, net.fingerprint());
            }
            other @ (HostKeyStatus::Trusted | HostKeyStatus::Unknown(_) | _) => {
                panic!("{user}{mine}: {other:?}")
            }
        }
        match t.connect(&net, t.env(), password(SSH_PASSWORD)).await {
            Err(ConnectError::SshHostKeyChanged {
                problem: p,
                file: f,
                line: l,
                ..
            }) => {
                assert_eq!((p, &f, l), (problem, file, line), "{user}{mine}");
            }
            other => panic!("{user}{mine}: {:?}", other.err()),
        }
        assert_eq!(
            std::fs::read_to_string(&own).unwrap(),
            mine,
            "a refusal rewrites nothing"
        );
        assert_eq!(std::fs::read_to_string(&user_file).unwrap(), user);
    }
}
