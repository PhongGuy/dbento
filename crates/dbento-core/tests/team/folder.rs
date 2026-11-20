#![cfg(unix)]
use crate::potluck::{answer, blocking, fresh_token, ws};
use crate::support::{Sandbox, read, write};
use dbento_core::paths::StatePaths;
use dbento_core::{
    AccountRecord, FolderSync, FolderSyncError, JoinError, MemoryTokenStore, Resolution, ServerUrl,
    SyncOutcome, TeamAccount, TeamFile, TeamResolveError as ResolveError, TokenStore, team_join,
    team_resolve, team_sync,
};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wiremock::{MockServer, ResponseTemplate};

const TOML: &str = "dbento = 1\n[connections.prod]\nengine = \"postgres\"\nhost = \"db.example\"\ndatabase = \"app\"\nuser = \"app\"\n";

struct Env {
    sb: Sandbox,
    s: MockServer,
    paths: StatePaths,
    tokens: Arc<MemoryTokenStore>,
    bare: PathBuf,
    ws: PathBuf,
    mate: PathBuf,
}

async fn list(s: &MockServer, bare: &Path, role: &str) {
    s.reset().await;
    answer(
        s,
        "GET",
        "/api/v1/workspaces",
        ResponseTemplate::new(200).set_body_json(json!([ws(
            &format!("file://{}", bare.display()),
            "acme",
            "shared",
            role
        )])),
    )
    .await;
}

/// Plan 1's origin re-homed at the Potluck path shape, holding TOML; ada signed in and joined into `ws`.
async fn joined() -> Env {
    let sb = Sandbox::new();
    sb.origin();
    let mate = sb.clone_as("mate");
    sb.teammate(&mate, "dbento.toml", Some(TOML));
    sb.git(
        sb.path(),
        &["clone", "-q", "--bare", "origin.git", "git/acme/shared.git"],
    );
    let bare = sb.path().join("git/acme/shared.git");
    sb.git(
        &mate,
        &[
            "remote",
            "set-url",
            "origin",
            &format!("file://{}", bare.display()),
        ],
    );
    let s = MockServer::start().await;
    list(&s, &bare, "write").await;
    let paths = StatePaths::in_dir(&sb.path().join("state"));
    let server = ServerUrl::parse(&s.uri()).unwrap();
    TeamFile::new(&paths.team)
        .update(|d| d.put_account(AccountRecord::new(&server, "ada", Some("42".into()), false)))
        .unwrap();
    let tokens = Arc::new(MemoryTokenStore::new());
    tokens
        .set(&TeamAccount::new(server.clone(), "ada"), &fresh_token())
        .unwrap();
    let ws = sb.path().join("ws");
    let (t, p, w, o) = (Arc::clone(&tokens), paths.clone(), ws.clone(), sb.options());
    blocking(move || team_join(t.as_ref(), &p, &server, None, "acme/shared", &w, &o))
        .await
        .unwrap();
    Env {
        sb,
        s,
        paths,
        tokens,
        bare,
        ws,
        mate,
    }
}

impl Env {
    async fn sync(&self) -> Result<FolderSync, FolderSyncError> {
        let (t, p, w, o) = (
            Arc::clone(&self.tokens),
            self.paths.clone(),
            self.ws.clone(),
            self.sb.options(),
        );
        blocking(move || team_sync(t.as_ref(), &p, &w, &o)).await
    }
    async fn resolve(&self, rel: &str, c: Resolution) -> Result<Option<FolderSync>, ResolveError> {
        let (t, p, w, r, o) = (
            Arc::clone(&self.tokens),
            self.paths.clone(),
            self.ws.clone(),
            rel.to_owned(),
            self.sb.options(),
        );
        blocking(move || team_resolve(t.as_ref(), &p, &w, &r, c, &o)).await
    }
    async fn join(&self, name: &'static str, dir: PathBuf) -> Result<(), JoinError> {
        let (t, p, s, o) = (
            Arc::clone(&self.tokens),
            self.paths.clone(),
            ServerUrl::parse(&self.s.uri()).unwrap(),
            self.sb.options(),
        );
        blocking(move || team_join(t.as_ref(), &p, &s, None, name, &dir, &o).map(|_| ())).await
    }
    fn origin_show(&self, rel: &str) -> String {
        self.sb.git(
            self.sb.path(),
            &[
                "--git-dir",
                self.bare.to_str().unwrap(),
                "show",
                &format!("main:{rel}"),
            ],
        )
    }
    /// dbento.toml changed on both sides; returns (mine, theirs) after the conflicted sync.
    async fn conflicted(&self) -> (String, String) {
        let (mine, theirs) = (
            TOML.replace("db.example", "mine.example"),
            TOML.replace("db.example", "theirs.example"),
        );
        self.sb.teammate(&self.mate, "dbento.toml", Some(&theirs));
        write(&self.ws, "dbento.toml", &mine);
        let done = self.sync().await.unwrap();
        let SyncOutcome::Conflicted(files) = &done.outcome else {
            panic!("{:?}", done.outcome)
        };
        assert_eq!(
            (files.len(), files[0].path.as_str()),
            (1, "dbento.toml"),
            "one conflict on dbento.toml"
        );
        assert_eq!(
            read(&self.ws, "dbento.toml").unwrap(),
            theirs,
            "the server's side is on disk"
        );
        (mine, theirs)
    }
}

#[tokio::test]
async fn join_clones_writes_gitignore_and_records_the_binding() {
    let e = joined().await;
    assert_eq!(read(&e.ws, "dbento.toml").unwrap(), TOML, "cloned");
    assert!(
        read(&e.ws, ".gitignore").unwrap().contains("*.sqlite"),
        "join .gitignore written"
    );
    let (_, b) = TeamFile::new(&e.paths.team)
        .load()
        .unwrap()
        .binding_for(&e.ws)
        .unwrap();
    assert_eq!(
        (
            b.team.as_str(),
            b.workspace.as_str(),
            b.branch.as_str(),
            b.role.as_str()
        ),
        ("acme", "shared", "main", "write"),
        "binding names the workspace"
    );
    assert_eq!(
        b.public_url,
        format!("file://{}", e.sb.path().display()),
        "public url"
    );
    assert!(
        !read(&e.ws, ".git/config").unwrap().contains("plk_"),
        "no token in .git/config"
    );
}

#[tokio::test]
async fn join_refuses_a_non_empty_folder_and_an_unknown_workspace() {
    let e = joined().await;
    let busy = e.sb.path().join("busy");
    write(&busy, "notes.txt", "keep");
    let err = e.join("acme/shared", busy.clone()).await.unwrap_err();
    assert!(matches!(err, JoinError::NotEmpty(_)), "{err:?}");
    let err = e
        .join("acme/nope", e.sb.path().join("fresh"))
        .await
        .unwrap_err();
    assert!(matches!(err, JoinError::NoSuchWorkspace(_)), "{err:?}");
    assert!(
        !busy.join(".git").exists() && !e.sb.path().join("fresh").exists(),
        "neither folder was touched"
    );
    assert_eq!(
        TeamFile::new(&e.paths.team)
            .load()
            .unwrap()
            .bindings()
            .len(),
        1,
        "no binding added"
    );
}

#[tokio::test]
async fn join_takes_over_a_folder_already_cloned_and_refuses_a_bound_one() {
    let e = joined().await;
    let url = format!("file://{}", e.bare.display());
    e.sb.git(e.sb.path(), &["clone", "-q", "-b", "main", &url, "half"]);
    let half = e.sb.path().join("half");
    e.join("acme/shared", half.clone()).await.unwrap();
    assert!(
        read(&half, ".gitignore").unwrap().contains("*.sqlite"),
        "the unfinished setup was done"
    );
    assert_eq!(
        TeamFile::new(&e.paths.team)
            .load()
            .unwrap()
            .bindings()
            .len(),
        2,
        "the folder is bound"
    );
    let err = e.join("acme/shared", e.ws.clone()).await.unwrap_err();
    assert!(matches!(err, JoinError::AlreadyBound(_)), "{err:?}");
}

#[tokio::test]
async fn sync_pushes_local_work_and_reports_a_teammate_rename() {
    let e = joined().await;
    e.sb.git(&e.mate, &["pull", "-q", "--rebase"]);
    e.sb.teammate(
        &e.mate,
        "dbento.toml",
        Some(&TOML.replace("[connections.prod]", "[connections.main]")),
    );
    write(&e.ws, "queries/b.sql", "select 2;\n");
    let done = e.sync().await.unwrap();
    assert!(
        matches!(done.outcome, SyncOutcome::Synced),
        "{:?}",
        done.outcome
    );
    assert_eq!(
        done.renames
            .iter()
            .map(|r| (r.from.as_str(), r.to.as_str()))
            .collect::<Vec<_>>(),
        [("prod", "main")],
        "the rename is reported"
    );
    assert_eq!(e.origin_show("queries/b.sql"), "select 2;\n", "pushed");
    assert!(
        TeamFile::new(&e.paths.team)
            .load()
            .unwrap()
            .binding_for(&e.ws)
            .unwrap()
            .1
            .last_synced
            .is_some(),
        "last_synced recorded"
    );
}

#[tokio::test]
async fn a_demoted_member_syncs_read_only() {
    let e = joined().await;
    list(&e.s, &e.bare, "read").await;
    let done = e.sync().await.unwrap();
    assert!(
        matches!(done.outcome, SyncOutcome::ReadOnly),
        "{:?}",
        done.outcome
    );
    assert_eq!(done.binding.role, "read", "the new role is returned");
}

#[tokio::test]
async fn sync_needs_a_binding_and_a_token() {
    let e = joined().await;
    let (t, p, other, o) = (
        Arc::clone(&e.tokens),
        e.paths.clone(),
        e.sb.path().to_path_buf(),
        e.sb.options(),
    );
    assert!(
        matches!(
            blocking(move || team_sync(t.as_ref(), &p, &other, &o))
                .await
                .unwrap_err(),
            FolderSyncError::NotBound
        ),
        "an unbound folder"
    );
    e.tokens
        .delete(&TeamAccount::new(
            ServerUrl::parse(&e.s.uri()).unwrap(),
            "ada",
        ))
        .unwrap();
    assert!(
        matches!(
            e.sync().await.unwrap_err(),
            FolderSyncError::SignedOut { .. }
        ),
        "no token"
    );
}

#[tokio::test]
async fn a_git_401_confirmed_by_me_is_signed_out() {
    let e = joined().await;
    e.sb.git(
        &e.ws,
        &[
            "remote",
            "set-url",
            "origin",
            &format!("{}/git/acme/shared.git", e.s.uri()),
        ],
    );
    answer(
        &e.s,
        "GET",
        "/git/acme/shared.git/info/refs",
        ResponseTemplate::new(401).insert_header("www-authenticate", "Basic realm=\"potluck\""),
    )
    .await;
    answer(&e.s, "GET", "/api/v1/me", ResponseTemplate::new(401)).await;
    assert!(
        matches!(
            e.sync().await.unwrap_err(),
            FolderSyncError::SignedOut { .. }
        ),
        "signed out"
    );
}

#[tokio::test]
async fn keep_mine_writes_my_copy_back_and_pushes_it() {
    let e = joined().await;
    let (mine, _) = e.conflicted().await;
    let done = e
        .resolve("dbento.toml", Resolution::KeepMine)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(done.outcome, SyncOutcome::Synced),
        "{:?}",
        done.outcome
    );
    assert_eq!(
        e.origin_show("dbento.toml"),
        mine,
        "mine reached the server"
    );
}

#[tokio::test]
async fn take_theirs_drops_the_saved_copy_once_and_bad_paths_are_refused() {
    let e = joined().await;
    let (_, theirs) = e.conflicted().await;
    assert!(
        e.resolve("dbento.toml", Resolution::TakeTheirs)
            .await
            .unwrap()
            .is_none(),
        "take theirs does not sync"
    );
    assert!(
        matches!(
            e.resolve("dbento.toml", Resolution::TakeTheirs)
                .await
                .unwrap_err(),
            ResolveError::NoSavedCopy(_)
        ),
        "the copy is gone"
    );
    assert_eq!(read(&e.ws, "dbento.toml").unwrap(), theirs, "theirs stays");
    for bad in ["../x", "/etc/passwd", ""] {
        assert!(
            matches!(
                e.resolve(bad, Resolution::KeepMine).await.unwrap_err(),
                ResolveError::BadPath(_)
            ),
            "{bad}"
        );
    }
}
