//! `ssh -G` resolution against fake `ssh` programs: the argv sent, the alias check, and every
//! failure refusing without falling back to the parser.
use dbento_core::{
    ConnectionConfig, ResolvedSsh, SshHome, SshResolveError, Workspace, resolve_ssh,
};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const JUMP: &str = "[connections.c.ssh]\nhost = \"jump\"\n";

fn conn(ssh: &str) -> ConnectionConfig {
    let text = format!(
        "dbento = 1\n[connections.c]\nengine = \"postgres\"\nhost = \"db\"\ndatabase = \"d\"\nuser = \"u\"\n{ssh}"
    );
    Workspace::from_text(PathBuf::from("/w"), Path::new("/w/dbento.toml"), &text)
        .unwrap()
        .declared("c")
        .unwrap()
        .clone()
}

fn g(host: &str, user: &str, port: u16) -> String {
    format!(
        "user {user}\nhostname {host}\nport {port}\nidentityfile ~/.ssh/id_rsa\nuserknownhostsfile ~/.ssh/known_hosts\nconnecttimeout none\n"
    )
}

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_owned()).collect()
}

/// `resolve_ssh`, once more when a sibling test's fork still held the fresh script open.
/// The failed spawn logs no argv, so the call log stays exact.
fn resolve(c: &ConnectionConfig, home: &SshHome) -> Result<ResolvedSsh, SshResolveError> {
    let first = resolve_ssh(c, Path::new("/w"), home);
    if let Err(SshResolveError::OpenSsh { message, .. }) = &first
        && message.contains("Text file busy")
    {
        return resolve_ssh(c, Path::new("/w"), home);
    }
    first
}

/// A fake `ssh` in a tempdir that is also the SSH home.
struct Fake {
    dir: tempfile::TempDir,
    program: PathBuf,
}

impl Fake {
    /// Logs each call's arguments, runs `before` (shell), then prints `base` when called with
    /// `-F none` and `real` otherwise.
    fn new(real: &str, base: &str, before: &str) -> Fake {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().display();
        std::fs::write(dir.path().join("real.txt"), real).unwrap();
        std::fs::write(dir.path().join("base.txt"), base).unwrap();
        let script = format!(
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> '{d}/argv.log'; done\nprintf '%s\\n' '--end--' >> '{d}/argv.log'\n{before}\nprev=\nfor a in \"$@\"; do\n  if [ \"$prev\" = -F ] && [ \"$a\" = none ]; then cat '{d}/base.txt'; exit 0; fi\n  prev=$a\ndone\ncat '{d}/real.txt'\n"
        );
        let program = dir.path().join("ssh");
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        Fake { dir, program }
    }

    /// Each logged call's arguments in call order; empty when ssh never ran.
    fn calls(&self) -> Vec<Vec<String>> {
        let Ok(log) = std::fs::read_to_string(self.dir.path().join("argv.log")) else {
            return Vec::new();
        };
        log.split("--end--\n")
            .filter(|c| !c.is_empty())
            .map(|c| c.lines().map(str::to_owned).collect())
            .collect()
    }

    fn home(&self, config: Option<&Path>) -> SshHome {
        SshHome::new(self.dir.path(), "tester")
            .with_openssh(self.program.clone(), config.map(Path::to_path_buf))
    }
}

#[test]
fn ssh_g_resolves_with_the_dbento_toml_fields_as_flags() {
    let fake = Fake::new(&g("10.0.0.5", "me", 2222), &g("jump", "me", 2222), "");
    let cfg = fake.dir.path().join("cfg");
    std::fs::write(&cfg, "").unwrap();
    let c = conn("[connections.c.ssh]\nhost = \"jump\"\nuser = \"me\"\nport = 2222\n");
    let r = resolve(&c, &fake.home(Some(&cfg))).unwrap();
    assert_eq!(r.login(), "me@10.0.0.5:2222", "the login ssh -G resolved");
    let cfg = cfg.display().to_string();
    let calls = fake.calls();
    assert_eq!(calls.len(), 2, "one real call and one baseline: {calls:?}");
    assert!(
        calls.contains(&argv(&[
            "-G", "-F", &cfg, "-l", "me", "-p", "2222", "--", "jump"
        ])),
        "real: {calls:?}"
    );
    assert!(
        calls.contains(&argv(&[
            "-G", "-F", "none", "-l", "me", "-p", "2222", "--", "jump"
        ])),
        "baseline: {calls:?}"
    );
}

#[test]
fn without_a_config_path_the_real_call_reads_the_ssh_defaults() {
    let fake = Fake::new(&g("jump", "tester", 22), &g("jump", "tester", 22), "");
    resolve(&conn(JUMP), &fake.home(None)).unwrap();
    let calls = fake.calls();
    assert!(
        calls.contains(&argv(&["-G", "--", "jump"])),
        "real: {calls:?}"
    );
    assert!(
        calls.contains(&argv(&["-G", "-F", "none", "--", "jump"])),
        "baseline: {calls:?}"
    );
}

#[test]
fn an_alias_ssh_would_read_as_options_or_a_user_is_refused_before_ssh_runs() {
    for alias in [
        "ops@b",
        "ssh://ops@b:99",
        "-oProxyCommand=x",
        "a b",
        "a/b",
        "a%h",
    ] {
        let fake = Fake::new(&g("x", "u", 22), &g("x", "u", 22), "");
        let r = resolve(
            &conn(&format!("[connections.c.ssh]\nhost = \"{alias}\"\n")),
            &fake.home(None),
        );
        assert!(
            matches!(&r, Err(SshResolveError::BadHost { alias: a }) if a == alias),
            "{alias}: {r:?}"
        );
        assert!(fake.calls().is_empty(), "ssh must not run for {alias}");
    }
    for alias in ["jump", "db-1.example.com", "fe80::1", "host_x"] {
        let fake = Fake::new(&g("x", "u", 22), &g("x", "u", 22), "");
        let r = resolve(
            &conn(&format!("[connections.c.ssh]\nhost = \"{alias}\"\n")),
            &fake.home(None),
        );
        assert!(r.is_ok(), "{alias}: {r:?}");
    }
}

#[test]
fn a_failing_ssh_refuses_and_never_falls_back_to_the_parser() {
    let fake = Fake::new(
        &g("x", "u", 22),
        &g("x", "u", 22),
        "echo 'ssh: Could not resolve hostname jump' >&2\nexit 255",
    );
    std::fs::create_dir_all(fake.dir.path().join(".ssh")).unwrap();
    std::fs::write(
        fake.dir.path().join(".ssh/config"),
        "Host jump\n  HostName from-parser\n",
    )
    .unwrap();
    let e = resolve(&conn(JUMP), &fake.home(None)).unwrap_err();
    let SshResolveError::OpenSsh {
        alias,
        program,
        message,
    } = &e
    else {
        panic!("{e:?}");
    };
    assert_eq!(alias, "jump", "the alias is named");
    assert_eq!(program, &fake.program, "the program is named");
    assert!(
        message.contains("255") && message.contains("Could not resolve hostname"),
        "{message}"
    );
    let text = e.to_string();
    assert!(
        text.contains("`jump`") && text.contains(&fake.program.display().to_string()),
        "{text}"
    );
}

#[test]
fn the_message_keeps_the_last_twenty_stderr_lines() {
    let fake = Fake::new(
        "",
        "",
        "i=0; while [ $i -lt 30 ]; do echo line$i >&2; i=$((i+1)); done; exit 1",
    );
    match resolve(&conn(JUMP), &fake.home(None)) {
        Err(SshResolveError::OpenSsh { message, .. }) => {
            assert!(
                message.contains("line10") && message.contains("line29"),
                "the tail is kept: {message}"
            );
            assert!(!message.contains("line9"), "older lines are cut: {message}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn oversized_output_refuses_without_waiting_out_the_limit() {
    let fake = Fake::new("", "", "head -c 300000 /dev/zero | tr '\\000' a\nexit 0");
    let start = Instant::now();
    match resolve(&conn(JUMP), &fake.home(None)) {
        Err(SshResolveError::OpenSsh { message, .. }) => {
            assert!(message.contains("256 KiB"), "{message}")
        }
        other => panic!("{other:?}"),
    }
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "refused at the cap: {:?}",
        start.elapsed()
    );
}

#[test]
fn output_without_a_hostname_refuses() {
    let fake = Fake::new(
        "user x\nport 22\nconnecttimeout none\n",
        "user x\nport 22\nconnecttimeout none\n",
        "",
    );
    match resolve(&conn(JUMP), &fake.home(None)) {
        Err(SshResolveError::OpenSsh { message, .. }) => {
            assert!(message.contains("hostname"), "{message}")
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_refused_key_names_the_config_ssh_read() {
    let fake = Fake::new(
        &format!("{}hostkeyalias other\n", g("jump", "u", 22)),
        &g("jump", "u", 22),
        "",
    );
    let cfg = fake.dir.path().join("cfg");
    std::fs::write(&cfg, "").unwrap();
    for (config, file) in [
        (Some(cfg.as_path()), cfg.clone()),
        (None, fake.dir.path().join(".ssh/config")),
    ] {
        let e = resolve(&conn(JUMP), &fake.home(config)).unwrap_err();
        assert!(
            matches!(&e, SshResolveError::Unsupported { construct, file: f, .. } if *construct == "HostKeyAlias" && *f == file),
            "{e:?}"
        );
        assert!(e.to_string().contains("or the system ssh_config"), "{e}");
    }
}
