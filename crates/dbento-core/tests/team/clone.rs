use std::fs;

use dbento_core::{Binding, CloneError, Role, clone, unpushed};

use crate::support::{PUBLIC_URL, Sandbox, binding, read, write};

const IGNORED: [&str; 11] = [
    "*.db",
    "*.sqlite",
    "*.sqlite3",
    "*.duckdb",
    "*-wal",
    "*-shm",
    "*-journal",
    "*.pem",
    "*.key",
    "*.p12",
    "*.pfx",
];

#[test]
fn a_clone_into_a_missing_folder_writes_and_commits_the_gitignore() {
    let sb = Sandbox::new();
    sb.origin();
    let dir = sb.path().join("joined/ws");
    clone(&sb.url(), &binding(&dir), &sb.options()).expect("clone");
    assert_eq!(
        read(&dir, "dbento.toml").as_deref(),
        Some("name = \"seed\"\n")
    );
    let ignore = read(&dir, ".gitignore").expect(".gitignore");
    assert_eq!(ignore.lines().collect::<Vec<_>>(), IGNORED);
    assert_eq!(
        sb.git(&dir, &["log", "-1", "--format=%s"]).trim(),
        "Add .gitignore"
    );
    assert_eq!(
        sb.git(&dir, &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/main"
    );
    assert_eq!(unpushed(&binding(&dir), &sb.options()).unwrap(), 1);
    write(&dir, "local.db", "SQLite format 3");
    write(&dir, "id.pem", "key");
    assert_eq!(sb.git(&dir, &["status", "--porcelain"]), "");
}

#[test]
fn a_read_member_ignores_locally_and_commits_nothing() {
    let sb = Sandbox::new();
    sb.origin();
    let dir = sb.path().join("joined/ws");
    let read_only = binding(&dir).with_role(Role::Read);
    clone(&sb.url(), &read_only, &sb.options()).expect("clone");
    assert_eq!(read(&dir, ".gitignore"), None, "no .gitignore is written");
    assert_eq!(
        unpushed(&read_only, &sb.options()).unwrap(),
        0,
        "nothing waits to be pushed"
    );
    let exclude = read(&dir, ".git/info/exclude").expect("exclude");
    assert!(
        IGNORED.iter().all(|p| exclude.lines().any(|l| l == *p)),
        "{exclude}"
    );
    write(&dir, "local.db", "SQLite format 3");
    write(&dir, "id.pem", "key");
    assert_eq!(
        sb.git(&dir, &["status", "--porcelain"]),
        "",
        "local databases and keys stay ignored"
    );
}

#[test]
fn an_empty_folder_is_used_and_a_non_empty_one_refused_before_cloning() {
    let sb = Sandbox::new();
    sb.origin();
    let empty = sb.path().join("empty");
    fs::create_dir(&empty).unwrap();
    clone(&sb.url(), &binding(&empty), &sb.options()).expect("clone");
    assert!(empty.join("dbento.toml").exists());
    let full = sb.path().join("full");
    write(&full, "keep.txt", "mine\n");
    let err = clone(&sb.url(), &binding(&full), &sb.options()).unwrap_err();
    assert!(matches!(err, CloneError::NotEmpty { .. }), "{err:?}");
    assert_eq!(read(&full, "keep.txt").as_deref(), Some("mine\n"));
    assert!(!full.join(".git").exists());
}

#[test]
fn a_gitignore_already_on_the_server_is_kept() {
    let sb = Sandbox::new();
    sb.origin();
    let mate = sb.clone_as("mate");
    sb.teammate(&mate, ".gitignore", Some("custom\n"));
    let dir = sb.path().join("ws");
    clone(&sb.url(), &binding(&dir), &sb.options()).expect("clone");
    assert_eq!(read(&dir, ".gitignore").as_deref(), Some("custom\n"));
    assert_eq!(unpushed(&binding(&dir), &sb.options()).unwrap(), 0);
}

#[test]
fn a_failed_clone_leaves_no_folder_behind() {
    let sb = Sandbox::new();
    sb.origin();
    let dir = sb.path().join("ws");
    let b = Binding::new(&dir, PUBLIC_URL, "no-such-branch");
    let err = clone(&sb.url(), &b, &sb.options()).unwrap_err();
    assert!(matches!(err, CloneError::Git(_)), "{err:?}");
    assert!(!dir.exists());
}

#[cfg(unix)]
#[test]
fn symlinks_check_out_as_plain_files() {
    let sb = Sandbox::new();
    sb.origin();
    let mate = sb.clone_as("mate");
    std::os::unix::fs::symlink("/etc/hostname", mate.join("queries/link.sql")).unwrap();
    sb.git(&mate, &["add", "-A"]);
    sb.git(&mate, &["commit", "-qm", "link"]);
    sb.git(&mate, &["push", "-q", "origin", "main"]);
    let dir = sb.path().join("ws");
    clone(&sb.url(), &binding(&dir), &sb.options()).expect("clone");
    let meta = fs::symlink_metadata(dir.join("queries/link.sql")).unwrap();
    assert!(!meta.file_type().is_symlink());
    assert_eq!(
        read(&dir, "queries/link.sql").as_deref(),
        Some("/etc/hostname")
    );
}
