//! The folder outside the workspace that keeps the local side of each pull conflict.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::paths::StatePaths;

/// One workspace's conflict copies, each at its path relative to the workspace, and beside
/// them a folder of empty marks for conflicted local deletes, which leave no bytes to copy.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConflictStore {
    copies: PathBuf,
    marks: PathBuf,
}

impl ConflictStore {
    /// The store of workspace `dir` under `paths.team_conflicts`, named by the first 16 hex
    /// digits of the SHA-256 of its canonical path, so every spelling of `dir` shares one
    /// store. Fails when `dir` cannot be canonicalized.
    pub fn for_workspace(paths: &StatePaths, dir: &Path) -> io::Result<ConflictStore> {
        let canonical = fs::canonicalize(dir)?;
        let digest = Sha256::digest(path_bytes(&canonical));
        let name = hex::encode(&digest[..8]);
        Ok(ConflictStore {
            copies: paths.team_conflicts.join(&name),
            marks: paths.team_conflicts.join(format!("{name}.deleted")),
        })
    }

    /// Where the local copy of workspace path `rel` is kept.
    pub fn copy_path(&self, rel: &str) -> PathBuf {
        self.copies.join(rel)
    }

    /// Where the mark of a conflicted local delete of `rel` is kept.
    pub fn mark_path(&self, rel: &str) -> PathBuf {
        self.marks.join(rel)
    }

    /// Every copy's `/`-separated path, sorted; empty when the folder is missing.
    pub fn copies(&self) -> io::Result<Vec<String>> {
        list(&self.copies)
    }

    /// Every conflicted local delete's `/`-separated path, sorted; empty when none.
    pub fn deleted(&self) -> io::Result<Vec<String>> {
        list(&self.marks)
    }
}

#[cfg(unix)]
fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_bytes(path: &Path) -> Vec<u8> {
    path.to_string_lossy().into_owned().into_bytes()
}

/// Every regular file below `dir`, sorted; a missing `dir` is an empty list.
fn list(dir: &Path) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    match collect(dir, "", &mut out) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        other => other?,
    }
    out.sort();
    Ok(out)
}

/// Every regular file under `dir`, named `prefix` plus its `/`-joined path below `dir`.
fn collect(dir: &Path, prefix: &str, out: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = format!("{prefix}{name}");
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect(&entry.path(), &format!("{rel}/"), out)?;
        } else if kind.is_file() {
            out.push(rel);
        }
    }
    Ok(())
}
