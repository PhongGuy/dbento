//! Helpers for `dbento team` tests: a per-run token, the CLI with test state, a Potluck stub and git repos.
use dbento_core::paths::StatePaths;
use dbento_core::{AccountRecord, ServerUrl, TeamFile};
use serde_json::{Value, json};
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::process::Output;
use wiremock::{MockServer, ResponseTemplate};

/// A token made fresh for this run; it reaches the CLI only through the environment.
pub fn fresh_token() -> String {
    use base64::Engine as _;
    let mut bytes = Vec::with_capacity(32);
    for i in 0..4u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        );
        bytes.extend_from_slice(&h.finish().to_le_bytes());
    }
    format!(
        "plk_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

/// `dbento team <args>`: state in `state`, the memory token store seeded with `seed`, git isolated
/// from the user's config; run off the runtime so the stub keeps answering. Returns (code, stdout, stderr).
pub async fn team(
    state: &Path,
    seed: Option<&str>,
    args: &[&str],
    stdin: &str,
) -> (i32, String, String) {
    let mut cmd = super::dbento();
    cmd.env("DBENTO_STATE_DIR", state)
        .env("DBENTO_TEST_TOKEN_STORE", "memory")
        .env_remove("DBENTO_TEST_TEAM_TOKEN")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("team")
        .args(args)
        .write_stdin(stdin.to_owned());
    if let Some(t) = seed {
        cmd.env("DBENTO_TEST_TEAM_TOKEN", t);
    }
    let o: Output = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// Records `ada` (token id 42) on `uri`.
pub fn signed_in(state: &Path, uri: &str) {
    let server = ServerUrl::parse(uri).unwrap();
    TeamFile::new(StatePaths::in_dir(state).team)
        .update(|d| d.put_account(AccountRecord::new(&server, "ada", Some("42".into()), false)))
        .unwrap();
}

pub fn team_file(state: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(StatePaths::in_dir(state).team).unwrap()).unwrap()
}

pub fn any_file_contains(dir: &Path, needle: &str) -> bool {
    std::fs::read_dir(dir).unwrap().flatten().any(|e| {
        let p = e.path();
        if p.is_dir() {
            any_file_contains(&p, needle)
        } else {
            std::fs::read(&p).is_ok_and(|b| String::from_utf8_lossy(&b).contains(needle))
        }
    })
}

pub fn info(api: u32) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"version": "0.3.0", "api": api,
        "auth": {"password": true, "oidc": false}, "kinds": {"dbento": {"formats": []}}}))
}

pub fn user(name: &str) -> Value {
    json!({"username": name, "display_name": name, "email": null, "server_admin": false, "disabled": false, "created_at": "x"})
}

pub fn ws_json(git_url: &str, team: &str, slug: &str, role: &str, branch: &str) -> Value {
    json!({"team": team, "slug": slug, "name": slug, "kind": "dbento", "default_branch": branch, "role": role,
           "git_url": git_url, "events_url": "https://t.example/events"})
}

pub async fn hits(s: &MockServer, verb: &str, at: &str) -> usize {
    s.received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == verb && r.url.path() == at)
        .count()
}

/// git as a teammate, isolated from the user's config.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let o = std::process::Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Mate",
            "-c",
            "user.email=mate@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// A bare repo at `<root>/git/acme/shared.git` whose main holds `toml`, and a teammate clone at `<root>/mate`.
pub fn seed(root: &Path, toml: &str) -> PathBuf {
    let bare = root.join("git/acme/shared.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q", "--bare", "-b", "main"]);
    git(root, &["clone", "-q", bare.to_str().unwrap(), "mate"]);
    let mate = root.join("mate");
    std::fs::write(mate.join("dbento.toml"), toml).unwrap();
    git(&mate, &["add", "dbento.toml"]);
    git(&mate, &["commit", "-q", "-m", "seed"]);
    git(&mate, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
    bare
}
