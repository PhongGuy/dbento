//! dbento.toml changed on both sides of a sync: merged per connection, or reported as a conflict.

use std::fs;
use std::path::{Path, PathBuf};

use dbento_core::{
    ConflictFile, ConflictKind, ConflictStore, Pulled, StatePaths, SyncOutcome, Workspace,
    pull_rebase, sync_once,
};

use crate::support::{Sandbox, binding, read, write};

const TOKEN: &str = "plk_testtoken123";
const TOML: &str = "dbento.toml";
const BASE: &str = "dbento = 1\n\n[connections.a]\nengine = \"sqlite\"\npath = \"a.db\"\n";
const PG: &str = "dbento = 1\n\n[connections.p]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\n";

/// A SQLite connection `name` as an appended table, blank line first.
fn entry(name: &str) -> String {
    format!("\n[connections.{name}]\nengine = \"sqlite\"\npath = \"{name}.db\"\n")
}

/// `text` with `extra` inserted right after its `dbento = 1` line.
fn at_top(text: &str, extra: &str) -> String {
    text.replacen("dbento = 1\n", &format!("dbento = 1\n{extra}"), 1)
}

/// Origin seeded with `base` as dbento.toml; returns the clones (mine, mate), both holding it.
fn seeded(sb: &Sandbox, base: &str) -> (PathBuf, PathBuf) {
    sb.origin();
    let mate = sb.clone_as("mate");
    sb.teammate(&mate, TOML, Some(base));
    (sb.clone_as("mine"), mate)
}

fn state(sb: &Sandbox) -> StatePaths {
    StatePaths::in_dir(&sb.path().join("state"))
}

fn sync(sb: &Sandbox, w: &Path) -> SyncOutcome {
    sync_once(&binding(w), TOKEN, &state(sb), &sb.options()).expect("sync")
}

fn store(sb: &Sandbox, w: &Path) -> ConflictStore {
    ConflictStore::for_workspace(&state(sb), w).expect("store")
}

fn valid(text: &str) -> bool {
    Workspace::from_text(PathBuf::from("/ws"), Path::new("/ws/dbento.toml"), text).is_ok()
}

/// The one conflict a sync reported, which must be dbento.toml changed on both sides.
fn toml_conflict(got: SyncOutcome) {
    let SyncOutcome::Conflicted(files) = got else {
        panic!("no conflict: {got:?}")
    };
    assert_eq!(
        files
            .iter()
            .map(|f: &ConflictFile| (f.path.as_str(), f.kind))
            .collect::<Vec<_>>(),
        [(TOML, ConflictKind::BothChanged)],
        "only dbento.toml conflicts"
    );
}

#[test]
fn two_fields_of_one_connection_conflict_where_git_would_rebase_cleanly() {
    let sb = Sandbox::new();
    let (mine, mate) = seeded(&sb, PG);
    let theirs = PG.replace("user = \"u\"", "user = \"theirs\"");
    let ours = PG.replace("host = \"h\"", "host = \"mine\"");
    sb.teammate(&mate, TOML, Some(&theirs));
    write(&mine, TOML, &ours);
    write(&mine, "queries/mine.sql", "select 3;\n");
    toml_conflict(sync(&sb, &mine));
    assert_eq!(sb.origin_show(TOML).as_deref(), Some(theirs.as_str()));
    assert_eq!(read(&mine, TOML).as_deref(), Some(theirs.as_str()));
    assert_eq!(
        sb.origin_show("queries/mine.sql").as_deref(),
        Some("select 3;\n")
    );
    let copy = fs::read_to_string(store(&sb, &mine).copy_path(TOML)).unwrap();
    assert_eq!(copy, ours);
    assert_eq!(sb.git(&mine, &["status", "--porcelain"]), "");
}

#[test]
fn one_name_added_twice_conflicts_and_no_duplicate_table_is_pushed() {
    let sb = Sandbox::new();
    let (mine, mate) = seeded(&sb, BASE);
    let theirs = at_top(BASE, &entry("x"));
    let ours = format!("{BASE}{}", entry("x").replace("x.db", "mine.db"));
    sb.teammate(&mate, TOML, Some(&theirs));
    write(&mine, TOML, &ours);
    toml_conflict(sync(&sb, &mine));
    let pushed = sb.origin_show(TOML).unwrap();
    assert_eq!(pushed, theirs);
    assert!(valid(&pushed));
    assert_eq!(read(&mine, TOML).as_deref(), Some(theirs.as_str()));
}

#[test]
fn a_clean_rebase_whose_merge_matches_git_adds_no_commit() {
    let sb = Sandbox::new();
    let (mine, mate) = seeded(&sb, BASE);
    let theirs = at_top(BASE, &entry("b"));
    sb.teammate(&mate, TOML, Some(&theirs));
    write(&mine, TOML, &format!("{BASE}{}", entry("c")));
    let pulled = pull_rebase(&binding(&mine), &store(&sb, &mine), &sb.options()).unwrap();
    assert_eq!(pulled, Pulled::Rebased);
    let want = format!("{theirs}{}", entry("c"));
    assert_eq!(read(&mine, TOML).as_deref(), Some(want.as_str()));
    assert_eq!(sb.rev(&mine, "HEAD^"), sb.rev(&mine, "origin/main"));
    assert_eq!(sb.git(&mine, &["status", "--porcelain"]), "");
}

#[test]
fn a_clean_rebase_whose_merge_differs_from_git_commits_the_merge() {
    let sb = Sandbox::new();
    let (mine, mate) = seeded(&sb, BASE);
    sb.teammate(&mate, TOML, Some(&format!("{BASE}{}", entry("b"))));
    write(&mine, TOML, &at_top(BASE, &entry("c")));
    let pulled = pull_rebase(&binding(&mine), &store(&sb, &mine), &sb.options()).unwrap();
    assert_eq!(pulled, Pulled::Rebased);
    let want = format!("{BASE}{}{}", entry("b"), entry("c"));
    assert_eq!(read(&mine, TOML).as_deref(), Some(want.as_str()));
    assert_eq!(
        sb.git(&mine, &["log", "-1", "--format=%s"]).trim(),
        "Merge dbento.toml"
    );
    assert_eq!(sb.rev(&mine, "HEAD~2"), sb.rev(&mine, "origin/main"));
    assert_eq!(sb.git(&mine, &["status", "--porcelain"]), "");
}

#[test]
fn a_dbento_toml_that_is_not_utf8_is_left_to_the_conflict_choice() {
    let sb = Sandbox::new();
    let (mine, mate) = seeded(&sb, BASE);
    // A Latin-1 byte in a comment: valid for git, not UTF-8, so the merge cannot read it.
    let mut bytes = b"dbento = 1\n# caf\xe9\n".to_vec();
    bytes.extend_from_slice(&BASE.as_bytes()["dbento = 1\n".len()..]);
    fs::write(mate.join(TOML), &bytes).unwrap();
    sb.git(&mate, &["commit", "-qam", "latin-1 comment"]);
    sb.git(&mate, &["push", "-q", "origin", "main"]);
    let ours = format!("{BASE}{}", entry("c"));
    write(&mine, TOML, &ours);
    toml_conflict(sync(&sb, &mine));
    assert_eq!(fs::read(mine.join(TOML)).unwrap(), bytes);
    let copy = fs::read_to_string(store(&sb, &mine).copy_path(TOML)).unwrap();
    assert_eq!(copy, ours);
}

#[test]
fn a_local_delete_of_dbento_toml_keeps_the_delete_conflict() {
    let sb = Sandbox::new();
    let (mine, mate) = seeded(&sb, BASE);
    let theirs = format!("{BASE}{}", entry("b"));
    sb.teammate(&mate, TOML, Some(&theirs));
    fs::remove_file(mine.join(TOML)).unwrap();
    let got = sync(&sb, &mine);
    let SyncOutcome::Conflicted(files) = &got else {
        panic!("no conflict: {got:?}")
    };
    assert_eq!(
        (files[0].path.as_str(), files[0].kind),
        (TOML, ConflictKind::DeletedLocally)
    );
    assert_eq!(read(&mine, TOML).as_deref(), Some(theirs.as_str()));
}
