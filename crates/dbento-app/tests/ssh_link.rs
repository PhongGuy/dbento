//! The app's SSH plumbing: the context every connect carries, the SSH secrets a connect gets
//! and the fixes SSH failures show.
use dbento_app::host_keys::{HostKeyAnswer, HostKeyPrompt};
use dbento_app::ssh::SshContext;
use dbento_core::{HostKey, Secret, SessionOptions, SshEnv, SshHome, SshSecrets, StatePaths};
use std::path::Path;

/// A context over an empty home and state folder under `dir`, with no agent.
fn ctx(dir: &Path) -> SshContext {
    let state = StatePaths::in_dir(dir);
    let env = SshEnv::new(
        SshHome::new(dir.join("home"), "tester"),
        state.ssh_known_hosts.clone(),
    );
    SshContext::new(Some(env), Some(state))
}

/// A throwaway Ed25519 public key.
const KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAICqzCt/eLjiYnme5P1CYozYfgd3otQ6E+HI5N0RPYe4m";

#[test]
fn host_keys_cross_threads() {
    fn movable<T: Clone + Send + Sync + 'static>() {}
    movable::<HostKey>();
}

#[test]
fn the_context_hands_core_its_env_and_the_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path());
    let mut secrets = SshSecrets::default();
    secrets.password = Some(Secret::new("pw"));
    let mut options = SessionOptions::default();
    c.apply(&mut options, secrets);
    assert_eq!(options.ssh_env.as_ref(), c.env.as_ref(), "the env");
    assert_eq!(
        options.ssh_secrets.password.as_ref().map(Secret::expose),
        Some("pw")
    );
    assert_eq!(
        c.home().map(SshHome::dir),
        Some(dir.path().join("home").as_path()),
        "home"
    );
}

#[tokio::test]
async fn a_context_without_ssh_has_no_home_and_asks_nobody() {
    let c = SshContext::none();
    assert!(
        c.env.is_none() && c.state.is_none() && c.home().is_none(),
        "nothing"
    );
    let key = HostKey::from_openssh("jump", 22, KEY).unwrap();
    let prompt = HostKeyPrompt {
        connection: "tun".into(),
        host: "jump".into(),
        port: 22,
        fingerprint: key.fingerprint(),
        key,
    };
    assert_eq!(
        c.gate.wait(prompt).await,
        HostKeyAnswer::Unasked,
        "refused at once"
    );
}
