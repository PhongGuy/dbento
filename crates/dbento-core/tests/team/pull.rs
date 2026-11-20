use std::fs;
use std::path::{Path, PathBuf};

use dbento_core::{
    ConflictFile, ConflictKind, ConflictStore, Pulled, StatePaths, SyncError, pull_rebase,
};

use crate::support::{Sandbox, binding, read, write};

fn setup(sb: &Sandbox) -> (PathBuf, PathBuf, ConflictStore) {
    sb.origin();
    let (mine, mate) = (sb.clone_as("mine"), sb.clone_as("mate"));
    let paths = StatePaths::in_dir(&sb.path().join("state"));
    let store = ConflictStore::for_workspace(&paths, &mine).expect("store");
    (mine, mate, store)
}

fn pull(sb: &Sandbox, mine: &Path, store: &ConflictStore) -> Pulled {
    pull_rebase(&binding(mine), store, &sb.options()).expect("pull")
}

fn conflicted(pulled: Pulled) -> Vec<ConflictFile> {
    let Pulled::Conflicted(files) = pulled else {
        panic!("no conflict: {pulled:?}")
    };
    files
}

#[test]
fn state_paths_in_dir_keep_team_files_in_the_test_folder() {
    let dir = Path::new("/tmp/dbento-state");
    let p = StatePaths::in_dir(dir);
    assert_eq!(p.team, dir.join("team.json"));
    assert_eq!(p.team_conflicts, dir.join("team-conflicts"));
}

#[test]
fn the_store_sits_under_a_sixteen_hex_hash_of_the_canonical_dir() {
    let sb = Sandbox::new();
    let ws = sb.path().join("ws");
    fs::create_dir(&ws).unwrap();
    fs::create_dir(sb.path().join("other")).unwrap();
    let paths = StatePaths::in_dir(&sb.path().join("state"));
    let store = ConflictStore::for_workspace(&paths, &ws).unwrap();
    let copy = store.copy_path("queries/a.sql");
    let rel = copy
        .strip_prefix(&paths.team_conflicts)
        .expect("under team_conflicts");
    let hash = rel.iter().next().unwrap().to_str().unwrap().to_owned();
    assert_eq!(hash.len(), 16);
    assert!(
        hash.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
        "{hash}"
    );
    assert_eq!(rel, Path::new(&hash).join("queries/a.sql"));
    let marks = paths.team_conflicts.join(format!("{hash}.deleted"));
    assert_eq!(
        store.mark_path("queries/a.sql"),
        marks.join("queries/a.sql")
    );
    let again = ConflictStore::for_workspace(&paths, &ws.join("..").join("ws")).unwrap();
    assert_eq!(again, store);
    let other = ConflictStore::for_workspace(&paths, &sb.path().join("other")).unwrap();
    assert_ne!(other, store);
    assert!(store.copies().unwrap().is_empty() && store.deleted().unwrap().is_empty());
}

#[test]
fn up_to_date_then_fast_forward_then_rebase() {
    let sb = Sandbox::new();
    let (mine, mate, store) = setup(&sb);
    assert_eq!(pull(&sb, &mine, &store), Pulled::UpToDate);
    sb.teammate(&mate, "queries/b.sql", Some("select 2;\n"));
    assert_eq!(pull(&sb, &mine, &store), Pulled::FastForwarded);
    assert_eq!(read(&mine, "queries/b.sql").as_deref(), Some("select 2;\n"));
    sb.teammate(&mate, "queries/c.sql", Some("select 3;\n"));
    write(&mine, "dbento.toml", "name = \"mine\"\n");
    assert_eq!(pull(&sb, &mine, &store), Pulled::Rebased);
    assert_eq!(read(&mine, "queries/c.sql").as_deref(), Some("select 3;\n"));
    assert_eq!(
        read(&mine, "dbento.toml").as_deref(),
        Some("name = \"mine\"\n")
    );
    assert_eq!(sb.rev(&mine, "HEAD^"), sb.rev(&mine, "origin/main"));
    assert_eq!(sb.git(&mine, &["status", "--porcelain"]), "");
}

#[test]
fn both_changed_saves_mine_takes_theirs_and_keeps_the_rest() {
    let sb = Sandbox::new();
    let (mine, mate, store) = setup(&sb);
    sb.teammate(&mate, "dbento.toml", Some("name = \"theirs\"\n"));
    write(&mine, "dbento.toml", "name = \"mine\"\n");
    write(&mine, "queries/mine.sql", "select 3;\n");
    let files = conflicted(pull(&sb, &mine, &store));
    assert_eq!(files.len(), 1);
    assert_eq!(
        (files[0].path.as_str(), files[0].kind),
        ("dbento.toml", ConflictKind::BothChanged)
    );
    assert_eq!(
        read(&mine, "dbento.toml").as_deref(),
        Some("name = \"theirs\"\n")
    );
    assert_eq!(
        fs::read_to_string(store.copy_path("dbento.toml")).unwrap(),
        "name = \"mine\"\n"
    );
    assert_eq!(store.copies().unwrap(), ["dbento.toml"]);
    assert_eq!(
        read(&mine, "queries/mine.sql").as_deref(),
        Some("select 3;\n")
    );
    assert_eq!(
        sb.git(&mine, &["log", "-1", "--format=%s"]).trim(),
        "Keep local changes"
    );
    assert_eq!(sb.rev(&mine, "HEAD^"), sb.rev(&mine, "origin/main"));
    assert_eq!(sb.git(&mine, &["status", "--porcelain"]), "");
    assert!(!mine.join(".git/rebase-merge").exists() && !mine.join(".git/rebase-apply").exists());
}

#[test]
fn a_local_delete_of_a_remote_edit_is_marked_and_the_remote_file_stays() {
    let sb = Sandbox::new();
    let (mine, mate, store) = setup(&sb);
    sb.teammate(&mate, "queries/a.sql", Some("select 42;\n"));
    fs::remove_file(mine.join("queries/a.sql")).unwrap();
    let files = conflicted(pull(&sb, &mine, &store));
    assert_eq!(
        (files[0].path.as_str(), files[0].kind),
        ("queries/a.sql", ConflictKind::DeletedLocally)
    );
    assert_eq!(
        read(&mine, "queries/a.sql").as_deref(),
        Some("select 42;\n")
    );
    assert!(
        store.mark_path("queries/a.sql").exists() && !store.copy_path("queries/a.sql").exists()
    );
    assert_eq!(store.deleted().unwrap(), ["queries/a.sql"]);
    assert_eq!(sb.rev(&mine, "HEAD"), sb.rev(&mine, "origin/main"));
}

#[test]
fn a_remote_delete_of_a_local_edit_saves_mine() {
    let sb = Sandbox::new();
    let (mine, mate, store) = setup(&sb);
    sb.teammate(&mate, "queries/a.sql", None);
    write(&mine, "queries/a.sql", "select 7;\n");
    let files = conflicted(pull(&sb, &mine, &store));
    assert_eq!(files[0].kind, ConflictKind::DeletedRemotely);
    assert!(!mine.join("queries/a.sql").exists());
    assert_eq!(
        fs::read_to_string(store.copy_path("queries/a.sql")).unwrap(),
        "select 7;\n"
    );
}

#[test]
fn a_dirty_file_outside_the_workspace_paths_fails_without_a_conflict() {
    let sb = Sandbox::new();
    let (mine, mate, store) = setup(&sb);
    sb.teammate(&mate, "README.md", Some("one\n"));
    pull(&sb, &mine, &store);
    sb.teammate(&mate, "dbento.toml", Some("name = \"theirs\"\n"));
    write(&mine, "queries/b.sql", "select 5;\n");
    write(&mine, "README.md", "edited by hand\n");
    let err = pull_rebase(&binding(&mine), &store, &sb.options()).unwrap_err();
    assert!(matches!(err, SyncError::Git(_)), "{err:?}");
    assert!(store.copies().unwrap().is_empty() && !mine.join(".git/rebase-merge").exists());
}
