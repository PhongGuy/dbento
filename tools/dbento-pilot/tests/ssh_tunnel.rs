//! QA under gpui-pilot: the connection form's SSH section, the host-key question before a
//! query runs through the tunnel, and a changed key refused without a question.
mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args};
use dbento_app::ssh_form::SSH_HOST_NOTE;
use dbento_core::paths::STATE_DIR_VAR;
use dbento_core::{StatePaths, ssh_password_env_var};
use dbento_testkit::{SSH_PASSWORD, SSH_USER, SshNet};
use serde_json::{Map, Value, json};

const PW_VAR: &str = "DBENTO_SECRET_TEST_TUN_PW";
/// A throwaway Ed25519 public key that is not the fixture's.
const OTHER_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAICqzCt/eLjiYnme5P1CYozYfgd3otQ6E+HI5N0RPYe4m";

async fn until(pilot: &Pilot, what: &str, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let tree = pilot.ok("tree", json!({})).await;
        if done(&tree) {
            return tree;
        }
        assert!(Instant::now() < deadline, "{what} never happened: {tree}");
        pilot.ok("wait", json!({ "ms": 50 })).await;
    }
}

fn name<'a>(tree: &'a Value, id: &str) -> Option<&'a str> {
    find_node(tree, id).and_then(|n| n["name"].as_str())
}

async fn click(pilot: &Pilot, id: &str) {
    pilot.ok("click", json!({ "target": { "id": [id] } })).await;
}

async fn fill(pilot: &Pilot, id: &str, text: &str) {
    pilot
        .ok("type", json!({ "target": { "id": [id] }, "text": text }))
        .await;
}

async fn shot(pilot: &Pilot, file: &str) {
    if let Some(dir) = std::env::var_os("DBENTO_SHOTS_DIR").map(std::path::PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        pilot
            .ok("screenshot", json!({ "save_to": dir.join(file) }))
            .await;
    }
}

/// `tun`: Postgres through the fixture's SSH server with password auth.
fn tunnel_toml(f: &SshNet) -> String {
    format!(
        "dbento = 1\n\n[connections.tun]\nengine = \"postgres\"\nhost = \"{}\"\nport = {}\ndatabase = \"{}\"\nuser = \"{}\"\ntls = \"off\"\nsecret = {{ env = \"{PW_VAR}\" }}\n[connections.tun.ssh]\nhost = \"127.0.0.1\"\nport = {}\nuser = \"{SSH_USER}\"\nauth = \"password\"\n",
        f.db_host(),
        f.db_port(),
        f.pg().database(),
        f.pg().user(),
        f.ssh_port(),
    )
}

/// The app headless on `ws`, its state in `state`, both passwords in its environment.
async fn launch(f: &SshNet, ws: &Path, state: &Path) -> Pilot {
    let pilot = Pilot::start().await;
    let mut env = Map::new();
    env.insert(PW_VAR.into(), f.pg().password().into());
    env.insert(ssh_password_env_var("tun"), SSH_PASSWORD.into());
    env.insert(STATE_DIR_VAR.into(), state.display().to_string().into());
    env.insert("USER".into(), "tester".into());
    let mut args = launch_args("headless", ws);
    args["env"] = Value::Object(env);
    pilot.ok("launch", args).await;
    pilot
}

#[tokio::test(flavor = "multi_thread")]
async fn the_connection_form_has_an_ssh_section_with_its_note() {
    let ws = common::sqlite_workspace();
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["connection-new"] } }))
        .await;
    click(&pilot, "connection-new").await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["conn-ssh"] } }))
        .await;
    click(&pilot, "conn-ssh").await;
    let tree = until(&pilot, "the SSH rows", |t| {
        find_node(t, "conn-ssh-host").is_some()
    })
    .await;
    for id in [
        "conn-ssh-port",
        "conn-ssh-user",
        "conn-ssh-auth",
        "conn-ssh-identity",
        "conn-ssh-password",
        "conn-ssh-key-passphrase",
        "conn-ssh-keepalive",
    ] {
        assert!(find_node(&tree, id).is_some(), "{id}");
    }
    assert_eq!(name(&tree, "conn-ssh-note"), Some(SSH_HOST_NOTE));
    shot(&pilot, "ssh-form.png").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_host_key_asks_once_then_the_query_runs() {
    let f = dbento_testkit::ssh_net().await;
    let (ws, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(ws.path().join("dbento.toml"), tunnel_toml(&f)).unwrap();
    let pilot = launch(&f, ws.path(), state.path()).await;
    common::open_connection(&pilot, "connection-tun").await;
    let tree = until(&pilot, "the host key question", |t| {
        find_node(t, "host-key-trust").is_some()
    })
    .await;
    let detail = name(&tree, "confirm-detail").unwrap_or_default();
    assert!(detail.contains(f.fingerprint()), "{detail}");
    shot(&pilot, "ssh-host-key.png").await;
    click(&pilot, "host-key-trust").await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["editor"] } }))
        .await;
    fill(&pilot, "editor", "SELECT 41 + 1").await;
    click(&pilot, "run-all").await;
    until(&pilot, "the answer", |t| name(t, "cell-0-0") == Some("42")).await;
    let known = std::fs::read_to_string(StatePaths::in_dir(state.path()).ssh_known_hosts).unwrap();
    assert!(
        known.contains(&format!("[127.0.0.1]:{}", f.ssh_port())),
        "{known}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changed_host_key_is_refused_with_the_forget_command() {
    let f = dbento_testkit::ssh_net().await;
    let (ws, state) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(ws.path().join("dbento.toml"), tunnel_toml(&f)).unwrap();
    let known = StatePaths::in_dir(state.path()).ssh_known_hosts;
    std::fs::create_dir_all(known.parent().unwrap()).unwrap();
    std::fs::write(
        &known,
        format!("[127.0.0.1]:{} {OTHER_KEY}\n", f.ssh_port()),
    )
    .unwrap();
    let pilot = launch(&f, ws.path(), state.path()).await;
    common::open_connection(&pilot, "connection-tun").await;
    let tree = until(&pilot, "the refusal", |t| {
        name(t, "connect-error-hint").is_some_and(|h| h.contains("dbento ssh forget tun"))
    })
    .await;
    assert!(
        find_node(&tree, "host-key-trust").is_none(),
        "no question for a changed key"
    );
    shot(&pilot, "ssh-host-key-changed.png").await;
}
