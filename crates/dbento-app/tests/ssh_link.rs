//! The app's SSH plumbing: the context every connect carries, the SSH secrets a connect gets
//! and the fixes SSH failures show.
use dbento_app::host_keys::{HostKeyAnswer, HostKeyPrompt};
use dbento_app::ssh::{
    AUTH_HINT, CONNECT_HINT, NO_HOME, SshContext, TUNNEL_HINT, UNKNOWN_KEY_HINT, connect_message,
    not_trusted, ssh_hint, ssh_secrets, unknown_key,
};
use dbento_core::{
    ConnectError, HostKey, HostKeyProblem, MemoryStore, Secret, SecretError, SecretKey,
    SecretStore, SessionOptions, SshEnv, SshHome, SshSecrets, StatePaths, Workspace,
    ssh_password_env_var,
};
use std::path::{Path, PathBuf};

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

const TUN: &str = "dbento = 1\n[connections.tun]\nengine = \"postgres\"\nhost = \"db\"\ndatabase = \"d\"\nuser = \"u\"\n[connections.tun.ssh]\nhost = \"jump\"\nuser = \"ops\"\n[connections.plain]\nengine = \"postgres\"\nhost = \"db\"\ndatabase = \"d\"\nuser = \"u\"\n";

fn ws() -> Workspace {
    Workspace::from_text(PathBuf::from("/w"), Path::new("/w/dbento.toml"), TUN).unwrap()
}

fn changed(file: PathBuf, port: u16, problem: HostKeyProblem) -> ConnectError {
    ConnectError::SshHostKeyChanged {
        host: "jump".into(),
        port,
        fingerprint: "SHA256:new".into(),
        file,
        line: 3,
        problem,
    }
}

/// A store that fails the test when anything asks it.
struct Untouchable;
impl SecretStore for Untouchable {
    fn get(&self, _: &SecretKey) -> Result<Option<Secret>, SecretError> {
        panic!("the store was read")
    }
    fn set(&self, _: &SecretKey, _: &Secret) -> Result<(), SecretError> {
        panic!("the store was written")
    }
    fn delete(&self, _: &SecretKey) -> Result<(), SecretError> {
        panic!("the store was written")
    }
}

#[test]
fn a_tunnelled_connect_gets_the_pinned_password_and_the_passphrase() {
    let dir = tempfile::tempdir().unwrap();
    let (w, store) = (ws(), MemoryStore::new());
    store
        .set(
            &SecretKey::ssh_password("/w", "tun", "ops@jump:22"),
            &Secret::new("pw"),
        )
        .unwrap();
    store
        .set(
            &SecretKey::ssh_key_passphrase("/w", "tun"),
            &Secret::new("pp"),
        )
        .unwrap();
    let conn = w.connection("tun").unwrap();
    let got = ssh_secrets(&w, &conn, &ctx(dir.path()), &store, &|_| None).unwrap();
    assert_eq!(
        got.password.as_ref().map(Secret::expose),
        Some("pw"),
        "password"
    );
    assert_eq!(
        got.key_passphrase.as_ref().map(Secret::expose),
        Some("pp"),
        "passphrase"
    );
}

#[test]
fn the_ssh_password_falls_back_to_its_variable() {
    let dir = tempfile::tempdir().unwrap();
    let w = ws();
    let conn = w.connection("tun").unwrap();
    let var = ssh_password_env_var("tun");
    let env = move |v: &str| (v == var).then(|| "from-env".to_owned());
    let got = ssh_secrets(&w, &conn, &ctx(dir.path()), &MemoryStore::new(), &env).unwrap();
    assert_eq!(
        got.password.as_ref().map(Secret::expose),
        Some("from-env"),
        "variable"
    );
}

#[test]
fn a_connection_without_ssh_never_asks_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let w = ws();
    let conn = w.connection("plain").unwrap();
    let got = ssh_secrets(&w, &conn, &ctx(dir.path()), &Untouchable, &|_| None).unwrap();
    assert!(
        got.password.is_none() && got.key_passphrase.is_none(),
        "nothing"
    );
}

#[test]
fn without_a_home_a_tunnelled_connect_is_refused() {
    let w = ws();
    let conn = w.connection("tun").unwrap();
    let e = ssh_secrets(&w, &conn, &SshContext::none(), &Untouchable, &|_| None).unwrap_err();
    assert_eq!(e, NO_HOME, "the refusal");
}

#[test]
fn a_changed_key_in_dbentos_file_points_at_forget() {
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path());
    let e = changed(
        StatePaths::in_dir(dir.path()).ssh_known_hosts,
        22,
        HostKeyProblem::Changed,
    );
    assert_eq!(
        ssh_hint("tun", &e, &c).as_deref(),
        Some(
            "The SSH server's host key changed. If it was rotated on purpose, run `dbento ssh forget tun`, then connect again to check the new fingerprint."
        )
    );
}

#[test]
fn a_changed_key_in_another_file_points_at_ssh_keygen() {
    let dir = tempfile::tempdir().unwrap();
    let e = changed(
        PathBuf::from("/home/u/.ssh/known_hosts"),
        2222,
        HostKeyProblem::Changed,
    );
    assert_eq!(
        ssh_hint("tun", &e, &ctx(dir.path())).as_deref(),
        Some(
            "The SSH server's host key changed in /home/u/.ssh/known_hosts. If it was rotated on purpose, remove the old entry with `ssh-keygen -R '[jump]:2222' -f /home/u/.ssh/known_hosts`, then connect again."
        )
    );
}

#[test]
fn revoked_and_unreadable_keys_never_point_at_forget() {
    let dir = tempfile::tempdir().unwrap();
    let file = PathBuf::from("/s/ssh_known_hosts");
    let c = ctx(dir.path());
    assert_eq!(
        ssh_hint(
            "tun",
            &changed(file.clone(), 22, HostKeyProblem::Revoked),
            &c
        )
        .as_deref(),
        Some(
            "The SSH server's host key is revoked in /s/ssh_known_hosts. Do not connect until the server's administrator replaces it."
        )
    );
    assert_eq!(
        ssh_hint("tun", &changed(file, 22, HostKeyProblem::Unreadable), &c).as_deref(),
        Some(
            "Line 3 of /s/ssh_known_hosts cannot be read. Fix or remove that line, then connect again."
        )
    );
}

#[test]
fn connect_auth_and_tunnel_failures_have_their_fixes() {
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path());
    let connect = ConnectError::SshConnect {
        address: "127.0.0.1:9".into(),
        message: "refused".into(),
    };
    let auth = ConnectError::SshAuth {
        message: "no method left".into(),
    };
    let tunnel = ConnectError::SshTunnel {
        message: "forwarding off".into(),
    };
    let unsupported = ConnectError::SshUnsupported {
        message: "Match".into(),
    };
    assert_eq!(ssh_hint("tun", &connect, &c).as_deref(), Some(CONNECT_HINT));
    assert_eq!(ssh_hint("tun", &auth, &c).as_deref(), Some(AUTH_HINT));
    assert_eq!(ssh_hint("tun", &tunnel, &c).as_deref(), Some(TUNNEL_HINT));
    assert_eq!(
        ssh_hint("tun", &unsupported, &c),
        None,
        "its message says it all"
    );
}

#[test]
fn an_unknown_key_becomes_a_prompt_with_its_fingerprint() {
    let key = HostKey::from_openssh("jump", 2222, KEY).unwrap();
    let e = ConnectError::SshHostKeyUnknown {
        host: "jump".into(),
        port: 2222,
        key: key.clone(),
    };
    let p = unknown_key("tun", &e).unwrap();
    assert_eq!(
        (
            p.connection.as_str(),
            p.host.as_str(),
            p.port,
            p.fingerprint.clone()
        ),
        ("tun", "jump", 2222, key.fingerprint())
    );
    assert_eq!(p.key, key, "the key trusting stores");
    let other = ConnectError::SshAuth {
        message: "no".into(),
    };
    assert!(
        unknown_key("tun", &other).is_none(),
        "only an unknown key asks"
    );
}

#[test]
fn an_unknown_key_nobody_was_asked_about_names_the_trust_command() {
    let dir = tempfile::tempdir().unwrap();
    let key = HostKey::from_openssh("jump", 22, KEY).unwrap();
    let e = ConnectError::SshHostKeyUnknown {
        host: "jump".into(),
        port: 22,
        key,
    };
    let fix = UNKNOWN_KEY_HINT.replace("{conn}", "tun");
    assert_eq!(ssh_hint("tun", &e, &ctx(dir.path())), Some(fix.clone()));
    assert!(fix.contains("`dbento ssh trust tun`"), "{fix}");
}

#[test]
fn a_declined_or_conflicting_question_says_what_to_do() {
    let declined = not_trusted("tun", "jump", 22, HostKeyAnswer::Declined);
    assert_eq!(
        declined.message,
        "the SSH host key of jump:22 was not trusted"
    );
    assert_eq!(
        declined.fix.as_deref(),
        Some("Connect again to check the fingerprint, or run `dbento ssh trust tun`.")
    );
    let conflicting = not_trusted("tun", "jump", 22, HostKeyAnswer::Conflicting);
    assert!(
        conflicting.message.contains("two different host keys"),
        "{conflicting:?}"
    );
    assert!(
        conflicting
            .fix
            .as_deref()
            .is_some_and(|f| f.contains("administrator")),
        "{conflicting:?}"
    );
}

#[test]
fn a_message_without_its_own_fix_line_carries_the_ssh_fix() {
    let dir = tempfile::tempdir().unwrap();
    let c = ctx(dir.path());
    let auth = ConnectError::SshAuth {
        message: "no method left".into(),
    };
    assert_eq!(
        connect_message("tun", &auth, &c),
        format!("{auth}\nhint: {AUTH_HINT}")
    );
    let unsupported = ConnectError::SshUnsupported {
        message: "Match".into(),
    };
    assert_eq!(
        connect_message("tun", &unsupported, &c),
        unsupported.to_string(),
        "no fix"
    );
}
