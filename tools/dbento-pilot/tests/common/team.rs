//! A pilot launch on a workspace bound to a file:// origin and the loopback Potluck stub.
//! The app's tokens live in memory, seeded from DBENTO_TEST_TEAM_TOKEN; git and the stub stay
//! on this machine.
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dbento_core::paths::StatePaths;
use dbento_core::{AccountRecord, BindingRecord, ServerUrl, TeamFile, public_url_of};
use dbento_testkit::potluck::{self, Origin, Stub, write};
use serde_json::{Value, json};

use super::{Pilot, find_node, launch_args};

pub const TOML: &str = "dbento = 1\n\n[connections.lite]\nengine = \"sqlite\"\npath = \"app.db\"\n";

pub struct PilotTeam {
    pub origin: Origin,
    pub root: PathBuf,
    pub ws: PathBuf,
    pub state: PathBuf,
    pub stub: Stub,
    pub token: String,
}

impl PilotTeam {
    /// The headless launch on `ws` with the team state dir, the seed token and git on PATH.
    pub fn launch_args(&self) -> Value {
        let mut args = launch_args("headless", &self.ws);
        args["env"] = json!({
            "DBENTO_STATE_DIR": self.state,
            "DBENTO_TEST_TEAM_TOKEN": self.token,
            "PATH": "/usr/bin:/bin",
            "GIT_CONFIG_GLOBAL": "/dev/null",
            "GIT_CONFIG_NOSYSTEM": "1",
        });
        args
    }
    pub fn origin_show(&self, rel: &str) -> Option<String> {
        self.origin.show(rel)
    }
    pub fn teammate(&self, rel: &str, text: &str) {
        self.origin.teammate(rel, text);
    }
    pub fn local_commit(&self, rel: &str, text: &str) {
        self.origin.commit_in(&self.ws, rel, text);
    }
    pub fn hook(&self, name: &str, body: &str) {
        self.origin.hook(name, body);
    }
    /// A `ref` notice for `main` at the origin's current tip, sent to every open stream.
    pub fn notify_ref(&self) {
        self.stub.send(&format!(
            "event: ref\ndata: {{\"ref\":\"refs/heads/main\",\"sha\":\"{}\"}}\n\n",
            self.origin.tip()
        ));
    }
}

/// As the app tests' fixture: a seeded origin, the stub with `role`, and when `bound` a clone
/// at `ws` bound for ada; otherwise `ws` is a plain SQLite workspace and `state` is empty.
pub fn team_fixture_with(role: &str, bound: bool) -> PilotTeam {
    let origin = Origin::seeded(&[("dbento.toml", TOML), ("queries/a.sql", "select 1;\n")]);
    let root = origin.root().to_path_buf();
    let (stub, state) = (Stub::start(), root.join("state"));
    std::fs::create_dir_all(&state).unwrap();
    potluck::serve_team(&stub, origin.url(), role);
    let ws = if bound {
        origin.clone_to("ws")
    } else {
        write(&root.join("ws"), "dbento.toml", TOML);
        root.join("ws")
    };
    std::fs::write(ws.join("app.db"), b"").unwrap();
    if bound {
        let server = ServerUrl::parse(stub.uri()).unwrap();
        let record = BindingRecord::new(
            server.as_str(),
            &public_url_of(origin.url()).unwrap(),
            potluck::USER,
            potluck::TEAM,
            potluck::WORKSPACE,
            "main",
            role,
        );
        TeamFile::new(&StatePaths::in_dir(&state).team)
            .update(|d| {
                d.put_account(AccountRecord::new(
                    &server,
                    potluck::USER,
                    Some("42".into()),
                    false,
                ));
                d.put_binding(&ws, record)
            })
            .unwrap()
            .unwrap();
    }
    PilotTeam {
        origin,
        root,
        ws,
        state,
        stub,
        token: potluck::fresh_token(),
    }
}
pub fn team_fixture(role: &str) -> PilotTeam {
    team_fixture_with(role, true)
}

/// Polls the tree every 50 ms for up to 30 s until `done` holds, and returns that tree.
pub async fn until(pilot: &Pilot, what: &str, done: impl Fn(&Value) -> bool) -> Value {
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
pub fn name<'a>(tree: &'a Value, id: &str) -> Option<&'a str> {
    find_node(tree, id).and_then(|n| n["name"].as_str())
}
