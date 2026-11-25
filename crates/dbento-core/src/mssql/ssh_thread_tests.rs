//! An SSH channel opened and read on a current-thread runtime of its own thread, as the SQL
//! Server connection thread does, while the link runs on the runtime that opened it.
use std::future::Future;
use std::sync::Arc;

use dbento_testkit::{SSH_PASSWORD, SSH_USER, SshNet, ssh_net};
use tokio::io::AsyncReadExt as _;

use crate::ssh::SshLink;
use crate::{Secret, SshEnv, SshHome, SshSecrets, Workspace, resolve_ssh};

/// A link to the fixture as the password user, its host key trusted, nothing read from the
/// real home. The folder holds the workspace, the SSH home and the known_hosts file.
async fn link(net: &SshNet) -> (tempfile::TempDir, Arc<SshLink>) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("home/.ssh")).unwrap();
    let text = format!(
        "dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"{}\"\ndatabase = \"d\"\nuser = \"u\"\ntls = \"off\"\n[connections.pg.ssh]\nhost = \"127.0.0.1\"\nport = {}\nuser = \"{SSH_USER}\"\nauth = \"password\"\n",
        net.db_host(),
        net.ssh_port()
    );
    let toml = dir.path().join("dbento.toml");
    let ws = Workspace::from_text(dir.path().to_path_buf(), &toml, &text).unwrap();
    let conn = ws.declared("pg").unwrap();
    let known = dir.path().join("known_hosts");
    std::fs::write(
        &known,
        format!("[127.0.0.1]:{} {}\n", net.ssh_port(), net.host_key()),
    )
    .unwrap();
    let env = SshEnv::new(SshHome::new(dir.path().join("home"), "tester"), known);
    let resolved = resolve_ssh(conn, dir.path(), env.home()).unwrap();
    let secrets = SshSecrets {
        password: Some(Secret::new(SSH_PASSWORD)),
        ..SshSecrets::default()
    };
    let link = SshLink::open(conn, &resolved, &env, &secrets)
        .await
        .unwrap();
    (dir, Arc::new(link))
}

/// Runs `work` on a fresh current-thread runtime on its own thread, as `conn::start` does,
/// and returns its output after that runtime has been dropped.
fn on_own_thread<F, Fut, T>(work: F) -> T
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = T>,
    T: Send + 'static,
{
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(work())
    })
    .join()
    .unwrap()
}

/// The first eight bytes the bastion's own sshd sends down a channel aimed at its port 22.
async fn greeting(stream: &mut (impl tokio::io::AsyncRead + Unpin)) -> String {
    let mut head = [0u8; 8];
    stream.read_exact(&mut head).await.unwrap();
    String::from_utf8_lossy(&head).into_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn channels_open_and_read_on_a_thread_runtime() {
    let net = ssh_net().await;
    let (_dir, link) = link(&net).await;
    let on_thread = Arc::clone(&link);
    let got = tokio::task::spawn_blocking(move || {
        on_own_thread(move || async move {
            let mut a = on_thread.channel("localhost", 22).await.unwrap();
            let mut b = on_thread.channel("localhost", 22).await.unwrap();
            let both = (greeting(&mut a).await, greeting(&mut b).await);
            // Dropped on this runtime; the yield lets russh's close-on-drop task run.
            drop((a, b));
            tokio::task::yield_now().await;
            both
        })
    })
    .await
    .unwrap();
    assert_eq!(got, ("SSH-2.0-".to_owned(), "SSH-2.0-".to_owned()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_link_outlives_a_thread_runtime_that_dropped_its_channel() {
    let net = ssh_net().await;
    let (_dir, link) = link(&net).await;
    let on_thread = Arc::clone(&link);
    let first = tokio::task::spawn_blocking(move || {
        on_own_thread(move || async move {
            let mut s = on_thread.channel("localhost", 22).await.unwrap();
            // `s` drops as block_on ends, with no yield: the worst case for the close task.
            greeting(&mut s).await
        })
    })
    .await
    .unwrap();
    assert_eq!(first, "SSH-2.0-");
    let mut again = link.channel("localhost", 22).await.unwrap();
    assert_eq!(greeting(&mut again).await, "SSH-2.0-");
}
