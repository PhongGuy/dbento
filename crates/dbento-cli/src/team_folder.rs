//! `dbento team join | status | sync | resolve | unlink`: one core call each, printed and mapped
//! to an exit code, plus `sync --watch`, which keeps a driver running. Results go to stdout,
//! notes to stderr.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dbento_core::paths::StatePaths;
use dbento_core::{
    ApiError, BindingRecord, CloneError, ConflictFile, ConflictKind, ConflictStore, FolderSync,
    FolderSyncError, GitError, GitOptions, JoinError, Refusal, Resolution, ServerUrl, SyncError,
    SyncOutcome, SyncState, SyncStatus, TeamAccount, TeamFile, TeamResolveError, TeamSync,
    TokenStore, WatchEvent, WorkspaceWatcher, secret_env_var, team_join, team_resolve, team_sync,
    unpushed,
};

use crate::Exit;
use crate::interrupt::Interrupt;
use crate::team::{api_failed, failed};

const NOT_BOUND: &str = "not a team workspace; join it with `dbento team join`";

/// Clones workspace `name` of `server` into `dir` and binds it.
pub(crate) fn join(
    tokens: &dyn TokenStore,
    paths: &StatePaths,
    server: &str,
    name: &str,
    dir: &Path,
    user: Option<&str>,
) -> Exit {
    let server = match ServerUrl::parse(server) {
        Ok(server) => server,
        Err(e) => return failed(e),
    };
    match team_join(tokens, paths, &server, user, name, dir, &GitOptions::new()) {
        Ok(b) => {
            eprintln!("joined {}/{} into {}", b.team, b.workspace, dir.display());
            Exit::Ok
        }
        Err(e @ JoinError::SignedOut { .. }) => signed_out(&e),
        Err(JoinError::Api(e)) => api_failed(&e),
        Err(JoinError::Clone(CloneError::Git(GitError::Missing))) => git_missing(),
        Err(e) => failed(e),
    }
}

/// Prints the binding of `dir` without calling the network: "signed in" means a stored token.
pub(crate) fn status(tokens: &dyn TokenStore, paths: &StatePaths, dir: &Path) -> Exit {
    let data = match TeamFile::new(&paths.team).load() {
        Ok(data) => data,
        Err(e) => return failed(e),
    };
    let Some((folder, b)) = data.binding_for(dir) else {
        return failed(NOT_BOUND);
    };
    let has_token = ServerUrl::parse(&b.server)
        .ok()
        .map(|server| TeamAccount::new(server, b.username.as_str()))
        .and_then(|account| tokens.get(&account).ok().flatten())
        .is_some();
    let ahead = match unpushed(&b.to_binding(&folder), &GitOptions::new()) {
        Ok(n) => n.to_string(),
        Err(e) => format!("unknown ({e})"),
    };
    let last = match b.last_synced {
        None => "never".to_owned(),
        Some(at) => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default();
            format!("{}s ago", now.saturating_sub(at))
        }
    };
    let state = if has_token { "signed in" } else { "signed out" };
    let rows = [
        ("workspace", format!("{}/{}", b.team, b.workspace)),
        ("server", b.server.clone()),
        ("branch", b.branch.clone()),
        ("role", b.role.clone()),
        ("account", format!("{} ({state})", b.username)),
        ("unpushed", ahead),
        ("last sync", last),
    ];
    for (key, value) in rows {
        println!("{:<11}{}", format!("{key}:"), value);
    }
    Exit::Ok
}

/// One sync of the folder bound at or around `dir`.
pub(crate) fn sync(tokens: &dyn TokenStore, paths: &StatePaths, dir: &Path) -> Exit {
    match team_sync(tokens, paths, dir, &GitOptions::new()) {
        Ok(done) => print_sync(paths, &done),
        Err(e) => sync_failed(&e),
    }
}

/// Settles conflicted `rel`; keeping mine syncs right after.
pub(crate) fn resolve(
    tokens: &dyn TokenStore,
    paths: &StatePaths,
    dir: &Path,
    rel: &str,
    keep_mine: bool,
) -> Exit {
    let choice = if keep_mine {
        Resolution::KeepMine
    } else {
        Resolution::TakeTheirs
    };
    match team_resolve(tokens, paths, dir, rel, choice, &GitOptions::new()) {
        Ok(Some(done)) => print_sync(paths, &done),
        Ok(None) => {
            eprintln!("kept the server's {rel}");
            Exit::Ok
        }
        Err(TeamResolveError::Sync(e)) => sync_failed(&e),
        Err(e) => failed(e),
    }
}

/// Forgets the binding of the folder at or around `dir`; the files and history stay.
pub(crate) fn unlink(paths: &StatePaths, dir: &Path) -> Exit {
    let file = TeamFile::new(&paths.team);
    let removed = file.update(|d| {
        let (folder, _) = d.binding_for(dir)?;
        d.remove_binding(&folder).then_some(folder)
    });
    match removed {
        Ok(Some(folder)) => {
            eprintln!(
                "unlinked {}; the folder and its git history stay",
                folder.display()
            );
            Exit::Ok
        }
        Ok(None) => failed(NOT_BOUND),
        Err(e) => failed(e),
    }
}

fn print_sync(paths: &StatePaths, done: &FolderSync) -> Exit {
    let name = full_name(&done.binding);
    let exit = match &done.outcome {
        SyncOutcome::Synced => {
            println!("synced {name}");
            Exit::Ok
        }
        SyncOutcome::ReadOnly => {
            println!("read-only: {name} is up to date with the server; nothing pushed");
            Exit::Ok
        }
        SyncOutcome::ReadOnlyBlocked(block) => {
            println!("read-only: {name} not updated; {block}");
            Exit::Ok
        }
        SyncOutcome::Refused(refusal) => {
            print_refused(refusal);
            Exit::Refused
        }
        SyncOutcome::Conflicted(files) => {
            print_conflicts(paths, &done.dir, files);
            Exit::Conflicted
        }
        _ => {
            println!("{name}: {:?}", done.outcome);
            Exit::Failed
        }
    };
    for r in &done.renames {
        println!(
            "renamed connection '{}' to '{}': move its secret with `dbento secret set {}` (or rename ${} to ${})",
            r.from,
            r.to,
            r.to,
            secret_env_var(&r.from),
            secret_env_var(&r.to)
        );
    }
    exit
}

fn print_refused(refusal: &Refusal) {
    for row in refusal.rows() {
        println!("refused: {row}");
    }
}

/// One line per conflicted file of the folder at `dir`, with where the local side was saved.
fn print_conflicts(paths: &StatePaths, dir: &Path, files: &[ConflictFile]) {
    let store = ConflictStore::for_workspace(paths, dir).ok();
    for f in files {
        let copy = store.as_ref().map(|s| s.copy_path(&f.path));
        match (f.kind, copy) {
            // A local delete keeps no copy, only a mark, so there is nothing to compare.
            (ConflictKind::DeletedLocally, _) | (_, None) => {
                println!("conflict: {} ({})", f.path, kind_text(f.kind));
            }
            (kind, Some(copy)) => {
                println!(
                    "conflict: {} ({}); yours is saved at {}",
                    f.path,
                    kind_text(kind),
                    copy.display()
                );
                println!(
                    "  compare: git diff --no-index {} {}",
                    copy.display(),
                    dir.join(&f.path).display()
                );
            }
        }
    }
    println!("settle each with: dbento team resolve <path> --keep-mine | --take-theirs");
}

/// Keeps the folder bound at or around `dir` in sync until Ctrl-C, a sign-out or missing git,
/// printing each new end state.
pub(crate) fn watch(tokens: Arc<dyn TokenStore>, paths: &StatePaths, dir: &Path) -> Exit {
    let data = match TeamFile::new(&paths.team).load() {
        Ok(data) => data,
        Err(e) => return failed(e),
    };
    let Some((folder, rec)) = data.binding_for(dir) else {
        return failed(NOT_BOUND);
    };
    let interrupt = match Interrupt::install() {
        Ok(interrupt) => interrupt,
        Err(e) => return failed(e),
    };
    // The status callback runs on the driver thread; it hands a final exit code back here.
    let code = Arc::new(AtomicI32::new(-1));
    let printer = Printer {
        paths: paths.clone(),
        dir: folder.clone(),
        name: full_name(&rec),
        server: rec.server.clone(),
        code: Arc::clone(&code),
        last: Mutex::new(None),
    };
    let sync = match TeamSync::start(rec.to_binding(&folder), tokens, paths, move |status| {
        printer.show(status);
    }) {
        Ok(sync) => Arc::new(sync),
        Err(e) => return failed(e),
    };
    // The watcher holds only a weak handle, so the sync is dropped, and flushed, on this thread.
    let target = Arc::downgrade(&sync);
    let watcher = WorkspaceWatcher::start(&folder, move |event| match event {
        WatchEvent::Changed => {
            if let Some(sync) = target.upgrade() {
                sync.changed();
            }
        }
        WatchEvent::Failed { reason } => watch_lost(&reason),
        _ => {}
    });
    let watcher = watcher.map_err(|e| watch_lost(&e.to_string())).ok();
    while !interrupt.stopped() && code.load(Ordering::SeqCst) < 0 {
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(watcher);
    // A save reported just as the watcher stopped may hold the sync a moment longer.
    let mut sync = sync;
    loop {
        match Arc::try_unwrap(sync) {
            Ok(last) => break drop(last),
            Err(held) => {
                sync = held;
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    match code.load(Ordering::SeqCst) {
        c if c == Exit::SignedOut as i32 => Exit::SignedOut,
        c if c == Exit::Failed as i32 => Exit::Failed,
        _ => Exit::Ok,
    }
}

fn watch_lost(reason: &str) {
    eprintln!(
        "warning: not watching the folder ({reason}); local saves sync when the server changes or on the next start"
    );
}

/// An end state as `--watch` compares it: the state, its read-only note and, for a sync, when.
type EndState = (SyncState, Option<String>, Option<SystemTime>);

/// Turns `--watch` statuses into lines: one per new end state, none for progress.
struct Printer {
    paths: StatePaths,
    dir: PathBuf,
    name: String,
    server: String,
    code: Arc<AtomicI32>,
    last: Mutex<Option<EndState>>,
}

impl Printer {
    fn show(&self, status: SyncStatus) {
        let SyncStatus {
            state,
            note,
            last_synced,
            ..
        } = status;
        if matches!(
            state,
            SyncState::Committing | SyncState::Pulling | SyncState::Pushing
        ) {
            return;
        }
        // Each completed sync stamps a new time, so every push gets a line; retries stay deduped.
        let stamp = (state == SyncState::Synced)
            .then_some(last_synced)
            .flatten();
        let end = (state, note, stamp);
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if last.as_ref() == Some(&end) {
            return;
        }
        let name = &self.name;
        match &end {
            (SyncState::Synced, _, _) => println!("synced {name}"),
            (SyncState::ReadOnly, None, _) => println!("read-only: following {name}"),
            (SyncState::ReadOnly, Some(note), _) => {
                println!("read-only: {name} not updated; {note}");
            }
            (SyncState::Refused(refusal), _, _) => print_refused(refusal),
            (SyncState::Conflicted(files), _, _) => print_conflicts(&self.paths, &self.dir, files),
            (SyncState::Offline, _, _) => println!("offline; retrying"),
            (SyncState::Busy, _, _) => {
                println!("busy: another Dbento is syncing this workspace; retrying");
            }
            (SyncState::Failed(text), _, _) => println!("error: {text}"),
            (SyncState::SignedOut, _, _) => {
                eprintln!(
                    "error: signed out of {}; run dbento team login",
                    self.server
                );
                self.code.store(Exit::SignedOut as i32, Ordering::SeqCst);
            }
            (SyncState::GitMissing, _, _) => {
                eprintln!("error: git is not installed");
                self.code.store(Exit::Failed as i32, Ordering::SeqCst);
            }
            _ => {}
        }
        *last = Some(end);
    }
}

fn kind_text(kind: ConflictKind) -> &'static str {
    match kind {
        ConflictKind::BothChanged => "changed on both sides",
        ConflictKind::DeletedLocally => "deleted here, changed on the server",
        ConflictKind::DeletedRemotely => "changed here, deleted on the server",
        _ => "conflicted",
    }
}

fn full_name(b: &BindingRecord) -> String {
    format!("{}/{}", b.team, b.workspace)
}

fn sync_failed(e: &FolderSyncError) -> Exit {
    match e {
        FolderSyncError::SignedOut { .. } | FolderSyncError::Api(ApiError::Unauthenticated) => {
            signed_out(e)
        }
        FolderSyncError::Api(api) => api_failed(api),
        FolderSyncError::Sync(SyncError::Git(GitError::Missing)) => git_missing(),
        _ => failed(e),
    }
}

fn signed_out(e: &dyn std::fmt::Display) -> Exit {
    eprintln!("error: {e}");
    Exit::SignedOut
}

fn git_missing() -> Exit {
    failed("git is not installed")
}
