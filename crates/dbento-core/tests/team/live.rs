#![cfg(unix)]
#![allow(dead_code)] // the fixture also serves Tasks 7 and 8
use crate::potluck::{fresh_token, user, ws};
use crate::stub::{Reply, Stub};
use crate::support::{Sandbox, write};
use dbento_core::paths::StatePaths;
use dbento_core::{
    AccountRecord, Binding, BindingRecord, MemoryTokenStore, ServerUrl, SyncConfig, SyncState,
    SyncStatus, TeamAccount, TeamFile, TeamSync, TokenStore, public_url_of,
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Default)]
pub struct Seen(Arc<Mutex<Vec<SyncStatus>>>);
impl Seen {
    fn hook(&self) -> impl Fn(SyncStatus) + Send + 'static {
        let seen = self.clone();
        move |s| seen.0.lock().unwrap().push(s)
    }
    pub fn mark(&self) -> usize {
        self.0.lock().unwrap().len()
    }
    /// The first status after `from` that `pred` accepts, waiting up to 15 s.
    pub fn wait(&self, from: usize, pred: impl Fn(&SyncStatus) -> bool) -> SyncStatus {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(s) = self.0.lock().unwrap()[from..].iter().find(|s| pred(s)) {
                return s.clone();
            }
            assert!(
                Instant::now() < deadline,
                "no such status; saw {:?}",
                self.0.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

pub fn eventually(mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

pub const EVENTS: &str = "/api/v1/workspaces/acme/shared/events";
pub fn me_json() -> String {
    json!({"user": user("ada"), "identity": {"name": "ada", "email": "x@x"}, "workspaces": []})
        .to_string()
}

pub struct Live {
    pub sb: Sandbox,
    pub stub: Stub,
    pub paths: StatePaths,
    pub tokens: Arc<MemoryTokenStore>,
    pub bare: PathBuf,
    pub url: String,
    pub ws: PathBuf,
    pub mate: PathBuf,
    pub seen: Seen,
}

/// Origin at the Potluck path shape, `ws` cloned and bound for ada with `role`, and the stub
/// serving the workspace list, `me` and an open event stream.
pub fn live(role: &str) -> Live {
    let sb = Sandbox::new();
    sb.origin();
    let mate = sb.clone_as("mate");
    sb.git(
        sb.path(),
        &["clone", "-q", "--bare", "origin.git", "git/acme/shared.git"],
    );
    let bare = sb.path().join("git/acme/shared.git");
    let url = format!("file://{}", bare.display());
    sb.git(&mate, &["remote", "set-url", "origin", &url]);
    sb.git(sb.path(), &["clone", "-q", "-b", "main", &url, "ws"]);
    let (ws_dir, stub) = (sb.path().join("ws"), Stub::start());
    let paths = StatePaths::in_dir(&sb.path().join("state"));
    let server = ServerUrl::parse(stub.uri()).unwrap();
    let record = BindingRecord::new(
        server.as_str(),
        &public_url_of(&url).unwrap(),
        "ada",
        "acme",
        "shared",
        "main",
        role,
    );
    TeamFile::new(&paths.team)
        .update(|d| {
            d.put_account(AccountRecord::new(&server, "ada", Some("42".into()), false));
            d.put_binding(&ws_dir, record)
        })
        .unwrap()
        .unwrap();
    let tokens = Arc::new(MemoryTokenStore::new());
    tokens
        .set(&TeamAccount::new(server, "ada"), &fresh_token())
        .unwrap();
    let l = Live {
        sb,
        stub,
        paths,
        tokens,
        bare,
        url,
        ws: ws_dir,
        mate,
        seen: Seen::default(),
    };
    l.stub.reply(
        "GET",
        "/api/v1/workspaces",
        Reply::Json(200, l.listed(role)),
    );
    l.stub
        .reply("GET", "/api/v1/me", Reply::Json(200, me_json()));
    l.stub.reply("GET", EVENTS, Reply::Stream);
    l
}

impl Live {
    pub fn listed(&self, role: &str) -> String {
        json!([ws(&self.url, "acme", "shared", role)]).to_string()
    }
    pub fn binding(&self) -> Binding {
        TeamFile::new(&self.paths.team)
            .load()
            .unwrap()
            .binding_for(&self.ws)
            .unwrap()
            .1
            .to_binding(&self.ws)
    }
    pub fn config(&self) -> SyncConfig {
        SyncConfig::new()
            .debounce(Duration::from_millis(50))
            .backoff(Duration::from_millis(20), Duration::from_millis(100))
            .token_timeout(Duration::from_secs(2))
            .git(self.sb.options())
    }
    pub fn start(&self, config: SyncConfig) -> TeamSync {
        let tokens: Arc<dyn TokenStore> = Arc::<MemoryTokenStore>::clone(&self.tokens);
        TeamSync::start_with(
            self.binding(),
            tokens,
            &self.paths,
            config,
            self.seen.hook(),
        )
        .unwrap()
    }
    pub fn origin_show(&self, rel: &str) -> Option<String> {
        let out = std::process::Command::new("git")
            .envs(self.sb.env())
            .arg("--git-dir")
            .arg(&self.bare)
            .args(["show", &format!("main:{rel}")])
            .output()
            .unwrap();
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

#[test]
fn the_handle_can_be_shared_across_threads() {
    fn shared<T: Send + Sync>() {}
    shared::<TeamSync>();
}

#[test]
fn start_syncs_and_a_change_is_pushed_after_the_debounce() {
    let l = live("write");
    let sync = l.start(l.config());
    assert!(
        l.seen
            .wait(0, |s| s.state == SyncState::Synced)
            .last_synced
            .is_some()
    );
    let from = l.seen.mark();
    write(&l.ws, "queries/b.sql", "select 2;\n");
    sync.changed();
    l.seen.wait(from, |s| s.state == SyncState::Pushing);
    assert_eq!(
        l.seen.wait(from, |s| s.state == SyncState::Synced).unpushed,
        0
    );
    assert_eq!(
        l.origin_show("queries/b.sql").as_deref(),
        Some("select 2;\n")
    );
}

#[test]
fn flush_skips_a_long_debounce_and_drop_flushes_what_is_pending() {
    let l = live("write");
    let sync = l.start(l.config().debounce(Duration::from_secs(600)));
    l.seen.wait(0, |s| s.state == SyncState::Synced);
    write(&l.ws, "queries/b.sql", "select 2;\n");
    sync.flush();
    assert!(
        eventually(|| l.origin_show("queries/b.sql").is_some()),
        "flush pushed at once"
    );
    write(&l.ws, "queries/c.sql", "select 3;\n");
    sync.changed();
    drop(sync);
    assert_eq!(
        l.origin_show("queries/c.sql").as_deref(),
        Some("select 3;\n"),
        "dropping flushed"
    );
}
