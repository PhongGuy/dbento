//! An unknown SSH host key asks once per server; the sessions waiting on it go on after Trust
//! or fail after Don't trust, and a changed key is refused without a question.
use crate::support::{self, click_id, label_of, present, wait_idle, wait_prompt, wait_real};
use dbento_app::app::start;
use dbento_app::browse::BrowseEvent;
use dbento_app::connection_dialog::Field;
use dbento_app::host_keys::HostKeyAnswer;
use dbento_app::host_keys::host_key_ask;
use dbento_app::link::{ConnectFailure, LinkState};
use dbento_app::runner::RunnerSpec;
use dbento_app::ssh::{AUTH_HINT, TUNNEL_HINT};
use dbento_app::tab_model::TabModel;
use dbento_core::{RelationRef, Workspace, trust_host_key};
use dbento_core::{StatePaths, ssh_password_env_var};
use dbento_testkit::{SSH_PASSWORD, SSH_USER, SshNet};
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

const PW_VAR: &str = "DBENTO_SECRET_TEST_TUN_PW";
/// A throwaway Ed25519 public key that is not the fixture's.
const OTHER_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAICqzCt/eLjiYnme5P1CYozYfgd3otQ6E+HI5N0RPYe4m";

#[test]
fn the_question_shows_the_fingerprint_and_declines_by_default() {
    let ask = host_key_ask("tun", "127.0.0.1", 2222, "SHA256:abc");
    assert_eq!(
        ask.title.to_string(),
        "Trust the SSH host key of 127.0.0.1:2222?"
    );
    let detail = ask.detail.unwrap().to_string();
    assert!(
        detail.contains("SHA256:abc") && detail.contains("tun"),
        "{detail}"
    );
    let ids: Vec<_> = ask.choices.iter().map(|c| c.id).collect();
    assert_eq!(ids, vec!["host-key-reject", "host-key-trust"]);
    assert_eq!(
        (ask.default, ask.cancel),
        (0, 0),
        "Enter and Escape decline"
    );
}

/// Postgres `name` through the fixture's SSH server, reading `db_host` as the bastion sees it.
fn tunnelled(f: &SshNet, name: &str, db_host: &str) -> String {
    format!(
        "[connections.{name}]\nengine = \"postgres\"\nhost = \"{db_host}\"\nport = {}\ndatabase = \"{}\"\nuser = \"{}\"\ntls = \"off\"\nsecret = {{ env = \"{PW_VAR}\" }}\n[connections.{name}.ssh]\nhost = \"127.0.0.1\"\nport = {}\nuser = \"{SSH_USER}\"\nauth = \"password\"\n",
        f.db_port(),
        f.pg().database(),
        f.pg().user(),
        f.ssh_port(),
    )
}

struct Run {
    win: AnyWindowHandle,
    shell: Entity<dbento_app::shell::Shell>,
    known_hosts: PathBuf,
    _dir: tempfile::TempDir,
    _files: tempfile::TempDir,
}

/// The app's tokio runtime. Each test enters it before starting the fixture, so the fixture
/// (declared after the guard) drops while the runtime is still entered.
fn runtime(cx: &mut TestAppContext) -> tokio::runtime::Handle {
    support::init(cx);
    support::init_tokio(cx);
    cx.update(|cx| dbento_app::tokio_bridge::handle(cx))
}

/// The app on `body` with the database password and each connection's SSH password (`x`, which
/// the server refuses, for `bad`) in the environment; a non-empty `seed` is written as Dbento's
/// known_hosts first.
fn run(cx: &mut TestAppContext, f: &SshNet, body: &str, seed: &str) -> Run {
    let (dir, files) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!("dbento = 1\n{body}"),
    )
    .unwrap();
    let known_hosts = StatePaths::in_dir(files.path()).ssh_known_hosts;
    if !seed.is_empty() {
        std::fs::create_dir_all(known_hosts.parent().unwrap()).unwrap();
        std::fs::write(&known_hosts, seed).unwrap();
    }
    let ssh = SSH_PASSWORD.to_owned();
    let vars: Vec<(String, String)> = vec![
        (PW_VAR.to_owned(), f.pg().password().to_owned()),
        (ssh_password_env_var("tun"), ssh.clone()),
        (ssh_password_env_var("far"), ssh),
        (ssh_password_env_var("bad"), "x".to_owned()),
    ];
    let mut services = support::services(files.path());
    services.env = Arc::new(move |v| vars.iter().find(|(k, _)| k == v).map(|(_, s)| s.clone()));
    let (win, shell) = cx
        .update(|cx| start(services, Some(dir.path().into()), cx))
        .unwrap();
    present(cx, win, "connection-list");
    Run {
        win,
        shell,
        known_hosts,
        _dir: dir,
        _files: files,
    }
}

impl Run {
    fn open(&self, cx: &mut TestAppContext, name: &str) {
        let shell = self.shell.clone();
        cx.update_window(self.win, |_, w, cx| {
            shell.update(cx, |s, cx| s.open_connection_named(name, w, cx))
        })
        .unwrap();
        cx.run_until_parked();
    }
    /// Each tab's connection, link and failure, in tab order.
    fn tabs(&self, cx: &mut TestAppContext) -> Vec<(String, LinkState, Option<ConnectFailure>)> {
        self.shell.read_with(cx, |s, cx| {
            s.tabs()
                .iter()
                .map(|t| {
                    let m = t.read(cx).model().read(cx);
                    (
                        m.spec().connection.clone(),
                        m.link(),
                        m.connect_failure().cloned(),
                    )
                })
                .collect()
        })
    }
}

#[gpui_kit::test]
fn one_question_serves_every_session_on_the_server(cx: &mut TestAppContext) {
    let rt = runtime(cx);
    let _enter = rt.enter();
    let f = rt.block_on(dbento_testkit::ssh_net());
    let body = format!(
        "{}{}{}",
        tunnelled(&f, "tun", f.db_host()),
        tunnelled(&f, "far", "nowhere.invalid"),
        tunnelled(&f, "bad", f.db_host()),
    );
    let r = run(cx, &f, &body, "");
    for name in ["tun", "tun", "far", "bad"] {
        r.open(cx, name);
    }
    wait_real(cx, "the question", |cx| {
        present(cx, r.win, "host-key-trust")
    });
    let detail = label_of(cx, r.win, "confirm-detail").unwrap();
    assert!(detail.contains(f.fingerprint()), "{detail}");
    click_id(cx, r.win, "host-key-trust");
    wait_real(cx, "every tab settled", |cx| {
        r.tabs(cx)
            .iter()
            .all(|(_, link, failure)| *link == LinkState::Connected || failure.is_some())
    });
    let tabs = r.tabs(cx);
    let fix = |name: &str| {
        tabs.iter()
            .find(|(c, _, _)| c == name)
            .and_then(|(_, _, f)| f.as_ref())
            .and_then(|f| f.fix.clone())
    };
    let connected = tabs
        .iter()
        .filter(|(c, l, _)| c == "tun" && *l == LinkState::Connected)
        .count();
    assert_eq!(connected, 2, "{tabs:?}");
    assert_eq!(fix("far").as_deref(), Some(TUNNEL_HINT), "{tabs:?}");
    assert_eq!(fix("bad").as_deref(), Some(AUTH_HINT), "{tabs:?}");
    assert!(
        !present(cx, r.win, "confirm-dialog"),
        "one question for the server"
    );
    let known = std::fs::read_to_string(&r.known_hosts).unwrap();
    assert!(
        known.contains(&format!("[127.0.0.1]:{}", f.ssh_port())),
        "{known}"
    );
}

#[gpui_kit::test]
fn declining_fails_the_session_and_names_the_trust_command(cx: &mut TestAppContext) {
    let rt = runtime(cx);
    let _enter = rt.enter();
    let f = rt.block_on(dbento_testkit::ssh_net());
    let r = run(cx, &f, &tunnelled(&f, "tun", f.db_host()), "");
    r.open(cx, "tun");
    wait_real(cx, "the question", |cx| {
        present(cx, r.win, "host-key-reject")
    });
    click_id(cx, r.win, "host-key-reject");
    wait_real(cx, "the failure", |cx| {
        present(cx, r.win, "connect-error-hint")
    });
    assert_eq!(
        label_of(cx, r.win, "connect-error-hint").as_deref(),
        Some("Connect again to check the fingerprint, or run `dbento ssh trust tun`.")
    );
    // a2 pins that core writes no known_hosts on an unknown key; declining adds no write here.
    assert!(!r.known_hosts.exists(), "nothing was written");
}

#[gpui_kit::test]
fn a_changed_key_is_refused_without_a_question(cx: &mut TestAppContext) {
    let rt = runtime(cx);
    let _enter = rt.enter();
    let f = rt.block_on(dbento_testkit::ssh_net());
    let seed = format!("[127.0.0.1]:{} {OTHER_KEY}\n", f.ssh_port());
    let r = run(cx, &f, &tunnelled(&f, "tun", f.db_host()), &seed);
    r.open(cx, "tun");
    wait_real(cx, "the refusal", |cx| {
        present(cx, r.win, "connect-error-hint")
    });
    let hint = label_of(cx, r.win, "connect-error-hint").unwrap();
    assert!(hint.contains("dbento ssh forget tun"), "{hint}");
    assert!(
        !present(cx, r.win, "confirm-dialog"),
        "a changed key is never asked about"
    );
    assert_eq!(
        std::fs::read_to_string(&r.known_hosts).unwrap(),
        seed,
        "untouched"
    );
}

/// `tun` as a runner spec on `support::services(files)`, Dbento's known_hosts seeded with the
/// fixture's key so the browse connects without a question.
fn tunnelled_spec(f: &SshNet, dir: &Path, files: &Path) -> RunnerSpec {
    let toml = format!("dbento = 1\n{}", tunnelled(f, "tun", f.db_host()));
    std::fs::write(dir.join("dbento.toml"), toml).unwrap();
    let known_hosts = StatePaths::in_dir(files).ssh_known_hosts;
    std::fs::create_dir_all(known_hosts.parent().unwrap()).unwrap();
    let seed = format!("[127.0.0.1]:{} {}\n", f.ssh_port(), f.host_key());
    std::fs::write(&known_hosts, seed).unwrap();
    let services = support::services(files);
    let (db_pw, ssh_var) = (f.pg().password().to_owned(), ssh_password_env_var("tun"));
    RunnerSpec {
        workspace: Arc::new(Workspace::open(dir).unwrap()),
        connection: "tun".into(),
        store: services.store,
        env: Arc::new(move |v: &str| match v {
            PW_VAR => Some(db_pw.clone()),
            v if v == ssh_var => Some(SSH_PASSWORD.to_owned()),
            _ => None,
        }),
        ssh: services.ssh,
    }
}

#[gpui_kit::test]
fn a_save_that_meets_an_unknown_key_asks_then_applies(cx: &mut TestAppContext) {
    let rt = runtime(cx);
    let _enter = rt.enter();
    let f = rt.block_on(dbento_testkit::ssh_net());
    rt.block_on(async {
        let setup = "CREATE TABLE t(id int PRIMARY KEY, name text); INSERT INTO t VALUES (1, 'a');";
        f.pg().admin().await.batch_execute(setup).await.unwrap()
    });
    let (dir, files) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let spec = tunnelled_spec(&f, dir.path(), files.path());
    let gate = Arc::clone(&spec.ssh.gate);
    // The test is the prompt handler, so it sees exactly what Save asks.
    let prompts = gate.prompts();
    let m = cx.new(|_| TabModel::new(spec));
    cx.update(|cx| {
        cx.subscribe(&m, |m, e: &BrowseEvent, cx| {
            if let BrowseEvent::Run(sql) = e {
                let sql = sql.clone();
                m.update(cx, |m, cx| m.run(sql, cx));
            }
        })
        .detach();
    });
    m.update(cx, |m, cx| {
        m.load_target(RelationRef::new("public", "t"), cx)
    });
    wait_real(cx, "an editable browse", |cx| {
        m.read_with(cx, |m, _| m.editable() && !m.running())
    });
    // Dbento loses the key, so Save's own connect meets it as unknown.
    let known_hosts = StatePaths::in_dir(files.path()).ssh_known_hosts;
    std::fs::remove_file(&known_hosts).unwrap();
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    assert!(m.update(cx, |m, cx| m.save(cx)));
    let mut asked = None;
    wait_real(cx, "a question or a failure", |cx| {
        asked = asked.take().or_else(|| prompts.try_recv().ok());
        asked.is_some() || m.read_with(cx, |m, _| m.edit_banner().is_some())
    });
    let banner = m.read_with(cx, |m, _| m.edit_banner().map(str::to_owned));
    let p = asked.unwrap_or_else(|| panic!("Save never asked: {banner:?}"));
    assert_eq!(p.fingerprint, f.fingerprint());
    trust_host_key(&StatePaths::in_dir(files.path()), &p.key).unwrap();
    assert!(gate.answer(&p.host, p.port, &p.fingerprint, HostKeyAnswer::Trusted));
    wait_prompt(cx, &m);
    m.update(cx, |m, cx| m.rollback(cx));
    wait_idle(cx, &m);
}

#[gpui_kit::test]
fn test_connection_asks_about_the_host_key_and_uses_the_typed_ssh_password(
    cx: &mut TestAppContext,
) {
    let rt = runtime(cx);
    let _enter = rt.enter();
    let f = rt.block_on(dbento_testkit::ssh_net());
    // `bad`'s SSH password variable holds `x`, which the server refuses: only the typed one works.
    let r = run(cx, &f, &tunnelled(&f, "bad", f.db_host()), "");
    support::gear(cx, r.win, "bad");
    wait_real(cx, "the dialog", |cx| present(cx, r.win, "conn-name"));
    let form = r
        .shell
        .read_with(cx, |s, _| s.connection_form().cloned().unwrap());
    let input = form.read_with(cx, |f, _| f.input(Field::SshPassword).clone());
    cx.update_window(r.win, |_, w, cx| {
        input.update(cx, |s, cx| s.set_value(SSH_PASSWORD.to_owned(), w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    click_id(cx, r.win, "conn-test");
    let failed = |cx: &mut TestAppContext| {
        label_of(cx, r.win, "conn-test-result").filter(|l| l.starts_with("Failed: "))
    };
    wait_real(cx, "the question or a failure", |cx| {
        present(cx, r.win, "host-key-trust") || failed(cx).is_some()
    });
    assert!(
        present(cx, r.win, "host-key-trust"),
        "Test asks first: {:?}",
        failed(cx)
    );
    click_id(cx, r.win, "host-key-trust");
    wait_real(cx, "the test result", |cx| {
        label_of(cx, r.win, "conn-test-result")
            .is_some_and(|l| l == "Connected" || l.starts_with("Failed: "))
    });
    assert_eq!(
        label_of(cx, r.win, "conn-test-result").as_deref(),
        Some("Connected")
    );
}
