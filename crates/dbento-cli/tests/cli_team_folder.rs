use crate::common::team::*;
use serde_json::json;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TOML: &str = "dbento = 1\n[connections.prod]\nengine = \"postgres\"\nhost = \"db.example\"\ndatabase = \"app\"\nuser = \"app\"\n";

struct Env {
    root: tempfile::TempDir,
    s: MockServer,
    state: PathBuf,
    ws: PathBuf,
    bare: PathBuf,
    tok: String,
}

async fn list(s: &MockServer, bare: &Path, role: &str) {
    Mock::given(method("GET"))
        .and(path("/api/v1/workspaces"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([ws_json(
            &format!("file://{}", bare.display()),
            "acme",
            "shared",
            role,
            "main"
        )])))
        .mount(s)
        .await;
}

async fn joined() -> Env {
    joined_as("write").await
}

async fn joined_as(role: &str) -> Env {
    let root = tempfile::tempdir().unwrap();
    let bare = seed(root.path(), TOML);
    let s = MockServer::start().await;
    list(&s, &bare, role).await;
    let state = root.path().join("state");
    signed_in(&state, &s.uri());
    let (tok, ws) = (fresh_token(), root.path().join("ws"));
    let (code, _, stderr) = team(
        &state,
        Some(&tok),
        &["join", &s.uri(), "acme/shared", ws.to_str().unwrap()],
        "",
    )
    .await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("joined acme/shared"), "{stderr}");
    Env {
        root,
        s,
        state,
        ws,
        bare,
        tok,
    }
}

impl Env {
    async fn run(&self, seed: Option<&str>, args: &[&str]) -> (i32, String, String) {
        let mut all = vec!["--workspace", self.ws.to_str().unwrap()];
        all.extend_from_slice(args);
        team(&self.state, seed, &all, "").await
    }

    fn mate_pushes(&self, toml: &str) {
        let mate = self.root.path().join("mate");
        git(&mate, &["pull", "-q", "--rebase", "origin", "main"]);
        std::fs::write(mate.join("dbento.toml"), toml).unwrap();
        git(&mate, &["commit", "-q", "-am", "mate"]);
        git(&mate, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
    }

    fn origin_show(&self, rel: &str) -> String {
        git(&self.bare, &["show", &format!("main:{rel}")])
    }
}

#[tokio::test]
async fn join_then_status_shows_the_binding_and_no_token_on_disk() {
    let e = joined().await;
    assert!(
        e.ws.join(".gitignore").exists(),
        "join commits a .gitignore"
    );
    assert!(
        !any_file_contains(&e.state, &e.tok) && !any_file_contains(&e.ws, &e.tok),
        "the token is in no file"
    );
    let (code, stdout, stderr) = e.run(Some(&e.tok), &["status"]).await;
    assert_eq!(code, 0, "{stderr}");
    for want in [
        "workspace: acme/shared",
        "branch:    main",
        "role:      write",
        "ada (signed in)",
        // The join's .gitignore commit waits for the first sync to push it.
        "unpushed:  1",
        "last sync: never",
    ] {
        assert!(stdout.contains(want), "{want} in {stdout}");
    }
    assert!(
        e.run(None, &["status"])
            .await
            .1
            .contains("ada (signed out)"),
        "no token reads as signed out"
    );
}

#[tokio::test]
async fn join_into_a_non_empty_folder_is_refused_and_help_names_the_import() {
    let e = joined().await;
    let busy = e.root.path().join("busy");
    std::fs::create_dir(&busy).unwrap();
    std::fs::write(busy.join("x"), "x").unwrap();
    assert_eq!(
        team(
            &e.state,
            Some(&e.tok),
            &["join", &e.s.uri(), "acme/shared", busy.to_str().unwrap()],
            ""
        )
        .await
        .0,
        1,
        "a non-empty folder is refused"
    );
    assert!(!busy.join(".git").exists(), "nothing was cloned into it");
    let (_, help, _) = team(&e.state, None, &["join", "--help"], "").await;
    assert!(
        help.contains("potluck admin workspace create --import"),
        "{help}"
    );
}

#[tokio::test]
async fn sync_pushes_prints_renames_and_records_the_time() {
    let e = joined().await;
    e.mate_pushes(&TOML.replace("[connections.prod]", "[connections.main]"));
    std::fs::create_dir_all(e.ws.join("queries")).unwrap();
    std::fs::write(e.ws.join("queries/a.sql"), "select 1;\n").unwrap();
    let (code, stdout, stderr) = e.run(Some(&e.tok), &["sync"]).await;
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.contains("synced acme/shared"), "{stdout}");
    assert!(
        stdout.contains("renamed connection 'prod' to 'main'"),
        "{stdout}"
    );
    assert_eq!(
        e.origin_show("queries/a.sql"),
        "select 1;\n",
        "the new query reached the server"
    );
    let (_, status, _) = e.run(Some(&e.tok), &["status"]).await;
    assert!(!status.contains("last sync: never"), "{status}");
    assert!(status.contains("unpushed:  0"), "{status}");
}

#[tokio::test]
async fn sync_exit_codes_for_signed_out_and_unbound() {
    let e = joined().await;
    assert_eq!(e.run(None, &["sync"]).await.0, 5, "no token is signed out");
    e.s.reset().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/workspaces"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&e.s)
        .await;
    assert_eq!(
        e.run(Some(&e.tok), &["sync"]).await.0,
        5,
        "a rejected token is signed out"
    );
    let (code, _, stderr) = team(
        &e.state,
        Some(&e.tok),
        &["--workspace", e.root.path().to_str().unwrap(), "sync"],
        "",
    )
    .await;
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("dbento team join"), "{stderr}");
}

#[tokio::test]
async fn a_refused_push_prints_every_finding_and_exits_2() {
    let e = joined().await;
    let hook = e.bare.join("hooks/pre-receive");
    // A child shell writes the hook, so no open descriptor of ours can make its exec fail.
    let body = "#!/bin/sh\necho 'potluck-reject secret refs%2Fheads%2Fmain dbento.toml password' >&2\n\
                echo 'potluck-reject-detail line%203' >&2\necho 'potluck-reject-more 2' >&2\nexit 1\n";
    let ok = std::process::Command::new("sh")
        .args([
            "-c",
            "printf '%s' \"$1\" > \"$2\" && chmod 755 \"$2\"",
            "sh",
            body,
        ])
        .arg(&hook)
        .status()
        .unwrap();
    assert!(ok.success(), "the hook was written");
    std::fs::write(
        e.ws.join("dbento.toml"),
        TOML.replace("db.example", "x.example"),
    )
    .unwrap();
    let (code, stdout, stderr) = e.run(Some(&e.tok), &["sync"]).await;
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stdout.contains("dbento.toml") && stdout.contains("line 3"),
        "{stdout}"
    );
    assert!(stdout.contains("and 2 more"), "{stdout}");
}

#[tokio::test]
async fn a_conflict_exits_4_then_keep_mine_pushes_my_copy_and_unlink_keeps_the_folder() {
    let e = joined().await;
    let (mine, theirs) = (
        TOML.replace("db.example", "mine.example"),
        TOML.replace("db.example", "theirs.example"),
    );
    e.mate_pushes(&theirs);
    std::fs::write(e.ws.join("dbento.toml"), &mine).unwrap();
    let (code, stdout, stderr) = e.run(Some(&e.tok), &["sync"]).await;
    assert_eq!(code, 4, "{stderr}");
    assert!(
        stdout.contains("conflict: dbento.toml") && stdout.contains("git diff --no-index"),
        "{stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(e.ws.join("dbento.toml")).unwrap(),
        theirs,
        "the server's side is checked out"
    );
    let (code, _, stderr) = e
        .run(Some(&e.tok), &["resolve", "dbento.toml", "--keep-mine"])
        .await;
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        e.origin_show("dbento.toml"),
        mine,
        "keeping mine pushed my copy"
    );
    assert_eq!(e.run(None, &["unlink"]).await.0, 0, "unlink succeeds");
    assert!(
        e.ws.join(".git").is_dir() && e.ws.join("dbento.toml").is_file(),
        "the folder and its history stay"
    );
    let (code, _, stderr) = e.run(None, &["status"]).await;
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("not a team workspace"), "{stderr}");
}

#[tokio::test]
async fn a_read_member_keeps_following_the_server() {
    let e = joined_as("read").await;
    let theirs = TOML.replace("db.example", "theirs.example");
    e.mate_pushes(&theirs);
    let (code, stdout, stderr) = e.run(Some(&e.tok), &["sync"]).await;
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains("read-only: acme/shared is up to date with the server"),
        "{stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(e.ws.join("dbento.toml")).unwrap(),
        theirs,
        "the teammate's change arrived"
    );
    let (_, status, _) = e.run(Some(&e.tok), &["status"]).await;
    assert!(status.contains("unpushed:  0"), "{status}");
}

#[tokio::test]
async fn a_read_member_with_a_colliding_edit_is_told_why_nothing_was_pulled() {
    let e = joined_as("read").await;
    e.mate_pushes(&TOML.replace("db.example", "theirs.example"));
    let mine = TOML.replace("db.example", "mine.example");
    std::fs::write(e.ws.join("dbento.toml"), &mine).unwrap();
    let (code, stdout, stderr) = e.run(Some(&e.tok), &["sync"]).await;
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stdout.contains(
            "read-only: acme/shared not updated; local edits block updates (dbento.toml)"
        ),
        "{stdout}"
    );
    assert!(!stdout.contains("pulled"), "{stdout}");
    assert_eq!(
        std::fs::read_to_string(e.ws.join("dbento.toml")).unwrap(),
        mine
    );
}

fn origin_has(bare: &Path, rel: &str, text: &str) -> bool {
    Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("--git-dir")
        .arg(bare)
        .args(["show", &format!("main:{rel}")])
        .output()
        .is_ok_and(|o| o.status.success() && o.stdout == text.as_bytes())
}

#[tokio::test]
async fn watch_pushes_a_saved_file_and_exits_0_on_ctrl_c() {
    let e = joined().await;
    let (state, ws, bare, tok) = (e.state.clone(), e.ws.clone(), e.bare.clone(), e.tok.clone());
    let mine = TOML.replace("db.example", "watched.example");
    // Off the runtime, so the wiremock server keeps answering the child.
    let (code, out) = tokio::task::spawn_blocking(move || {
        let mut child = Command::new(assert_cmd::cargo::cargo_bin("dbento"))
            .env("DBENTO_STATE_DIR", &state)
            .env("DBENTO_TEST_TOKEN_STORE", "memory")
            .env("DBENTO_TEST_TEAM_TOKEN", &tok)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("DBENTO_TEST_ASSUME_TTY")
            .args([
                "team",
                "--workspace",
                ws.to_str().unwrap(),
                "sync",
                "--watch",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, lines) = std::sync::mpsc::channel();
        let stdout = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx.send(line);
            }
        });
        let mut out = Vec::new();
        loop {
            let line = lines
                .recv_timeout(Duration::from_secs(30))
                .unwrap_or_else(|_| panic!("no first sync: {out:?}"));
            let hit = line.contains("synced acme/shared");
            out.push(line);
            if hit {
                break;
            }
        }
        std::fs::write(ws.join("dbento.toml"), &mine).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !origin_has(&bare, "dbento.toml", &mine) {
            assert!(
                Instant::now() < deadline,
                "the save never reached the server: {out:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        // The push is a new end state of its own, so it prints a second synced line.
        loop {
            let line = lines
                .recv_timeout(Duration::from_secs(10))
                .unwrap_or_else(|_| panic!("no line for the push: {out:?}"));
            let hit = line.contains("synced acme/shared");
            out.push(line);
            if hit {
                break;
            }
        }
        assert!(
            Command::new("kill")
                .args(["-INT", &child.id().to_string()])
                .status()
                .unwrap()
                .success(),
            "kill -INT failed"
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(s) = child.try_wait().unwrap() {
                break s;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!("--watch did not stop: {out:?}");
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        (status.code(), out)
    })
    .await
    .unwrap();
    assert_eq!(code, Some(0), "{out:?}");
}

#[tokio::test]
async fn watch_exits_5_when_signed_out_and_1_when_unbound() {
    let e = joined().await;
    let (code, _, stderr) = e.run(None, &["sync", "--watch"]).await;
    assert_eq!(code, 5, "{stderr}");
    assert!(stderr.contains("signed out"), "{stderr}");
    let elsewhere = e.root.path().to_str().unwrap().to_owned();
    let (code, _, stderr) = team(
        &e.state,
        Some(&e.tok),
        &["--workspace", &elsewhere, "sync", "--watch"],
        "",
    )
    .await;
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("not a team workspace"), "{stderr}");
}
