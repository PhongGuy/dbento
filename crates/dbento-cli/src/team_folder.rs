//! `dbento team join | status | sync | resolve | unlink`: one core call each, printed and mapped
//! to an exit code. Results go to stdout, notes to stderr.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use dbento_core::paths::StatePaths;
use dbento_core::{
    ApiError, BindingRecord, CloneError, ConflictKind, ConflictStore, FolderSync, FolderSyncError,
    GitError, GitOptions, JoinError, Resolution, ServerUrl, SyncError, SyncOutcome, TeamAccount,
    TeamFile, TeamResolveError, TokenStore, secret_env_var, team_join, team_resolve, team_sync,
    unpushed,
};

use crate::Exit;
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
            println!("read-only: pulled {name}; local changes stay unpushed");
            Exit::Ok
        }
        SyncOutcome::ReadOnlyBlocked(block) => {
            println!("read-only: {name} not updated; {block}");
            Exit::Ok
        }
        SyncOutcome::Refused(refusal) => {
            for row in refusal.rows() {
                println!("refused: {row}");
            }
            Exit::Refused
        }
        SyncOutcome::Conflicted(files) => {
            let store = ConflictStore::for_workspace(paths, &done.dir).ok();
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
                            done.dir.join(&f.path).display()
                        );
                    }
                }
            }
            println!("settle each with: dbento team resolve <path> --keep-mine | --take-theirs");
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
