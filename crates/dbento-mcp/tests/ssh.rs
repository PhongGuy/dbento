//! A tunnelled Postgres connection over MCP: an unknown host key is refused with its own
//! code and never trusted; once the user trusts it, the same call runs.
use crate::common::TestServer;
use dbento_core::{SshEnv, SshHome, StatePaths, ssh_password_env_var};
use dbento_testkit::{SECRET_VAR, SSH_PASSWORD, SSH_USER, ssh_net};
use serde_json::json;

#[tokio::test]
async fn an_unknown_host_key_is_refused_with_its_own_code_and_never_trusted() {
    let net = ssh_net().await;
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".ssh")).unwrap();
    let state = tempfile::tempdir().unwrap();
    let known = StatePaths::in_dir(state.path()).ssh_known_hosts;
    let env = SshEnv::new(SshHome::new(home.path(), "tester"), known.clone());
    let toml = format!(
        "dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"{}\"\nport = {}\ndatabase = \"{}\"\nuser = \"{}\"\nsecret = {{ env = \"{SECRET_VAR}\" }}\ntls = \"off\"\n[connections.pg.ssh]\nhost = \"127.0.0.1\"\nport = {}\nuser = \"{SSH_USER}\"\nauth = \"password\"\n",
        net.db_host(),
        net.db_port(),
        net.pg().database(),
        net.pg().user(),
        net.ssh_port()
    );
    let (pg_pw, ssh_var) = (net.pg().password().to_owned(), ssh_password_env_var("pg"));
    let server = TestServer::start_with(&toml, move |o| {
        o.env_lookup(move |v| {
            if v == SECRET_VAR {
                Some(pg_pw.clone())
            } else if v == ssh_var {
                Some(SSH_PASSWORD.to_owned())
            } else {
                None
            }
        })
        .ssh_env(env)
    })
    .await;
    let call = json!({"connection": "pg", "sql": "SELECT 1"});
    let e = server.err("query", call.clone()).await;
    assert_eq!(e["code"], "ssh_host_key", "{e}");
    assert!(
        e["message"].as_str().unwrap().contains(net.fingerprint()),
        "{e}"
    );
    assert!(!known.exists(), "MCP never trusts a key");
    let text = e.to_string();
    for dir in [home.path(), state.path()] {
        assert!(!text.contains(&*dir.to_string_lossy()), "{text}");
    }
    std::fs::write(
        &known,
        format!("[127.0.0.1]:{} {}\n", net.ssh_port(), net.host_key()),
    )
    .unwrap();
    let r = server.ok("query", call).await;
    assert_eq!(r["rows"], json!([["1"]]));
}
