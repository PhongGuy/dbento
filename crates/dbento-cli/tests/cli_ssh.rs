//! `dbento ssh trust|forget` and queries through the sshd fixture, with a temporary SSH
//! home (also `HOME`) and state folder, the keychain off and no agent.
use crate::common::{dbento, write_tree};
use dbento_testkit::{SECRET_VAR, SSH_PASSWORD, SSH_USER, SshNet, ssh_net};
use std::process::Output;
use tempfile::TempDir;

const B: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILY84+Y52dK/APwNyrey10YmkTK3XSb2Yat+Dg2fiIr5";

struct Cli {
    ws: TempDir,
    home: TempDir,
    state: TempDir,
}

fn cli(net: &SshNet) -> Cli {
    let toml = format!(
        "dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"{}\"\nport = {}\ndatabase = \"{}\"\nuser = \"{}\"\nsecret = {{ env = \"{SECRET_VAR}\" }}\ntls = \"off\"\n[connections.pg.ssh]\nhost = \"127.0.0.1\"\nport = {}\nuser = \"{SSH_USER}\"\nauth = \"password\"\n",
        net.db_host(),
        net.db_port(),
        net.pg().database(),
        net.pg().user(),
        net.ssh_port()
    );
    let home = TempDir::new().unwrap();
    std::fs::create_dir_all(home.path().join(".ssh")).unwrap();
    Cli {
        ws: write_tree(&[("dbento.toml", toml.as_str())]),
        home,
        state: TempDir::new().unwrap(),
    }
}

fn dbento_ssh(c: &Cli, net: &SshNet, args: &[&str], stdin: &str, tty: bool) -> Output {
    let mut cmd = dbento();
    cmd.current_dir(c.ws.path())
        .env("DBENTO_TEST_SSH_HOME", c.home.path())
        .env("HOME", c.home.path())
        .env("DBENTO_STATE_DIR", c.state.path())
        .env("DBENTO_TEST_NO_KEYCHAIN", "1")
        .env(SECRET_VAR, net.pg().password())
        .env(dbento_core::ssh_password_env_var("pg"), SSH_PASSWORD)
        .env_remove("SSH_AUTH_SOCK");
    if tty {
        cmd.env("DBENTO_TEST_ASSUME_TTY", "1");
    }
    cmd.args(args).write_stdin(stdin).output().unwrap()
}

fn err(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn known(c: &Cli) -> String {
    std::fs::read_to_string(c.state.path().join("ssh_known_hosts")).unwrap_or_default()
}

#[tokio::test]
async fn an_unknown_key_stops_a_query_with_its_fingerprint_and_the_trust_command() {
    let net = ssh_net().await;
    let c = cli(&net);
    let out = dbento_ssh(&c, &net, &["query", "pg", "SELECT 1"], "", false);
    assert_eq!(out.status.code(), Some(3), "{}", err(&out));
    // The Postgres secret is supplied, and the hint still comes.
    let hint = format!(
        "hint: check {} with the server's admin, then run `dbento ssh trust pg`",
        net.fingerprint()
    );
    assert!(err(&out).contains(&hint), "{}", err(&out));
    assert_eq!(known(&c), "");
}

#[tokio::test]
async fn trust_needs_a_matching_fingerprint_without_a_terminal_then_queries_run() {
    let net = ssh_net().await;
    let c = cli(&net);
    let out = dbento_ssh(&c, &net, &["ssh", "trust", "pg"], "", false);
    assert_eq!(out.status.code(), Some(3));
    assert!(
        err(&out).contains(net.fingerprint()) && err(&out).contains("--fingerprint"),
        "{}",
        err(&out)
    );
    let out = dbento_ssh(
        &c,
        &net,
        &["ssh", "trust", "pg", "--fingerprint", "SHA256:AAAA"],
        "",
        false,
    );
    assert_eq!(out.status.code(), Some(3));
    assert!(err(&out).contains("does not match"), "{}", err(&out));
    assert_eq!(known(&c), "");
    let args = ["ssh", "trust", "pg", "--fingerprint", net.fingerprint()];
    let out = dbento_ssh(&c, &net, &args, "", false);
    assert_eq!(out.status.code(), Some(0), "{}", err(&out));
    assert_eq!(
        known(&c),
        format!("[127.0.0.1]:{} {}\n", net.ssh_port(), net.host_key())
    );
    let out = dbento_ssh(&c, &net, &["ssh", "trust", "pg"], "", false);
    assert_eq!(out.status.code(), Some(0));
    assert!(err(&out).contains("already trusted"), "{}", err(&out));
    let out = dbento_ssh(
        &c,
        &net,
        &["query", "pg", "SELECT 1", "--format", "json"],
        "",
        false,
    );
    assert_eq!(out.status.code(), Some(0), "{}", err(&out));
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["statements"][0]["rows"], serde_json::json!([["1"]]));
}

#[tokio::test]
async fn a_terminal_answer_trusts_or_declines() {
    let net = ssh_net().await;
    let c = cli(&net);
    let out = dbento_ssh(&c, &net, &["ssh", "trust", "pg"], "n\n", true);
    assert_eq!(out.status.code(), Some(2), "{}", err(&out));
    assert_eq!(known(&c), "");
    let out = dbento_ssh(&c, &net, &["ssh", "trust", "pg"], "y\n", true);
    assert_eq!(out.status.code(), Some(0), "{}", err(&out));
    assert!(known(&c).contains(net.host_key()));
}

#[tokio::test]
async fn forget_and_a_changed_key_point_at_the_right_remedy() {
    let net = ssh_net().await;
    let c = cli(&net);
    let id = format!("[127.0.0.1]:{}", net.ssh_port());
    std::fs::write(
        c.state.path().join("ssh_known_hosts"),
        format!("{id} {B}\n"),
    )
    .unwrap();
    let out = dbento_ssh(&c, &net, &["ssh", "trust", "pg"], "y\n", true);
    assert_eq!(out.status.code(), Some(3));
    let hint = format!(
        "hint: if the key was rotated, check {} with the server's admin, run `dbento ssh forget pg`, then `dbento ssh trust pg`",
        net.fingerprint()
    );
    assert!(err(&out).contains(&hint), "{}", err(&out));
    assert_eq!(known(&c), format!("{id} {B}\n"), "a refusal stores nothing");
    let out = dbento_ssh(&c, &net, &["ssh", "forget", "pg"], "", false);
    assert_eq!(out.status.code(), Some(0), "{}", err(&out));
    assert!(err(&out).contains("removed 1"), "{}", err(&out));
    let out = dbento_ssh(&c, &net, &["ssh", "forget", "pg"], "", false);
    assert!(err(&out).contains("no stored key"), "{}", err(&out));
    std::fs::write(
        c.home.path().join(".ssh/known_hosts"),
        format!("{id} {}\n", net.host_key()),
    )
    .unwrap();
    let out = dbento_ssh(&c, &net, &["ssh", "forget", "pg"], "", false);
    assert!(
        err(&out).contains(&format!("ssh-keygen -R '{id}'")),
        "{}",
        err(&out)
    );
}

#[tokio::test]
async fn trust_stores_nothing_over_any_refusal_and_names_its_remedy() {
    let net = ssh_net().await;
    let c = cli(&net);
    let (id, fp, key) = (
        format!("[127.0.0.1]:{}", net.ssh_port()),
        net.fingerprint(),
        net.host_key(),
    );
    let own = c.state.path().join("ssh_known_hosts");
    let cases = [
        (
            format!("{id} {B}\n"),
            String::new(),
            format!(
                "hint: if the key was rotated, check {fp} with the server's admin, run ssh-keygen -R '{id}', then `dbento ssh trust pg`"
            ),
        ),
        (
            format!("@revoked * {key}\n"),
            format!("{id} {key}\n"),
            "hint: a revoked key is never trusted; ask the server's admin why it was revoked"
                .to_owned(),
        ),
        (
            String::new(),
            format!("{id} ssh-ed25519 !!!\n"),
            format!(
                "hint: fix or delete line 1 of {}, then run `dbento ssh trust pg`",
                own.display()
            ),
        ),
    ];
    for (user, mine, hint) in cases {
        std::fs::write(c.home.path().join(".ssh/known_hosts"), &user).unwrap();
        std::fs::write(&own, &mine).unwrap();
        let out = dbento_ssh(&c, &net, &["ssh", "trust", "pg"], "y\n", true);
        assert_eq!(out.status.code(), Some(3), "{}", err(&out));
        assert!(
            err(&out).contains(&format!("({fp})")) && err(&out).contains("; refusing"),
            "{}",
            err(&out)
        );
        assert!(err(&out).contains(&hint), "{}", err(&out));
        assert!(!err(&out).contains("forget"), "{}", err(&out));
        assert_eq!(known(&c), mine, "a refusal stores nothing");
    }
}
