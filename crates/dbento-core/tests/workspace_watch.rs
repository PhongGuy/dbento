//! The workspace watcher on real folders: what counts as a change, what is ignored, and
//! that `queries/` is followed when it is replaced.
#![cfg(feature = "watch")]
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use dbento_core::{WatchEvent, WorkspaceWatcher};

const WAIT: Duration = Duration::from_secs(10);
// Four debounce windows: whatever the watcher was going to report has arrived.
const QUIET: Duration = Duration::from_millis(1000);

fn workspace() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ws");
    fs::create_dir_all(root.join("queries/lite")).unwrap();
    fs::write(root.join("dbento.toml"), "dbento = 1\n").unwrap();
    fs::write(root.join("queries/lite/a.sql"), "SELECT 1").unwrap();
    (tmp, root)
}

fn start(root: &Path) -> (WorkspaceWatcher, Receiver<WatchEvent>) {
    let (tx, rx) = channel();
    let w = WorkspaceWatcher::start(root, move |e| {
        let _ = tx.send(e);
    })
    .expect("watcher starts");
    (w, rx)
}

fn changed(rx: &Receiver<WatchEvent>, what: &str) {
    match rx.recv_timeout(WAIT) {
        Ok(WatchEvent::Changed) => {}
        other => panic!("{what}: expected Changed, got {other:?}"),
    }
}

fn drain(rx: &Receiver<WatchEvent>) {
    while rx.recv_timeout(QUIET).is_ok() {}
}

fn quiet(rx: &Receiver<WatchEvent>, what: &str) {
    match rx.recv_timeout(QUIET) {
        Err(RecvTimeoutError::Timeout) => {}
        other => panic!("{what}: expected silence, got {other:?}"),
    }
}

#[test]
fn dbento_toml_writes_rename_overs_and_deletes_are_changes() {
    let (_tmp, root) = workspace();
    let (_w, rx) = start(&root);
    fs::write(root.join("dbento.toml"), "dbento = 1\n# edited\n").unwrap();
    changed(&rx, "write in place");
    drain(&rx);
    fs::write(root.join(".dbento.toml.swp"), "dbento = 1\n# saved\n").unwrap();
    fs::rename(root.join(".dbento.toml.swp"), root.join("dbento.toml")).unwrap();
    changed(&rx, "rename-over");
    drain(&rx);
    fs::remove_file(root.join("dbento.toml")).unwrap();
    changed(&rx, "delete");
}

#[test]
fn a_file_in_a_new_queries_subfolder_is_a_change_and_stays_watched() {
    let (_tmp, root) = workspace();
    let (_w, rx) = start(&root);
    fs::create_dir_all(root.join("queries/new/sub")).unwrap();
    fs::write(root.join("queries/new/sub/b.sql"), "SELECT 2").unwrap();
    changed(&rx, "new subfolder");
    drain(&rx);
    fs::write(root.join("queries/new/sub/b.sql"), "SELECT 3").unwrap();
    changed(&rx, "second write in the new subfolder");
}

#[test]
fn a_replaced_queries_folder_is_followed() {
    let (_tmp, root) = workspace();
    let (_w, rx) = start(&root);
    fs::rename(root.join("queries"), root.join("old-queries")).unwrap();
    fs::create_dir_all(root.join("queries/lite2")).unwrap();
    fs::write(root.join("queries/lite2/b.sql"), "SELECT 2").unwrap();
    changed(&rx, "replacement");
    drain(&rx);
    fs::write(root.join("queries/lite2/b.sql"), "SELECT 3").unwrap();
    changed(&rx, "write in the replacement");
}

#[test]
fn a_removed_then_recreated_queries_folder_is_followed() {
    let (_tmp, root) = workspace();
    let (_w, rx) = start(&root);
    fs::remove_dir_all(root.join("queries")).unwrap();
    changed(&rx, "removal");
    drain(&rx);
    fs::create_dir_all(root.join("queries/lite")).unwrap();
    fs::write(root.join("queries/lite/c.sql"), "SELECT 4").unwrap();
    changed(&rx, "recreation");
    drain(&rx);
    fs::write(root.join("queries/lite/c.sql"), "SELECT 5").unwrap();
    changed(&rx, "write in the recreated folder");
}

#[test]
fn siblings_are_ignored() {
    let (_tmp, root) = workspace();
    let (_w, rx) = start(&root);
    fs::write(root.join(".dbento.toml.edit.tmp"), "x").unwrap();
    fs::create_dir(root.join(".git")).unwrap();
    fs::write(root.join(".git/HEAD"), "ref").unwrap();
    fs::write(root.join("notes.txt"), "x").unwrap();
    fs::create_dir(root.join("data")).unwrap();
    fs::write(root.join("data/app.db"), b"").unwrap();
    quiet(&rx, "siblings");
}

#[test]
fn dropping_the_watcher_ends_its_thread() {
    let (_tmp, root) = workspace();
    let (w, rx) = start(&root);
    fs::write(root.join("dbento.toml"), "dbento = 1\n# a\n").unwrap();
    changed(&rx, "before the drop");
    drain(&rx);
    drop(w);
    fs::write(root.join("dbento.toml"), "dbento = 1\n# b\n").unwrap();
    // The thread owned the callback and its sender, so the channel closes instead of delivering.
    let res = rx.recv_timeout(WAIT);
    assert!(
        matches!(res, Err(RecvTimeoutError::Disconnected)),
        "{res:?}"
    );
}

#[test]
fn the_workspace_folder_removed_reports_failed() {
    let (_tmp, root) = workspace();
    let (_w, rx) = start(&root);
    fs::remove_dir_all(&root).unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(WatchEvent::Failed { .. }) => return,
            Ok(_) => {}
            Err(e) => panic!("no Failed within {WAIT:?}: {e:?}"),
        }
    }
}

#[test]
fn a_missing_folder_cannot_be_watched() {
    let tmp = tempfile::tempdir().unwrap();
    let err = WorkspaceWatcher::start(&tmp.path().join("nope"), |_| {}).unwrap_err();
    assert!(err.to_string().contains("nope"), "{err}");
}
