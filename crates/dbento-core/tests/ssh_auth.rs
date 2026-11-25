//! SSH authentication through the fixture: key files and the passphrase slot, the
//! in-process agent, the order tried without `auth`, the failure listing every try, and
//! the cap on concurrent handshakes per target.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;

use crate::ssh_support::{passphrase, password, scalar, tunnel};
use dbento_core::{
    ConnectError, SessionOptions, SshEnv, SshHome, SshSecrets, StatePaths, Workspace, connect,
};
use dbento_testkit::{SSH_KEY_PASSPHRASE, SSH_KEY_USER, SSH_PASSWORD, SSH_USER, SshNet, ssh_net};

/// `ssh` for the key user with `auth` (a whole line, or empty) and `identity_file`.
fn key_login(auth: &str, identity: &Path) -> String {
    format!(
        "user = \"{SSH_KEY_USER}\"\n{auth}identity_file = \"{}\"\n",
        identity.display()
    )
}

#[tokio::test]
async fn key_files_log_in_and_an_encrypted_one_needs_its_passphrase_slot() {
    let net = ssh_net().await;
    for (name, secrets) in [
        ("ed25519", SshSecrets::default()),
        ("rsa", SshSecrets::default()),
        ("ed25519_enc", passphrase(SSH_KEY_PASSPHRASE)),
    ] {
        let t = tunnel(
            &net,
            &key_login("auth = \"key\"\n", &net.client_key(name)),
            "tls = \"off\"",
        );
        t.trust(&net);
        let mut s = t
            .connect(&net, t.env(), secrets)
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(scalar(&mut s, "SELECT 1").await, "1");
    }
    let enc = net.client_key("ed25519_enc");
    let t = tunnel(&net, &key_login("auth = \"key\"\n", &enc), "tls = \"off\"");
    t.trust(&net);
    match t.connect(&net, t.env(), SshSecrets::default()).await {
        Err(ConnectError::SshAuth { message }) => {
            assert!(
                message.contains("dbento secret set pg --ssh-key-passphrase"),
                "{message}"
            );
        }
        other => panic!("{:?}", other.err()),
    }
}

#[tokio::test]
async fn without_identity_file_the_default_keys_in_the_ssh_home_are_tried() {
    let net = ssh_net().await;
    let t = tunnel(
        &net,
        &format!("user = \"{SSH_KEY_USER}\"\n"),
        "tls = \"off\"",
    );
    std::fs::copy(
        net.client_key("ed25519"),
        t.home.path().join(".ssh/id_ed25519"),
    )
    .unwrap();
    t.trust(&net);
    let mut s = t
        .connect(&net, t.env(), SshSecrets::default())
        .await
        .unwrap();
    assert_eq!(scalar(&mut s, "SELECT 1").await, "1");
}

#[tokio::test]
async fn without_auth_the_password_comes_last_and_a_failure_lists_every_try() {
    let net = ssh_net().await;
    let login = format!(
        "user = \"{SSH_USER}\"\nidentity_file = \"{}\"\n",
        net.client_key("stranger1").display()
    );
    let t = tunnel(&net, &login, "tls = \"off\"");
    t.trust(&net);
    let mut s = t
        .connect(&net, t.env(), password(SSH_PASSWORD))
        .await
        .unwrap();
    assert_eq!(scalar(&mut s, "SELECT 1").await, "1");
    drop(s);
    match t.connect(&net, t.env(), SshSecrets::default()).await {
        Err(ConnectError::SshAuth { message }) => {
            for word in [
                "agent",
                "stranger1",
                "password",
                "--ssh-password",
                "publickey",
            ] {
                assert!(message.contains(word), "{word}: {message}");
            }
        }
        other => panic!("{:?}", other.err()),
    }
}

/// An in-process agent on a temporary socket holding the fixture keys `names`, in order.
#[cfg(unix)]
async fn agent_holding(net: &SshNet, names: &[&str]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("agent.sock");
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    let incoming = futures_util::stream::unfold(listener, |l| async move {
        Some((l.accept().await.map(|(s, _)| s), l))
    });
    tokio::spawn(russh::keys::agent::server::serve(Box::pin(incoming), ()));
    let mut client = russh::keys::agent::client::AgentClient::connect_uds(&sock)
        .await
        .unwrap();
    for name in names {
        let key = russh::keys::load_secret_key(net.client_key(name), None).unwrap();
        client.add_identity(&key, &[]).await.unwrap();
    }
    (dir, sock)
}

#[cfg(unix)]
#[tokio::test]
async fn the_agent_logs_in_and_tries_the_identity_files_key_first() {
    let net = ssh_net().await;
    let agent_only = format!("user = \"{SSH_KEY_USER}\"\nauth = \"agent\"\n");
    let (_a, sock) = agent_holding(&net, &["ed25519"]).await;
    let t = tunnel(&net, &agent_only, "tls = \"off\"");
    t.trust(&net);
    let mut s = t
        .connect(&net, t.env().with_agent(&sock), SshSecrets::default())
        .await
        .unwrap();
    assert_eq!(scalar(&mut s, "SELECT 1").await, "1");
    drop(s);
    match t.connect(&net, t.env(), SshSecrets::default()).await {
        Err(ConnectError::SshAuth { message }) => assert!(message.contains("agent"), "{message}"),
        other => panic!("{:?}", other.err()),
    }
    // Only the public half sits beside the configured path, so the agent alone can sign;
    // three strangers tried first would spend sshd's MaxAuthTries of 3.
    let keys = tempfile::tempdir().unwrap();
    std::fs::copy(
        net.client_key("ed25519").with_extension("pub"),
        keys.path().join("id.pub"),
    )
    .unwrap();
    let (_b, crowded) =
        agent_holding(&net, &["stranger1", "stranger2", "stranger3", "ed25519"]).await;
    let t = tunnel(
        &net,
        &key_login("", &keys.path().join("id")),
        "tls = \"off\"",
    );
    t.trust(&net);
    let mut s = t
        .connect(&net, t.env().with_agent(&crowded), SshSecrets::default())
        .await
        .unwrap();
    assert_eq!(scalar(&mut s, "SELECT 1").await, "1");
}

#[tokio::test]
async fn at_most_four_handshakes_run_at_once_per_target() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&accepted);
    // A server that never sends its banner: every handshake hangs until the client leaves.
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buf = [0u8; 256];
                while matches!(sock.read(&mut buf).await, Ok(n) if n > 0) {}
            });
        }
    });
    let text = format!(
        "dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"db.internal\"\ndatabase = \"d\"\nuser = \"u\"\ntls = \"off\"\n[connections.pg.ssh]\nhost = \"127.0.0.1\"\nport = {port}\nuser = \"u\"\nauth = \"password\"\n"
    );
    let ws = Workspace::from_text(PathBuf::from("/w"), Path::new("/w/dbento.toml"), &text).unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".ssh")).unwrap();
    std::fs::write(
        home.path().join(".ssh/config"),
        "Host 127.0.0.1\n  ConnectTimeout 2\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let options = || {
        let mut o = SessionOptions::default();
        let env = SshHome::new(home.path(), "tester");
        o.ssh_env = Some(SshEnv::new(
            env,
            StatePaths::in_dir(state.path()).ssh_known_hosts,
        ));
        o
    };
    let conn = ws.declared("pg").unwrap();
    let started = Instant::now();
    let tries = (0..6).map(|_| connect(&ws, conn, None, options()));
    let (results, at_one_second) = tokio::join!(futures_util::future::join_all(tries), async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        accepted.load(Ordering::SeqCst)
    });
    assert_eq!(at_one_second, 4, "only four handshakes may start at once");
    for r in results {
        assert!(
            matches!(r, Err(ConnectError::SshConnect { .. })),
            "{:?}",
            r.err()
        );
    }
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "one timeout budget per connect"
    );
}
