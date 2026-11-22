//! A workspace bound to a file:// origin, the loopback Potluck stub and an in-memory token
//! store: team tests never reach the keychain, the real state dirs or a non-loopback host.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dbento_app::shell::Shell;
use dbento_app::team::TeamHooks;
use dbento_core::paths::StatePaths;
use dbento_core::{
    AccountRecord, BindingRecord, GitOptions, MemoryTokenStore, ServerUrl, SyncConfig, TeamAccount,
    TeamFile, TeamToken, TokenStore, public_url_of,
};
pub use dbento_testkit::potluck::write;
use dbento_testkit::potluck::{self, Origin, Stub};
use gpui_kit::{AnyWindowHandle, Entity, TestAppContext};

pub const TOML: &str = "dbento = 1\n\n[connections.lite]\nengine = \"sqlite\"\npath = \"app.db\"\n";

pub struct TeamFx {
    pub origin: Origin,
    /// The scratch folder holding the origin, `ws` and `state`; removed with `origin`.
    pub root: PathBuf,
    pub ws: PathBuf,
    pub stub: Stub,
    pub paths: StatePaths,
    pub tokens: Arc<MemoryTokenStore>,
}

impl TeamFx {
    pub fn git_options(&self) -> GitOptions {
        self.origin
            .git_env()
            .into_iter()
            .fold(GitOptions::new(), |o, (k, v)| o.env(k, v))
    }
    pub fn server(&self) -> ServerUrl {
        ServerUrl::parse(self.stub.uri()).unwrap()
    }
    pub fn account(&self) -> TeamAccount {
        TeamAccount::new(self.server(), potluck::USER)
    }
    pub fn origin_show(&self, rel: &str) -> Option<String> {
        self.origin.show(rel)
    }
    pub fn teammate(&self, rel: &str, text: &str) {
        self.origin.teammate(rel, text);
    }
    /// Commits `text` to `rel` in the workspace without pushing it.
    pub fn local_commit(&self, rel: &str, text: &str) {
        self.origin.commit_in(&self.ws, rel, text);
    }
    pub fn hook(&self, name: &str, body: &str) {
        self.origin.hook(name, body);
    }
    pub fn hooks(&self) -> TeamHooks {
        let tokens: Arc<dyn TokenStore> = Arc::<MemoryTokenStore>::clone(&self.tokens);
        TeamHooks::new(tokens, self.paths.clone())
            .git(self.git_options())
            .sync(
                SyncConfig::new()
                    .debounce(Duration::from_millis(50))
                    .backoff(Duration::from_millis(20), Duration::from_millis(100))
                    .token_timeout(Duration::from_secs(2)),
            )
    }
}

/// An origin seeded with `TOML` and `queries/a.sql`, and the stub serving it with `role`.
/// `bound` clones it to `ws`, signs ada in and binds the folder; otherwise `ws` is a plain
/// SQLite workspace and the state dir is empty.
pub fn team_fx_with(role: &str, bound: bool) -> TeamFx {
    let origin = Origin::seeded(&[("dbento.toml", TOML), ("queries/a.sql", "select 1;\n")]);
    let root = origin.root().to_path_buf();
    let stub = Stub::start();
    potluck::serve_team(&stub, origin.url(), role);
    let ws = if bound {
        origin.clone_to("ws")
    } else {
        write(&root.join("ws"), "dbento.toml", TOML);
        root.join("ws")
    };
    std::fs::write(ws.join("app.db"), b"").unwrap();
    let fx = TeamFx {
        ws: std::fs::canonicalize(ws).unwrap(),
        paths: StatePaths::in_dir(&root.join("state")),
        tokens: Arc::new(MemoryTokenStore::new()),
        origin,
        root,
        stub,
    };
    if bound {
        let server = fx.server();
        let record = BindingRecord::new(
            server.as_str(),
            &public_url_of(fx.origin.url()).unwrap(),
            potluck::USER,
            potluck::TEAM,
            potluck::WORKSPACE,
            "main",
            role,
        );
        TeamFile::new(&fx.paths.team)
            .update(|d| {
                d.put_account(AccountRecord::new(
                    &server,
                    potluck::USER,
                    Some("42".into()),
                    false,
                ));
                d.put_binding(&fx.ws, record)
            })
            .unwrap()
            .unwrap();
        let token = TeamToken::parse(&potluck::fresh_token()).unwrap();
        fx.tokens.set(&fx.account(), &token).unwrap();
    }
    fx
}
pub fn team_fx(role: &str) -> TeamFx {
    team_fx_with(role, true)
}

pub struct TeamStarted {
    pub files: tempfile::TempDir,
    pub win: AnyWindowHandle,
    pub shell: Entity<Shell>,
}
/// The app on `dir`, with `hooks` installed first when given.
pub fn start_on(cx: &mut TestAppContext, dir: &Path, hooks: Option<TeamHooks>) -> TeamStarted {
    super::init(cx);
    super::init_tokio(cx);
    if let Some(hooks) = hooks {
        cx.update(|cx| dbento_app::team::install(hooks, cx));
    }
    let files = tempfile::tempdir().unwrap();
    let (win, shell) = cx
        .update(|cx| dbento_app::app::start(super::services(files.path()), Some(dir.into()), cx))
        .unwrap();
    super::present(cx, win, "connection-list");
    TeamStarted { files, win, shell }
}
pub fn team_started(cx: &mut TestAppContext, fx: &TeamFx) -> TeamStarted {
    start_on(cx, &fx.ws, Some(fx.hooks()))
}
