//! Joining a team workspace: cloning its bound branch and ignoring local database and key files.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use super::lock::git_dir;
use super::{GitError, GitOptions, SyncError, commit_pending, run};
use crate::team::{Binding, Role};

/// What a joined workspace keeps out of git: local databases, their side files and keys.
const JOIN_GITIGNORE: &str = "*.db\n*.sqlite\n*.sqlite3\n*.duckdb\n*-wal\n*-shm\n*-journal\n\
                              *.pem\n*.key\n*.p12\n*.pfx\n";

/// Why a team workspace could not be cloned.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CloneError {
    /// The target folder holds files; nothing was cloned.
    #[error("{} is not empty", dir.display())]
    NotEmpty {
        /// The target folder.
        dir: PathBuf,
    },
    /// Git failed; whatever the clone created is gone.
    #[error(transparent)]
    Git(#[from] GitError),
    /// The clone worked but the join `.gitignore` could not be committed.
    #[error("could not set up the joined workspace: {0}")]
    Setup(#[source] SyncError),
    /// Reading or creating the folders failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Clone `binding.branch` of `url` into `binding.dir` (missing or empty); write and commit the
/// join .gitignore when absent (a read member's patterns go to the local exclude file).
pub fn clone(url: &str, binding: &Binding, opts: &GitOptions) -> Result<(), CloneError> {
    let dest = binding.dir.as_path();
    let name = dest.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the workspace folder has no name",
        )
    })?;
    let parent = match dest.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    // Emptiness is settled before git runs, so a refused folder is never touched.
    let existed = match fs::read_dir(dest) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                return Err(CloneError::NotEmpty {
                    dir: dest.to_owned(),
                });
            }
            true
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(e.into()),
    };
    fs::create_dir_all(parent)?;
    let args = [
        OsStr::new("clone"),
        OsStr::new("-q"),
        OsStr::new("--branch"),
        OsStr::new(binding.branch.as_str()),
        OsStr::new("--single-branch"),
        OsStr::new("--"),
        OsStr::new(url),
        name,
    ];
    if let Err(e) = run::git_ok(&args, parent, run::Reach::Network, opts) {
        remove_created(dest, existed);
        return Err(e.into());
    }
    set_up(binding, opts)
}

/// Write and commit the join .gitignore when `binding.dir` has none; also finishes a clone
/// whose setup failed earlier. A read member gets the patterns in the repository's own
/// exclude file instead, since a local commit it can never push would stop it following.
pub(crate) fn set_up(binding: &Binding, opts: &GitOptions) -> Result<(), CloneError> {
    let dest = binding.dir.as_path();
    match binding.role {
        Role::Read => exclude_locally(dest, opts),
        Role::Write => {
            if dest.join(".gitignore").symlink_metadata().is_err() {
                fs::write(dest.join(".gitignore"), JOIN_GITIGNORE)?;
                commit_pending(binding, "Add .gitignore", opts).map_err(CloneError::Setup)?;
            }
            Ok(())
        }
    }
}

/// Appends the join patterns to `<git dir>/info/exclude` unless they are there already.
fn exclude_locally(dest: &Path, opts: &GitOptions) -> Result<(), CloneError> {
    let info = git_dir(dest, opts).map_err(CloneError::Setup)?.join("info");
    fs::create_dir_all(&info)?;
    let exclude = info.join("exclude");
    let mut text = match fs::read_to_string(&exclude) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    if !text.contains(JOIN_GITIGNORE) {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(JOIN_GITIGNORE);
        fs::write(&exclude, text)?;
    }
    Ok(())
}

/// Undo a failed clone: delete `dest`, or only empty it when it was there before. One retry
/// after a short pause covers a killed git still releasing files; a missing path is clean.
fn remove_created(dest: &Path, existed: bool) {
    for attempt in 0..2 {
        if attempt > 0 {
            thread::sleep(Duration::from_millis(50));
        }
        let outcome = if existed {
            empty_dir(dest)
        } else {
            fs::remove_dir_all(dest)
        };
        match outcome {
            Ok(()) => return,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return,
            Err(_) => {}
        }
    }
}

fn empty_dir(dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() && !path.is_symlink() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}
