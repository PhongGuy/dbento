//! `ssh -G` through the system OpenSSH client on a temporary `-F` config: Match exec, `%`
//! tokens, a refused key, and how its failures read once connecting.
use dbento_core::{
    ConnectError, ConnectionConfig, SshEnv, SshHome, SshResolveError, StatePaths, Workspace,
    probe_host_key, resolve_ssh,
};
use std::path::{Path, PathBuf};

/// The system `ssh` from absolute PATH entries; when absent, CI fails and elsewhere a notice prints.
fn system_ssh() -> Option<PathBuf> {
    let name = if cfg!(windows) { "ssh.exe" } else { "ssh" };
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .filter(|d| d.is_absolute())
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    });
    if found.is_none() {
        assert!(
            std::env::var_os("CI").is_none(),
            "CI must have an OpenSSH client on PATH"
        );
        eprintln!("notice: no ssh on PATH, skipping the real ssh -G test");
    }
    found
}

fn ws(ssh: &str) -> Workspace {
    let text = format!(
        "dbento = 1\n[connections.c]\nengine = \"postgres\"\nhost = \"db\"\ndatabase = \"d\"\nuser = \"u\"\n[connections.c.ssh]\n{ssh}"
    );
    Workspace::from_text(PathBuf::from("/w"), Path::new("/w/dbento.toml"), &text).unwrap()
}

fn conn(ssh: &str) -> ConnectionConfig {
    ws(ssh).declared("c").unwrap().clone()
}

/// A tempdir home whose `cfg` holds `config`, read by the system ssh; `None` without ssh.
fn fixture(config: &str) -> Option<(tempfile::TempDir, SshHome, PathBuf)> {
    let ssh = system_ssh()?;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".ssh")).unwrap();
    std::fs::write(dir.path().join("cfg"), config).unwrap();
    let home =
        SshHome::new(dir.path(), "tester").with_openssh(ssh.clone(), Some(dir.path().join("cfg")));
    Some((dir, home, ssh))
}

/// ssh's own `identityfile` lines for `alias`, so a failure records the form this platform prints.
fn raw_identityfile(ssh: &Path, cfg: &Path, alias: &str) -> String {
    let out = std::process::Command::new(ssh)
        .arg("-G")
        .arg("-F")
        .arg(cfg)
        .args(["--", alias])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with("identityfile "))
        .collect::<Vec<_>>()
        .join(" | ")
}

#[test]
fn match_exec_and_percent_tokens_resolve_through_the_real_ssh() {
    let exec = if cfg!(windows) { "" } else { " exec true" };
    let config = format!(
        "Match host jump{exec}\n  HostName 127.0.0.1\n  Port 2201\n  User matched\nHost tok\n  HostName %h.example.test\n  User ops\n  IdentityFile ~/.ssh/%r_key\n"
    );
    let Some((dir, home, ssh)) = fixture(&config) else {
        return;
    };
    let jump = resolve_ssh(&conn("host = \"jump\"\n"), Path::new("/w"), &home)
        .unwrap_or_else(|e| panic!("ssh -G with -F and -F none must work here: {e}"));
    assert_eq!(
        jump.login(),
        "matched@127.0.0.1:2201",
        "the Match block applies"
    );
    let flagged = resolve_ssh(
        &conn("host = \"jump\"\nuser = \"me\"\nport = 2222\n"),
        Path::new("/w"),
        &home,
    )
    .unwrap();
    assert_eq!(
        flagged.login(),
        "me@127.0.0.1:2222",
        "dbento.toml user and port win"
    );
    let tok = resolve_ssh(&conn("host = \"tok\"\n"), Path::new("/w"), &home).unwrap();
    assert_eq!(tok.host_name, "tok.example.test", "%h in HostName expands");
    assert_eq!(
        tok.identity_files,
        vec![dir.path().join(".ssh/ops_key")],
        "~ and %r expand against the SSH home; ssh printed: {}",
        raw_identityfile(&ssh, &dir.path().join("cfg"), "tok")
    );
}

#[test]
fn host_key_alias_is_refused_by_the_real_ssh_path() {
    let Some((dir, home, _)) = fixture("Host hka\n  HostKeyAlias other\n") else {
        return;
    };
    let e = resolve_ssh(&conn("host = \"hka\"\n"), Path::new("/w"), &home).unwrap_err();
    assert!(
        matches!(&e, SshResolveError::Unsupported { construct, file, .. } if *construct == "HostKeyAlias" && *file == dir.path().join("cfg")),
        "{e:?}"
    );
}

#[tokio::test]
async fn ssh_g_failures_read_as_ssh_connect_errors() {
    let Some(ssh) = system_ssh() else { return };
    let dir = tempfile::tempdir().unwrap();
    let home = SshHome::new(dir.path(), "tester")
        .with_openssh(ssh, Some(dir.path().join("missing-config")));
    let env = SshEnv::new(home, StatePaths::in_dir(dir.path()).ssh_known_hosts);
    for (alias, why) in [
        ("jump", "ssh exits 255 on a missing -F file"),
        ("ops@b", "a bad alias"),
    ] {
        let w = ws(&format!("host = \"{alias}\"\n"));
        match probe_host_key(&w, w.declared("c").unwrap(), &env).await {
            Err(ConnectError::SshConnect { address, message }) => {
                assert_eq!(address, alias, "{why}: the ssh host is the address");
                assert!(message.contains(alias), "{why}: {message}");
            }
            other => panic!("{why}: {other:?}"),
        }
    }
}
