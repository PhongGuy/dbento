//! A per-repository advisory lock, `dbento-team.lock` in the git dir, held around a sync so
//! two Dbento processes never rewrite the same workspace at once.

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{GitError, GitOptions, SyncError, run};

/// A held repository lock, released when dropped or when the process exits.
#[derive(Debug)]
#[non_exhaustive]
#[must_use]
pub struct RepoLock {
    _file: File,
}

/// Take the lock of the repository holding `dir`, retrying for up to the options' lock wait.
/// Another holder for the whole wait is `Busy`; any other failure is an error, never an
/// unlocked go-ahead.
pub fn repo_lock(dir: &Path, opts: &GitOptions) -> Result<RepoLock, SyncError> {
    let lock = git_dir(dir, opts)?.join("dbento-team.lock");
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock)?;
    // Poll rather than block, so the wait stays bounded on every platform.
    let deadline = Instant::now() + opts.lock_wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(RepoLock { _file: file }),
            Err(TryLockError::Error(e)) => return Err(e.into()),
            Err(TryLockError::WouldBlock) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(SyncError::Busy { lock });
                }
                std::thread::sleep(left.min(Duration::from_millis(50)));
            }
        }
    }
}

/// The absolute git dir of the repository holding `dir`.
fn git_dir(dir: &Path, opts: &GitOptions) -> Result<PathBuf, SyncError> {
    let out = run::git_ok(
        &["rev-parse", "--absolute-git-dir"],
        dir,
        run::Reach::Local,
        opts,
    )?;
    let text = String::from_utf8(out.stdout)
        .map_err(|e| GitError::Unexpected(format!("git dir is not UTF-8: {e}")))?;
    Ok(PathBuf::from(text.trim_end_matches(['\n', '\r'])))
}
