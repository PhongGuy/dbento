use std::fs;
use std::path::{Path, PathBuf};

use dbento_core::{
    PushOutcome, Refusal, SAVE_MESSAGE, SyncError, commit_pending, push, remote_tip,
    squash_unpushed, unpushed,
};

use crate::support::{Sandbox, binding, write};

const REFUSE: &str = "cat >/dev/null\n\
    echo 'potluck-reject secret_value refs/heads/main dbento.toml password' >&2\n\
    echo 'potluck-reject-detail connections.a.password' >&2\nexit 1";

fn workspace(sb: &Sandbox) -> PathBuf {
    sb.origin();
    sb.clone_as("w")
}

fn save(sb: &Sandbox, w: &Path, rel: &str, text: &str) {
    write(w, rel, text);
    assert!(
        commit_pending(&binding(w), SAVE_MESSAGE, &sb.options()).unwrap(),
        "committed"
    );
}

/// Install `hook` as origin's pre-receive, commit an edit unique to it, push; the refusal.
#[cfg(unix)]
fn refused(sb: &Sandbox, w: &Path, hook: &str) -> Refusal {
    sb.hook("pre-receive", hook);
    save(
        sb,
        w,
        "dbento.toml",
        &format!("name = \"{}\"\n", hook.len()),
    );
    match push(&binding(w), &sb.options()).unwrap() {
        PushOutcome::Refused(r) => r,
        other => panic!("not refused: {other:?}"),
    }
}

#[test]
fn several_unpushed_commits_squash_into_one_with_the_same_tree() {
    let sb = Sandbox::new();
    let w = workspace(&sb);
    let (b, opts) = (binding(&w), sb.options());
    for i in 1..=3 {
        save(&sb, &w, &format!("queries/{i}.sql"), "select 1;\n");
    }
    let tree = sb.rev(&w, "HEAD^{tree}");
    assert_eq!(unpushed(&b, &opts).unwrap(), 3);
    assert!(squash_unpushed(&b, &opts).unwrap());
    assert_eq!(unpushed(&b, &opts).unwrap(), 1);
    assert_eq!(sb.rev(&w, "HEAD^{tree}"), tree);
    assert_eq!(sb.rev(&w, "HEAD^"), sb.rev(&w, "origin/main"));
    assert_eq!(
        sb.git(&w, &["log", "-1", "--format=%s"]).trim(),
        "Save changes"
    );
    let head = sb.rev(&w, "HEAD");
    assert!(!squash_unpushed(&b, &opts).unwrap());
    assert_eq!(sb.rev(&w, "HEAD"), head);
}

#[test]
fn unpushed_commits_that_cancel_out_leave_nothing_to_push() {
    let sb = Sandbox::new();
    let w = workspace(&sb);
    save(&sb, &w, "dbento.toml", "name = \"tmp\"\n");
    save(&sb, &w, "dbento.toml", "name = \"seed\"\n");
    assert!(squash_unpushed(&binding(&w), &sb.options()).unwrap());
    assert_eq!(unpushed(&binding(&w), &sb.options()).unwrap(), 0);
    assert_eq!(sb.rev(&w, "HEAD"), sb.rev(&w, "origin/main"));
}

#[test]
fn a_push_lands_and_a_moved_remote_is_non_fast_forward() {
    let sb = Sandbox::new();
    let w = workspace(&sb);
    let mate = sb.clone_as("mate");
    let (b, opts) = (binding(&w), sb.options());
    save(&sb, &w, "dbento.toml", "name = \"pushed\"\n");
    assert_eq!(push(&b, &opts).unwrap(), PushOutcome::Pushed);
    assert_eq!(
        sb.origin_show("dbento.toml").as_deref(),
        Some("name = \"pushed\"\n")
    );
    assert_eq!(unpushed(&b, &opts).unwrap(), 0);
    assert_eq!(remote_tip(&b, &opts).unwrap(), Some(sb.rev(&w, "HEAD")));
    sb.git(&mate, &["pull", "-q", "--ff-only"]);
    sb.teammate(&mate, "queries/b.sql", Some("select 2;\n"));
    save(&sb, &w, "dbento.toml", "name = \"late\"\n");
    assert_eq!(push(&b, &opts).unwrap(), PushOutcome::NonFastForward);
    assert_eq!(
        sb.origin_show("dbento.toml").as_deref(),
        Some("name = \"pushed\"\n")
    );
    assert_eq!(remote_tip(&b, &opts).unwrap(), Some(sb.rev(&mate, "HEAD")));
}

#[cfg(unix)]
#[test]
fn a_hook_refusal_comes_back_as_findings() {
    let sb = Sandbox::new();
    let w = workspace(&sb);
    let r = refused(&sb, &w, REFUSE);
    assert_eq!(r.findings.len(), 1);
    assert_eq!(r.findings[0].rule, "password");
    assert_eq!(
        r.findings[0].detail.as_deref(),
        Some("connections.a.password")
    );
    assert_eq!(
        sb.origin_show("dbento.toml").as_deref(),
        Some("name = \"seed\"\n")
    );
}

#[cfg(unix)]
#[test]
fn server_errors_are_refusals_without_findings() {
    let sb = Sandbox::new();
    let w = workspace(&sb);
    let r = refused(
        &sb,
        &w,
        "cat >/dev/null\necho 'potluck-error rules' >&2\nexit 1",
    );
    assert!(r.is_server_error());
    assert_eq!(r.other, ["potluck-error rules"]);
    let r = refused(&sb, &w, "cat >/dev/null\nexit 1");
    assert!(r.is_server_error());
    assert_eq!(r.other, ["pre-receive hook declined"]);
}

#[cfg(unix)]
#[test]
fn a_full_page_of_findings_survives_the_stderr_cap() {
    let sb = Sandbox::new();
    let w = workspace(&sb);
    let hook = "cat >/dev/null\ni=1\nwhile [ $i -le 20 ]; do\n\
        echo \"potluck-reject secret_value refs/heads/main q$i.sql password\" >&2\n\
        echo \"potluck-reject-detail d$i\" >&2\ni=$((i+1))\ndone\n\
        echo 'potluck-reject-more 5' >&2\nexit 1";
    let r = refused(&sb, &w, hook);
    assert_eq!((r.findings.len(), r.more), (20, 5));
    assert_eq!(r.findings[0].path, "q1.sql");
    assert_eq!(r.findings[19].detail.as_deref(), Some("d20"));
}

#[cfg(unix)]
#[test]
fn squashing_after_a_fix_drops_the_refused_blob() {
    let sb = Sandbox::new();
    let w = workspace(&sb);
    let (b, opts) = (binding(&w), sb.options());
    sb.hook("pre-receive", REFUSE);
    save(&sb, &w, "dbento.toml", "password = \"hunter2\"\n");
    let secret = sb.rev(&w, "HEAD:dbento.toml");
    assert!(matches!(push(&b, &opts).unwrap(), PushOutcome::Refused(_)));
    fs::remove_file(sb.path().join("origin.git/hooks/pre-receive")).unwrap();
    save(&sb, &w, "dbento.toml", "name = \"fixed\"\n");
    assert!(squash_unpushed(&b, &opts).unwrap());
    assert_eq!(push(&b, &opts).unwrap(), PushOutcome::Pushed);
    assert!(
        !sb.origin_objects().contains(&secret),
        "the refused blob reached origin"
    );
}

#[test]
fn push_refuses_a_workspace_off_its_branch() {
    let sb = Sandbox::new();
    let w = workspace(&sb);
    sb.git(&w, &["checkout", "-q", "--detach"]);
    let err = push(&binding(&w), &sb.options()).unwrap_err();
    assert!(matches!(err, SyncError::WrongBranch { .. }), "{err:?}");
}

#[test]
fn a_file_staged_by_hand_stays_out_of_the_squash() {
    let sb = Sandbox::new();
    let w = workspace(&sb);
    let b = binding(&w);
    write(&w, "notes.txt", "local only\n");
    sb.git(&w, &["add", "--", "notes.txt"]);
    save(&sb, &w, "queries/1.sql", "select 1;\n");
    save(&sb, &w, "queries/2.sql", "select 2;\n");
    let tree = sb.rev(&w, "HEAD^{tree}");
    assert!(squash_unpushed(&b, &sb.options()).unwrap());
    assert_eq!(sb.rev(&w, "HEAD^{tree}"), tree);
    assert_eq!(
        sb.git(&w, &["ls-tree", "--name-only", "HEAD", "--", "notes.txt"]),
        ""
    );
    assert_eq!(
        sb.git(&w, &["diff", "--cached", "--name-only"]).trim(),
        "notes.txt"
    );
}
