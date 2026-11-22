use std::fs;
use std::path::Path;
use std::time::Duration;

use dbento_core::{
    ConflictKind, ConflictStore, GitOptions, ReadBlock, Role, StatePaths, SyncError, SyncOutcome,
    repo_lock, sync_once, unpushed,
};

use crate::support::{Sandbox, binding, read, write};

const TOKEN: &str = "plk_testtoken123";

fn state(sb: &Sandbox) -> StatePaths {
    StatePaths::in_dir(&sb.path().join("state"))
}

fn sync(sb: &Sandbox, w: &Path, opts: &GitOptions) -> Result<SyncOutcome, SyncError> {
    sync_once(&binding(w), TOKEN, &state(sb), opts)
}

#[test]
fn edits_land_and_a_teammate_receives_them() {
    let sb = Sandbox::new();
    sb.origin();
    let (a, b) = (sb.clone_as("a"), sb.clone_as("b"));
    write(&a, "dbento.toml", "name = \"edited\"\n");
    assert_eq!(sync(&sb, &a, &sb.options()).unwrap(), SyncOutcome::Synced);
    assert_eq!(
        sb.origin_show("dbento.toml").as_deref(),
        Some("name = \"edited\"\n")
    );
    assert_eq!(unpushed(&binding(&a), &sb.options()).unwrap(), 0);
    assert_eq!(sync(&sb, &b, &sb.options()).unwrap(), SyncOutcome::Synced);
    assert_eq!(
        read(&b, "dbento.toml").as_deref(),
        Some("name = \"edited\"\n")
    );
}

#[test]
fn a_conflict_keeps_mine_outside_and_pushes_the_rest() {
    let sb = Sandbox::new();
    sb.origin();
    let (a, b) = (sb.clone_as("a"), sb.clone_as("b"));
    write(&a, "dbento.toml", "name = \"theirs\"\n");
    assert_eq!(sync(&sb, &a, &sb.options()).unwrap(), SyncOutcome::Synced);
    write(&b, "dbento.toml", "name = \"mine\"\n");
    write(&b, "queries/b.sql", "select 3;\n");
    let got = sync(&sb, &b, &sb.options()).unwrap();
    let SyncOutcome::Conflicted(files) = got else {
        panic!("no conflict: {got:?}")
    };
    assert_eq!((files.len(), files[0].kind), (1, ConflictKind::BothChanged));
    assert_eq!(
        sb.origin_show("dbento.toml").as_deref(),
        Some("name = \"theirs\"\n")
    );
    assert_eq!(
        sb.origin_show("queries/b.sql").as_deref(),
        Some("select 3;\n")
    );
    let store = ConflictStore::for_workspace(&state(&sb), &b).unwrap();
    assert_eq!(
        fs::read_to_string(store.copy_path("dbento.toml")).unwrap(),
        "name = \"mine\"\n"
    );
}

#[cfg(unix)]
#[test]
fn a_refusal_holds_and_the_fix_squashes_the_secret_away() {
    let sb = Sandbox::new();
    sb.origin();
    let w = sb.clone_as("w");
    sb.hook(
        "pre-receive",
        "cat >/dev/null\necho 'potluck-reject secret_value refs/heads/main dbento.toml password' >&2\nexit 1",
    );
    write(&w, "dbento.toml", "password = \"hunter2\"\n");
    let got = sync(&sb, &w, &sb.options()).unwrap();
    let SyncOutcome::Refused(r) = got else {
        panic!("not refused: {got:?}")
    };
    assert_eq!(r.findings[0].rule, "password");
    let secret = sb.rev(&w, "HEAD:dbento.toml");
    fs::remove_file(sb.path().join("origin.git/hooks/pre-receive")).unwrap();
    write(&w, "dbento.toml", "name = \"fixed\"\n");
    assert_eq!(sync(&sb, &w, &sb.options()).unwrap(), SyncOutcome::Synced);
    assert_eq!(
        sb.origin_show("dbento.toml").as_deref(),
        Some("name = \"fixed\"\n")
    );
    assert!(
        !sb.origin_objects().contains(&secret),
        "the refused blob reached origin"
    );
}

#[test]
fn a_read_member_pulls_and_never_pushes() {
    let sb = Sandbox::new();
    sb.origin();
    let (mate, w) = (sb.clone_as("mate"), sb.clone_as("w"));
    sb.teammate(&mate, "dbento.toml", Some("name = \"theirs\"\n"));
    write(&w, "queries/a.sql", "select 9;\n");
    let reader = binding(&w).with_role(Role::Read);
    let got = sync_once(&reader, TOKEN, &state(&sb), &sb.options()).unwrap();
    assert_eq!(got, SyncOutcome::ReadOnly);
    assert_eq!(
        read(&w, "dbento.toml").as_deref(),
        Some("name = \"theirs\"\n")
    );
    assert_eq!(
        sb.origin_show("queries/a.sql").as_deref(),
        Some("select 1;\n")
    );
    assert_eq!(sb.git(&w, &["status", "--porcelain"]), " M queries/a.sql\n");
}

#[test]
fn a_reader_whose_edit_collides_is_held_instead_of_stuck() {
    let sb = Sandbox::new();
    sb.origin();
    let (mate, w) = (sb.clone_as("mate"), sb.clone_as("w"));
    sb.teammate(&mate, "dbento.toml", Some("name = \"theirs\"\n"));
    write(&w, "dbento.toml", "name = \"mine\"\n");
    let reader = binding(&w).with_role(Role::Read);
    for _ in 0..2 {
        assert_eq!(
            sync_once(&reader, TOKEN, &state(&sb), &sb.options()).unwrap(),
            SyncOutcome::ReadOnlyBlocked(ReadBlock::Edits(vec!["dbento.toml".into()]))
        );
    }
    assert_eq!(
        read(&w, "dbento.toml").as_deref(),
        Some("name = \"mine\"\n")
    );
    assert_eq!(
        sb.git(&w, &["status", "--porcelain"]),
        " M dbento.toml\n",
        "nothing committed"
    );
    assert_eq!(unpushed(&reader, &sb.options()).unwrap(), 0);
}

#[test]
fn a_reader_whose_file_stands_where_the_server_adds_a_folder_is_held() {
    let sb = Sandbox::new();
    sb.origin();
    let (mate, w) = (sb.clone_as("mate"), sb.clone_as("w"));
    sb.teammate(&mate, "x/y", Some("theirs\n"));
    write(&w, "x", "mine\n");
    let reader = binding(&w).with_role(Role::Read);
    assert_eq!(
        sync_once(&reader, TOKEN, &state(&sb), &sb.options()).unwrap(),
        SyncOutcome::ReadOnlyBlocked(ReadBlock::Edits(vec!["x".into()]))
    );
    assert_eq!(
        read(&w, "x").as_deref(),
        Some("mine\n"),
        "the local file stays"
    );
}

#[test]
fn a_reader_with_an_unpushed_commit_is_held_and_says_so() {
    let sb = Sandbox::new();
    sb.origin();
    let (mate, w) = (sb.clone_as("mate"), sb.clone_as("w"));
    write(&w, "queries/mine.sql", "select 2;\n");
    sb.git(&w, &["add", "-A"]);
    sb.git(&w, &["commit", "-q", "-m", "before the demotion"]);
    sb.teammate(&mate, "dbento.toml", Some("name = \"theirs\"\n"));
    let reader = binding(&w).with_role(Role::Read);
    assert_eq!(
        sync_once(&reader, TOKEN, &state(&sb), &sb.options()).unwrap(),
        SyncOutcome::ReadOnlyBlocked(ReadBlock::Commits(1))
    );
    assert_eq!(
        read(&w, "dbento.toml").as_deref(),
        Some("name = \"seed\"\n")
    );
    assert_eq!(sb.origin_show("queries/mine.sql"), None);
}

#[test]
fn a_branch_deleted_on_the_server_stops_the_sync_and_is_never_recreated() {
    let sb = Sandbox::new();
    sb.origin();
    let w = sb.clone_as("w");
    sb.git(
        sb.path(),
        &[
            "--git-dir",
            "origin.git",
            "update-ref",
            "-d",
            "refs/heads/main",
        ],
    );
    write(&w, "queries/b.sql", "select 2;\n");
    for role in [Role::Write, Role::Read] {
        let got = sync_once(
            &binding(&w).with_role(role),
            TOKEN,
            &state(&sb),
            &sb.options(),
        );
        assert!(
            matches!(&got, Err(SyncError::BranchGone { branch }) if branch == "main"),
            "{role:?}: {got:?}"
        );
    }
    assert_eq!(
        sb.origin_show("dbento.toml"),
        None,
        "the branch stays deleted"
    );
}

/// Options whose `git` is a wrapper that, before a push, makes the racer clone push first:
/// on the first push only, or on every push with `always`. The race count lands in `races`.
#[cfg(unix)]
fn racing(sb: &Sandbox, always: bool) -> GitOptions {
    let racer = sb.clone_as("racer");
    let body = "case \" $* \" in\n*\" push \"*)\n\
        n=$(cat \"$RACE_COUNT\" 2>/dev/null || echo 0)\n\
        if [ \"$RACE_ALWAYS\" = 1 ] || [ \"$n\" = 0 ]; then\n\
        echo $((n+1)) > \"$RACE_COUNT\"\n\
        \"$REAL_GIT\" -C \"$RACER\" -c user.name=R -c user.email=r@example.com commit -q --allow-empty -m \"race $n\" &&\n\
        \"$REAL_GIT\" -C \"$RACER\" push -q origin main\nfi;;\nesac\nexec \"$REAL_GIT\" \"$@\"";
    sb.options()
        .env("PATH", sb.fake_git("race-bin", body))
        .env("REAL_GIT", crate::support::real_git())
        .env("RACER", racer)
        .env("RACE_COUNT", sb.path().join("races"))
        .env("RACE_ALWAYS", if always { "1" } else { "0" })
}

#[cfg(unix)]
#[test]
fn a_push_that_loses_a_race_pulls_and_pushes_again() {
    let sb = Sandbox::new();
    sb.origin();
    let w = sb.clone_as("w");
    write(&w, "dbento.toml", "name = \"after race\"\n");
    assert_eq!(
        sync(&sb, &w, &racing(&sb, false)).unwrap(),
        SyncOutcome::Synced
    );
    assert_eq!(
        sb.origin_show("dbento.toml").as_deref(),
        Some("name = \"after race\"\n")
    );
    let log = sb.git(
        sb.path(),
        &["--git-dir", "origin.git", "log", "--format=%s", "main"],
    );
    assert!(log.contains("race 0"), "{log}");
}

#[cfg(unix)]
#[test]
fn a_remote_that_keeps_moving_gives_up_after_three_retries() {
    let sb = Sandbox::new();
    sb.origin();
    let w = sb.clone_as("w");
    write(&w, "dbento.toml", "name = \"never lands\"\n");
    let err = sync(&sb, &w, &racing(&sb, true)).unwrap_err();
    assert!(matches!(err, SyncError::Raced { pushes: 4 }), "{err:?}");
    assert_eq!(
        fs::read_to_string(sb.path().join("races")).unwrap().trim(),
        "4"
    );
}

#[test]
fn a_held_lock_is_busy_and_a_detached_head_is_refused() {
    let sb = Sandbox::new();
    sb.origin();
    let w = sb.clone_as("w");
    let held = repo_lock(&w, &sb.options()).unwrap();
    let err = sync(&sb, &w, &sb.options().lock_wait(Duration::from_millis(200))).unwrap_err();
    assert!(matches!(err, SyncError::Busy { .. }), "{err:?}");
    drop(held);
    sb.git(&w, &["checkout", "-q", "--detach"]);
    write(&w, "dbento.toml", "name = \"detached\"\n");
    let err = sync(&sb, &w, &sb.options()).unwrap_err();
    assert!(matches!(err, SyncError::WrongBranch { .. }), "{err:?}");
    assert_eq!(
        sb.origin_show("dbento.toml").as_deref(),
        Some("name = \"seed\"\n")
    );
}

#[test]
fn a_failure_never_shows_the_token() {
    let sb = Sandbox::new();
    sb.origin();
    let w = sb.clone_as("w");
    fs::rename(sb.path().join("origin.git"), sb.path().join("gone.git")).unwrap();
    write(&w, "dbento.toml", "name = \"offline\"\n");
    let err = sync(&sb, &w, &sb.options()).unwrap_err();
    let text = format!("{err} {err:?}");
    assert!(!text.contains("testtoken123"), "{text}");
}
