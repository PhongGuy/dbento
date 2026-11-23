//! `~/.ssh/config` resolution of a connection's `ssh` table, against a temporary home.
use dbento_core::{
    ConnectionConfig, ResolvedSsh, SshAuth, SshHome, SshResolveError, Workspace, resolve_ssh,
    ssh_warnings,
};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn ws(ssh: &str) -> Workspace {
    let text = format!(
        "dbento = 1\n[connections.c]\nengine = \"postgres\"\nhost = \"db\"\ndatabase = \"d\"\nuser = \"u\"\n{ssh}"
    );
    Workspace::from_text(PathBuf::from("/w"), Path::new("/w/dbento.toml"), &text).unwrap()
}

fn conn(ssh: &str) -> ConnectionConfig {
    ws(ssh).declared("c").unwrap().clone()
}

/// A home holding `config` as `.ssh/config` (none when empty), extra `(path under .ssh, text)`
/// files, and the named empty key files.
fn home(config: &str, files: &[(&str, &str)], keys: &[&str]) -> (tempfile::TempDir, SshHome) {
    let dir = tempfile::tempdir().unwrap();
    let ssh = dir.path().join(".ssh");
    std::fs::create_dir_all(ssh.join("conf.d")).unwrap();
    if !config.is_empty() {
        std::fs::write(ssh.join("config"), config).unwrap();
    }
    for (rel, text) in files {
        std::fs::write(ssh.join(rel), text).unwrap();
    }
    for k in keys {
        std::fs::write(ssh.join(k), "").unwrap();
    }
    let home = SshHome::new(dir.path(), "tester");
    (dir, home)
}

fn resolve(
    ssh: &str,
    config: &str,
    keys: &[&str],
) -> (tempfile::TempDir, Result<ResolvedSsh, SshResolveError>) {
    let (dir, home) = home(config, &[], keys);
    let resolved = resolve_ssh(&conn(ssh), Path::new("/w"), &home);
    (dir, resolved)
}

fn refused_for(r: Result<ResolvedSsh, SshResolveError>, construct: &str) {
    let e = r.unwrap_err();
    assert!(
        matches!(&e, SshResolveError::Unsupported { construct: c, .. } if *c == construct),
        "{construct}: {e:?}"
    );
    let text = e.to_string();
    assert!(text.contains("is not supported yet"), "{text}");
    assert!(text.contains("dbento.toml"), "{text}");
    assert!(text.contains("`jump`"), "the alias is named: {text}");
}

const JUMP: &str = "[connections.c.ssh]\nhost = \"jump\"\n";

#[test]
fn without_a_config_file_the_defaults_apply() {
    let (_d, r) = resolve(JUMP, "", &[]);
    let r = r.unwrap();
    assert_eq!(
        (r.alias.as_str(), r.host_name.as_str()),
        ("jump", "jump"),
        "the alias is the host name"
    );
    assert_eq!(
        (r.port, r.user.as_str()),
        (22, "tester"),
        "default port and OS user"
    );
    assert!(r.identity_files.is_empty(), "{:?}", r.identity_files);
    assert_eq!(r.auth, None, "no auth written");
    assert!(r.keepalive, "keepalive defaults on");
    assert_eq!(
        r.connect_timeout,
        Duration::from_secs(15),
        "default connect timeout"
    );
    assert_eq!(r.login(), "tester@jump:22", "login");
}

#[test]
fn an_alias_takes_its_config_block() {
    let config = "Host jump\n  HostName 10.0.0.5\n  Port 2200\n  User ops\n  IdentityFile ~/.ssh/jump_key\n  ConnectTimeout 7\n\nHost other\n  Port 1\n";
    let (d, r) = resolve(JUMP, config, &[]);
    let r = r.unwrap();
    assert_eq!(r.login(), "ops@10.0.0.5:2200", "login from the block");
    assert_eq!(
        r.identity_files,
        vec![d.path().join(".ssh/jump_key")],
        "identity file from the block"
    );
    assert_eq!(
        r.connect_timeout,
        Duration::from_secs(7),
        "ConnectTimeout from the block"
    );
}

#[test]
fn an_included_file_resolves_like_the_main_one() {
    let (_d, home) = home(
        "Include conf.d/*\n",
        &[(
            "conf.d/jump",
            "Host jump\n  HostName 10.0.0.9\n  User inc\n",
        )],
        &[],
    );
    let r = resolve_ssh(&conn(JUMP), Path::new("/w"), &home).unwrap();
    assert_eq!(r.login(), "inc@10.0.0.9:22", "login from the included file");
}

#[test]
fn dbento_toml_fields_win_over_the_config() {
    let config =
        "Host jump\n  HostName 10.0.0.5\n  Port 2200\n  User ops\n  IdentityFile ~/.ssh/jump_key\n";
    let ssh = "[connections.c.ssh]\nhost = \"jump\"\nport = 2222\nuser = \"me\"\nauth = \"key\"\nidentity_file = \"keys/id\"\nkeepalive = false\n";
    let (_d, r) = resolve(ssh, config, &[]);
    let r = r.unwrap();
    assert_eq!(
        r.login(),
        "me@10.0.0.5:2222",
        "dbento.toml port and user win"
    );
    assert_eq!(
        r.identity_files,
        vec![PathBuf::from("/w/keys/id")],
        "a relative identity file is under the workspace"
    );
    assert_eq!(r.auth, Some(SshAuth::Key), "auth as written");
    assert!(!r.keepalive, "keepalive as written");
}

#[test]
fn a_tilde_identity_file_expands_per_home() {
    let (d, r) = resolve(&format!("{JUMP}identity_file = \"~/.ssh/id_x\"\n"), "", &[]);
    assert_eq!(
        r.unwrap().identity_files,
        vec![d.path().join(".ssh/id_x")],
        "~ is the SSH home"
    );
}

#[test]
fn openssh_default_keys_are_used_in_order_when_present() {
    let (d, r) = resolve(JUMP, "", &["id_rsa", "id_ed25519", "unrelated"]);
    let ssh = d.path().join(".ssh");
    assert_eq!(
        r.unwrap().identity_files,
        vec![ssh.join("id_ed25519"), ssh.join("id_rsa")],
        "present default keys in OpenSSH order"
    );
}

#[test]
fn proxy_jump_and_proxy_command_are_refused() {
    for (config, construct) in [
        ("Host jump\n  ProxyJump hop\n", "ProxyJump"),
        (
            "Host jump\n  ProxyCommand ssh -W %h:%p hop\n",
            "ProxyCommand",
        ),
        ("Host *\n  ProxyJump hop\n", "ProxyJump"),
    ] {
        refused_for(resolve(JUMP, config, &[]).1, construct);
    }
}

#[test]
fn proxy_set_to_none_is_off() {
    for config in [
        "Host jump\n  ProxyJump none\n",
        "Host jump\n  ProxyCommand none\n",
    ] {
        let (_d, r) = resolve(JUMP, config, &[]);
        assert!(r.is_ok(), "{config}: {r:?}");
    }
}

#[test]
fn a_match_anywhere_in_the_config_refuses() {
    for config in [
        "Host jump\n  HostName x\nMatch host other\n  User y\n",
        "Match all\n  User y\nHost other\n  Port 1\n",
    ] {
        refused_for(resolve(JUMP, config, &[]).1, "Match");
    }
}

#[test]
fn a_match_in_an_included_file_refuses() {
    let (_d, home) = home(
        "Include conf.d/*\n",
        &[(
            "conf.d/other",
            "Host other\n  Port 1\nMatch host other\n  User y\n",
        )],
        &[],
    );
    refused_for(resolve_ssh(&conn(JUMP), Path::new("/w"), &home), "Match");
}

#[test]
fn a_percent_token_in_a_used_field_refuses() {
    for config in [
        "Host jump\n  HostName %h.internal\n",
        "Host jump\n  User %u\n",
        "Host jump\n  IdentityFile ~/.ssh/%r_key\n",
    ] {
        refused_for(resolve(JUMP, config, &[]).1, "a % token");
    }
}

#[test]
fn a_percent_token_dbento_toml_overrides_is_not_used() {
    let ssh = "[connections.c.ssh]\nhost = \"jump\"\nuser = \"me\"\n";
    let (_d, r) = resolve(ssh, "Host jump\n  User %u\n", &[]);
    assert_eq!(r.unwrap().user, "me", "the overridden token is never read");
}

#[test]
fn unknown_and_unsupported_options_are_ignored() {
    let config = "Host jump\n  SomeFutureOption yes\n  UserKnownHostsFile /dev/null\n  StrictHostKeyChecking no\n";
    let (_d, r) = resolve(JUMP, config, &[]);
    assert!(r.is_ok(), "{r:?}");
}

#[test]
fn a_broken_config_names_the_file() {
    let (d, r) = resolve(JUMP, "Host jump\n  Port notaport\n", &[]);
    match r {
        Err(SshResolveError::Config { path, .. }) => {
            assert_eq!(path, d.path().join(".ssh/config"), "the file is named");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_connection_without_ssh_has_nothing_to_resolve() {
    let (_d, home) = home("", &[], &[]);
    assert!(
        matches!(
            resolve_ssh(&conn(""), Path::new("/w"), &home),
            Err(SshResolveError::NoSsh)
        ),
        "no ssh table"
    );
}

#[test]
fn only_an_absolute_identity_file_outside_the_home_warns() {
    let (d, home) = home("", &[], &[]);
    let inside = d.path().join(".ssh/id_x");
    for (file, warns) in [
        ("~/.ssh/id_x".to_owned(), false),
        ("keys/id".to_owned(), false),
        (inside.display().to_string(), false),
        ("/opt/keys/id".to_owned(), true),
    ] {
        let w = ws(&format!("{JUMP}identity_file = \"{file}\"\n"));
        let warnings = ssh_warnings(&w, &home);
        assert_eq!(warnings.len(), usize::from(warns), "{file}: {warnings:?}");
        if warns {
            assert!(warnings[0].contains("connection `c`"), "{warnings:?}");
            assert!(warnings[0].contains("/opt/keys/id"), "{warnings:?}");
        }
    }
}
