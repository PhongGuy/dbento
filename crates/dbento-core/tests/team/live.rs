#![cfg(unix)]
use crate::potluck::{fresh_token, user, ws};
use crate::stub::{Reply, Stub};
use crate::support::{Sandbox, read, real_git, write};
use dbento_core::paths::StatePaths;
use dbento_core::{
    AccountRecord, Binding, BindingRecord, ConflictStore, MemoryTokenStore, ServerUrl, SyncConfig,
    SyncState, SyncStatus, TeamAccount, TeamFile, TeamSync, TokenStore, public_url_of, repo_lock,
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

impl Live {
    fn conflicted(&self, sync: &TeamSync) {
        let from = self.seen.mark();
        self.sb
            .teammate(&self.mate, "dbento.toml", Some("name = \"theirs\"\n"));
        write(&self.ws, "dbento.toml", "name = \"mine\"\n");
        sync.flush();
        let got = self
            .seen
            .wait(from, |s| matches!(s.state, SyncState::Conflicted(_)));
        let SyncState::Conflicted(files) = got.state else {
            unreachable!()
        };
        assert_eq!(
            files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            ["dbento.toml"],
            "one conflict"
        );
        assert_eq!(
            read(&self.ws, "dbento.toml").as_deref(),
            Some("name = \"theirs\"\n"),
            "theirs is on disk"
        );
    }
}

#[test]
fn drop_gives_up_after_its_bound_when_git_hangs() {
    let l = live("write");
    let body = "case \" $* \" in *\" fetch \"*) exec sleep 20;; esac\nexec \"$REAL_GIT\" \"$@\"";
    let git =
        l.sb.options()
            .env("PATH", l.sb.fake_git("slow-bin", body))
            .env("REAL_GIT", real_git());
    let sync = l.start(l.config().git(git).drop_bound(Duration::from_millis(300)));
    l.seen.wait(0, |s| s.state == SyncState::Pulling);
    let started = Instant::now();
    drop(sync);
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(300) && took < Duration::from_millis(1500),
        "{took:?}"
    );
}

#[test]
fn a_held_repo_lock_reports_busy_until_a_retry_and_no_git_is_git_missing() {
    let l = live("write");
    let held = repo_lock(&l.ws, &l.sb.options()).unwrap();
    let sync = l.start(
        l.config()
            .git(l.sb.options().lock_wait(Duration::from_millis(200))),
    );
    l.seen.wait(0, |s| s.state == SyncState::Busy);
    drop(held);
    let from = l.seen.mark();
    sync.retry();
    l.seen.wait(from, |s| s.state == SyncState::Synced);
    drop(sync);
    let empty = l.sb.path().join("no-git");
    std::fs::create_dir(&empty).unwrap();
    let from = l.seen.mark();
    let _sync = l.start(l.config().git(l.sb.options().env("PATH", &empty)));
    l.seen.wait(from, |s| s.state == SyncState::GitMissing);
}
#[test]
fn keep_mine_puts_my_side_back_and_pushes_it() {
    let l = live("write");
    let sync = l.start(l.config());
    l.seen.wait(0, |s| s.state == SyncState::Synced);
    l.conflicted(&sync);
    sync.keep_mine("dbento.toml");
    assert!(eventually(
        || l.origin_show("dbento.toml").as_deref() == Some("name = \"mine\"\n")
    ));
}

#[test]
fn take_theirs_drops_my_copy_and_settles() {
    let l = live("write");
    let sync = l.start(l.config());
    l.seen.wait(0, |s| s.state == SyncState::Synced);
    l.conflicted(&sync);
    let from = l.seen.mark();
    sync.take_theirs("dbento.toml");
    l.seen.wait(from, |s| s.state == SyncState::Synced);
    assert!(
        ConflictStore::for_workspace(&l.paths, &l.ws)
            .unwrap()
            .copies()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        l.origin_show("dbento.toml").as_deref(),
        Some("name = \"theirs\"\n")
    );
}

#[test]
fn a_conflict_outlives_a_restart() {
    let l = live("write");
    let sync = l.start(l.config());
    l.seen.wait(0, |s| s.state == SyncState::Synced);
    l.conflicted(&sync);
    drop(sync);
    let from = l.seen.mark();
    let _again = l.start(l.config());
    l.seen.wait(
        from,
        |s| matches!(&s.state, SyncState::Conflicted(f) if f.len() == 1),
    );
}

#[test]
fn a_refusal_holds_until_the_next_change_and_the_fix_drops_the_secret() {
    let l = live("write");
    let hook = l.bare.join("hooks/pre-receive");
    l.sb.script(&hook, "cat >/dev/null\necho 'potluck-reject secret_value refs/heads/main dbento.toml password' >&2\nexit 1");
    let sync = l.start(l.config());
    let from = l.seen.mark();
    write(&l.ws, "dbento.toml", "password = \"hunter2\"\n");
    sync.flush();
    let got = l
        .seen
        .wait(from, |s| matches!(s.state, SyncState::Refused(_)));
    let SyncState::Refused(r) = got.state else {
        unreachable!()
    };
    assert_eq!(r.findings[0].rule, "password");
    let secret = l.sb.rev(&l.ws, "HEAD:dbento.toml");
    std::fs::remove_file(&hook).unwrap();
    let from = l.seen.mark();
    write(&l.ws, "dbento.toml", "name = \"fixed\"\n");
    sync.changed();
    l.seen.wait(from, |s| s.state == SyncState::Synced);
    assert_eq!(
        l.origin_show("dbento.toml").as_deref(),
        Some("name = \"fixed\"\n")
    );
    let objects = l.sb.git(
        l.sb.path(),
        &[
            "--git-dir",
            l.bare.to_str().unwrap(),
            "rev-list",
            "--objects",
            "--all",
        ],
    );
    assert!(
        !objects.contains(&secret),
        "the refused blob reached the server"
    );
}

impl Seen {
    pub fn count(&self, from: usize, pred: impl Fn(&SyncStatus) -> bool) -> usize {
        self.0.lock().unwrap()[from..]
            .iter()
            .filter(|s| pred(s))
            .count()
    }
}

impl Live {
    /// Pushes go to the stub's smart-HTTP address, which answers `reply`; fetches stay on file://.
    pub fn push_to_stub(&self, reply: Reply) {
        self.stub
            .reply("GET", "/git/acme/shared.git/info/refs", reply);
        self.sb.git(
            &self.ws,
            &[
                "remote",
                "set-url",
                "--push",
                "origin",
                &format!("{}/git/acme/shared.git", self.stub.uri()),
            ],
        );
    }
}

fn ref_frame(branch: &str, sha: &str) -> String {
    format!("event: ref\ndata: {{\"ref\":\"refs/heads/{branch}\",\"sha\":\"{sha}\"}}\n\n")
}

#[test]
fn a_ref_notice_pulls_the_teammates_change_and_another_branch_does_not() {
    let l = live("write");
    let _sync = l.start(
        l.config()
            .poll_every(Duration::from_secs(600))
            .silence(Duration::from_secs(600)),
    );
    l.seen.wait(0, |s| s.state == SyncState::Synced);
    assert!(
        eventually(|| l.stub.hits("GET", EVENTS) >= 1),
        "the reader connected"
    );
    assert!(
        l.stub
            .head("GET", EVENTS)
            .unwrap()
            .contains("authorization: bearer plk_"),
        "the reader sent the token"
    );
    let from = l.seen.mark();
    l.stub.send(&ref_frame("dev", &"1".repeat(40)));
    l.sb.teammate(&l.mate, "dbento.toml", Some("name = \"theirs\"\n"));
    l.stub.send(&ref_frame("main", &l.sb.rev(&l.bare, "main")));
    assert!(
        eventually(|| read(&l.ws, "dbento.toml").as_deref() == Some("name = \"theirs\"\n")),
        "the main notice pulled"
    );
    // Frames arrive in order, so the main pull ran after the dev frame was handled.
    assert_eq!(
        l.seen.count(from, |s| s.state == SyncState::Pulling),
        1,
        "only the main notice pulled"
    );
}

#[test]
fn a_closed_stream_reconnects_and_a_401_on_reconnect_signs_out_for_good() {
    let l = live("write");
    let _sync = l.start(l.config());
    assert!(
        eventually(|| l.stub.hits("GET", EVENTS) >= 1),
        "the reader connected"
    );
    l.stub.close_streams();
    assert!(
        eventually(|| l.stub.hits("GET", EVENTS) >= 2),
        "reconnected after a close"
    );
    let from = l.seen.mark();
    l.stub.reply("GET", EVENTS, Reply::Status(401));
    l.stub.close_streams();
    l.seen.wait(from, |s| s.state == SyncState::SignedOut);
    let hits = l.stub.hits("GET", EVENTS);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(l.stub.hits("GET", EVENTS), hits, "the reader ended");
}

#[test]
fn a_silent_stream_falls_back_to_polling() {
    let l = live("write");
    let _sync = l.start(
        l.config()
            .poll_every(Duration::from_millis(150))
            .silence(Duration::from_millis(300)),
    );
    assert!(
        eventually(|| l.stub.hits("GET", EVENTS) >= 1),
        "the reader connected"
    );
    l.sb.teammate(&l.mate, "dbento.toml", Some("name = \"theirs\"\n"));
    assert!(
        eventually(|| read(&l.ws, "dbento.toml").as_deref() == Some("name = \"theirs\"\n")),
        "a poll pulled"
    );
    assert_eq!(
        l.stub.hits("GET", EVENTS),
        1,
        "the silent stream stayed open"
    );
}

#[test]
fn a_git_401_confirmed_by_me_signs_out_and_saves_stay_local_until_retry() {
    let l = live("write");
    let sync = l.start(l.config());
    l.seen.wait(0, |s| s.state == SyncState::Synced);
    l.push_to_stub(Reply::Status(401));
    l.stub.reply("GET", "/api/v1/me", Reply::Status(401));
    let from = l.seen.mark();
    write(&l.ws, "queries/b.sql", "select 2;\n");
    sync.flush();
    l.seen.wait(from, |s| s.state == SyncState::SignedOut);
    let from = l.seen.mark();
    write(&l.ws, "queries/c.sql", "select 3;\n");
    sync.flush();
    l.seen
        .wait(from, |s| s.state == SyncState::SignedOut && s.unpushed == 2);
    assert_eq!(
        l.sb.git(&l.ws, &["status", "--porcelain"]),
        "",
        "the save was committed locally"
    );
    assert_eq!(
        l.origin_show("queries/c.sql"),
        None,
        "nothing pushed while signed out"
    );
    l.stub
        .reply("GET", "/api/v1/me", Reply::Json(200, me_json()));
    l.sb.git(&l.ws, &["remote", "set-url", "--push", "origin", &l.url]);
    let from = l.seen.mark();
    sync.retry();
    l.seen.wait(from, |s| s.state == SyncState::Synced);
    assert_eq!(
        l.origin_show("queries/c.sql").as_deref(),
        Some("select 3;\n"),
        "the retry pushed the saves"
    );
}

#[test]
fn a_push_403_after_a_demotion_lands_read_only_with_the_save_kept() {
    let l = live("write");
    let sync = l.start(l.config());
    l.seen.wait(0, |s| s.state == SyncState::Synced);
    l.push_to_stub(Reply::Status(403));
    l.stub.reply(
        "GET",
        "/api/v1/workspaces",
        Reply::Json(200, l.listed("read")),
    );
    let from = l.seen.mark();
    write(&l.ws, "queries/b.sql", "select 2;\n");
    sync.flush();
    assert_eq!(
        l.seen
            .wait(from, |s| s.state == SyncState::ReadOnly)
            .unpushed,
        1,
        "the save is kept as a local commit"
    );
    assert_eq!(
        l.origin_show("queries/b.sql"),
        None,
        "nothing reached the server"
    );
}

#[test]
fn an_unreadable_conflict_store_fails_instead_of_showing_synced() {
    let l = live("write");
    let sync = l.start(l.config());
    l.seen.wait(0, |s| s.state == SyncState::Synced);
    // A file where the store's folder belongs makes every listing fail.
    let _ = std::fs::remove_dir_all(&l.paths.team_conflicts);
    std::fs::write(&l.paths.team_conflicts, "").unwrap();
    let from = l.seen.mark();
    write(&l.ws, "queries/b.sql", "select 2;\n");
    sync.flush();
    l.seen
        .wait(from, |s| matches!(s.state, SyncState::Failed(_)));
}

#[test]
fn a_read_folder_whose_fetch_fails_reports_failed_not_read_only() {
    let l = live("read");
    let body = "case \" $* \" in *\" fetch \"*) echo 'fatal: the server broke' >&2; exit 1;; esac\nexec \"$REAL_GIT\" \"$@\"";
    let git =
        l.sb.options()
            .env("PATH", l.sb.fake_git("broken-bin", body))
            .env("REAL_GIT", real_git());
    let _sync = l.start(l.config().git(git));
    let got = l.seen.wait(0, |s| {
        matches!(s.state, SyncState::Failed(_) | SyncState::ReadOnly)
    });
    assert!(
        matches!(got.state, SyncState::Failed(_)),
        "the fetch error is shown: {got:?}"
    );
}
