use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dbento_core::{SAVE_MESSAGE, SyncError, commit_pending, repo_lock};

use crate::support::{Sandbox, binding, write};

fn workspace(sb: &Sandbox) -> PathBuf {
    sb.origin();
    sb.clone_as("w")
}

fn commit(sb: &Sandbox, w: &Path) -> Result<bool, SyncError> {
    commit_pending(&binding(w), SAVE_MESSAGE, &sb.options())
}

#[test]
fn nothing_changed_commits_nothing() {
    let sb = Sandbox::new();
    assert!(!commit(&sb, &workspace(&sb)).unwrap());
}

#[test]
fn a_seed_without_queries_commits_dbento_toml() {
    let sb = Sandbox::new();
    let w = workspace(&sb);
    sb.git(&w, &["rm", "-rq", "queries"]);
    sb.git(&w, &["commit", "-qm", "drop queries"]);
    write(&w, "dbento.toml", "name = \"edited\"\n");
    assert!(commit(&sb, &w).unwrap());
    assert_eq!(
        sb.git(&w, &["log", "-1", "--format=%s"]).trim(),
        "Save changes"
    );
    assert_eq!(sb.git(&w, &["status", "--porcelain"]), "");
}

#[test]
fn only_the_workspace_paths_are_committed() {
    let sb = Sandbox::new();
    let w = workspace(&sb);
    write(&w, "queries/new.sql", "select 2;\n");
    write(&w, "local.db", "SQLite format 3");
    write(&w, "notes.txt", "mine\n");
    write(&w, "staged.txt", "by hand\n");
    sb.git(&w, &["add", "staged.txt"]);
    assert!(commit(&sb, &w).unwrap());
    let files = sb.git(&w, &["show", "--name-only", "--format=", "HEAD"]);
    assert_eq!(files.trim(), "queries/new.sql");
    let status = sb.git(&w, &["status", "--porcelain"]);
    for line in ["A  staged.txt", "?? local.db", "?? notes.txt"] {
        assert!(status.contains(line), "{line} missing: {status}");
    }
}

#[test]
fn a_removed_queries_folder_is_committed_as_deletes() {
    let sb = Sandbox::new();
    let w = workspace(&sb);
    fs::remove_dir_all(w.join("queries")).unwrap();
    assert!(commit(&sb, &w).unwrap());
    assert_eq!(sb.git(&w, &["ls-files", "queries"]), "");
}

#[test]
fn the_username_is_the_fallback_and_the_config_identity_wins() {
    let sb = Sandbox::new();
    let w = workspace(&sb);
    let who = |sb: &Sandbox| sb.git(&w, &["log", "-1", "--format=%an <%ae>|%cn <%ce>"]);
    write(&w, "dbento.toml", "name = \"a\"\n");
    assert!(commit(&sb, &w).unwrap());
    assert_eq!(
        who(&sb).trim(),
        "ada <ada@potluck.invalid>|ada <ada@potluck.invalid>"
    );
    sb.set_identity("Grace", "grace@example.com");
    write(&w, "dbento.toml", "name = \"g\"\n");
    assert!(commit(&sb, &w).unwrap());
    assert_eq!(
        who(&sb).trim(),
        "Grace <grace@example.com>|Grace <grace@example.com>"
    );
}

#[cfg(unix)]
#[test]
fn a_hook_planted_in_the_repo_never_runs() {
    let sb = Sandbox::new();
    let w = workspace(&sb);
    let marker = sb.path().join("hook-ran");
    let body = format!("touch '{}'\nexit 1", marker.display());
    let evil = sb.path().join("evil-hooks");
    sb.script(&w.join(".git/hooks/pre-commit"), &body);
    sb.script(&evil.join("pre-commit"), &body);
    sb.git(&w, &["config", "core.hooksPath", evil.to_str().unwrap()]);
    write(&w, "dbento.toml", "name = \"x\"\n");
    assert!(commit(&sb, &w).unwrap());
    assert!(!marker.exists(), "a planted hook ran");
}

#[test]
fn a_detached_head_or_another_branch_is_refused_before_any_write() {
    let sb = Sandbox::new();
    let w = workspace(&sb);
    write(&w, "dbento.toml", "name = \"x\"\n");
    sb.git(&w, &["checkout", "-q", "--detach"]);
    let err = commit(&sb, &w).unwrap_err();
    assert!(
        matches!(err, SyncError::WrongBranch { found: None, .. }),
        "{err:?}"
    );
    assert_eq!(err.to_string(), "workspace is not on main");
    sb.git(&w, &["switch", "-q", "-c", "other"]);
    let err = commit(&sb, &w).unwrap_err();
    assert!(
        matches!(&err, SyncError::WrongBranch { found: Some(b), .. } if b == "other"),
        "{err:?}"
    );
    assert_eq!(
        sb.git(&w, &["status", "--porcelain"]).trim(),
        "M dbento.toml"
    );
}

#[test]
fn a_second_holder_waits_then_reports_busy() {
    let sb = Sandbox::new();
    let w = workspace(&sb);
    let opts = sb.options().lock_wait(Duration::from_millis(200));
    let first = repo_lock(&w, &opts).expect("first lock");
    let begun = Instant::now();
    let err = repo_lock(&w, &opts).unwrap_err();
    assert!(begun.elapsed() >= Duration::from_millis(200));
    let SyncError::Busy { lock } = &err else {
        panic!("{err:?}")
    };
    assert!(lock.ends_with(".git/dbento-team.lock"), "{lock:?}");
    drop(first);
    let _again = repo_lock(&w, &opts).expect("free after release");
}
