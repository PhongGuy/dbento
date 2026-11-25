//! ssh2-config 0.9 behaviour the resolver relies on: Include, Match and `%` tokens.
use ssh2_config::{ParseRule, SshConfig};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

/// A home whose `.ssh/config` is `config`, plus `(relative path under .ssh, text)` files.
fn home(config: &str, files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let ssh = dir.path().join(".ssh");
    std::fs::create_dir_all(ssh.join("conf.d")).unwrap();
    std::fs::write(ssh.join("config"), config).unwrap();
    for (rel, text) in files {
        std::fs::write(ssh.join(rel), text).unwrap();
    }
    dir
}

fn parse(home: &Path) -> SshConfig {
    let mut reader = BufReader::new(File::open(home.join(".ssh/config")).unwrap());
    SshConfig::default()
        .home_dir(home)
        .parse(
            &mut reader,
            ParseRule::ALLOW_UNKNOWN_FIELDS | ParseRule::ALLOW_UNSUPPORTED_FIELDS,
        )
        .unwrap()
}

const JUMP: &str = "Host jump\n  HostName 10.0.0.5\n  ProxyJump hop\n";

#[test]
fn a_tilde_glob_include_contributes_its_host_blocks() {
    let d = home(
        "Include ~/.ssh/conf.d/*\n\nHost other\n  Port 1\n",
        &[("conf.d/jump", JUMP)],
    );
    let p = parse(d.path()).query("jump");
    assert_eq!(
        p.host_name.as_deref(),
        Some("10.0.0.5"),
        "host name from the included file"
    );
    assert_eq!(
        p.proxy_jump,
        Some(vec!["hop".to_owned()]),
        "proxy jump from the included file"
    );
}

#[test]
fn a_relative_include_resolves_under_dot_ssh() {
    let d = home("Include conf.d/jump\n", &[("conf.d/jump", JUMP)]);
    assert_eq!(
        parse(d.path()).query("jump").host_name.as_deref(),
        Some("10.0.0.5"),
        "a relative Include path is read under ~/.ssh"
    );
}

#[test]
fn a_match_line_in_an_included_file_is_kept_as_an_ignored_field() {
    let d = home(
        "Include conf.d/jump\n",
        &[(
            "conf.d/jump",
            "Host jump\n  HostName x\nMatch host other\n  User y\n",
        )],
    );
    let config = parse(d.path());
    let has_match = config.get_hosts().iter().any(|h| {
        h.params
            .ignored_fields
            .keys()
            .any(|k| k.eq_ignore_ascii_case("match"))
    });
    assert!(has_match, "{config:?}");
}

#[test]
fn a_percent_token_is_kept_verbatim() {
    let d = home("Host jump\n  HostName %h.internal\n  User %u\n", &[]);
    let p = parse(d.path()).query("jump");
    assert_eq!(
        p.host_name.as_deref(),
        Some("%h.internal"),
        "HostName token unexpanded"
    );
    assert_eq!(p.user.as_deref(), Some("%u"), "User token unexpanded");
}
