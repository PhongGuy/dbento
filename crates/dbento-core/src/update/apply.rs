//! Installing a verified update: a same-directory file swap, the `.app` bundle swap, or the
//! NSIS installer handed back to the caller. Nothing here starts a process.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::bundle::{failed, install_bundle};
use super::{InstallError, InstallKind, Reason, Relaunch, Staged};

/// Where an update for `kind` is staged: the target's own directory, so the swap is one
/// rename, or the system temp directory for the NSIS installer. A kind left alone is
/// [`InstallError::NotOwned`].
pub fn staging_dir(kind: &InstallKind) -> Result<PathBuf, InstallError> {
    match kind {
        InstallKind::Standalone(p) | InstallKind::AppImage(p) | InstallKind::MacApp(p) => p
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .ok_or(InstallError::NotOwned(Reason::Unrecognised)),
        InstallKind::WindowsNsis => Ok(std::env::temp_dir()),
        InstallKind::NotOwned(r) => Err(InstallError::NotOwned(*r)),
    }
}

/// Installs the verified `staged` release for `kind` and says how the new program starts.
/// The install target is unchanged on every error, and the staged file is removed then.
pub fn install(kind: &InstallKind, staged: Staged) -> Result<Relaunch, InstallError> {
    // Windows locks are mandatory, so the hold goes before anything reads or moves the file.
    let mut staged = staged;
    drop(staged.hold.take());
    match kind {
        InstallKind::Standalone(target) => {
            replace_file(&staged, target)?;
            Ok(Relaunch::Nothing)
        }
        InstallKind::AppImage(target) => {
            replace_file(&staged, target)?;
            Ok(Relaunch::Restart(target.clone()))
        }
        InstallKind::MacApp(bundle) => {
            install_bundle(staged.path(), staged.version(), bundle)?;
            Ok(Relaunch::Restart(bundle.clone()))
        }
        InstallKind::WindowsNsis => hand_off_installer(&staged),
        InstallKind::NotOwned(r) => {
            let _ = std::fs::remove_file(staged.path());
            Err(InstallError::NotOwned(*r))
        }
    }
}

/// A staged file younger than this may still be between its creation and its lock.
const SWEEP_GRACE: Duration = Duration::from_secs(60);

/// Removes the leftovers in `dir` whose names start with `dbento-update-`, best effort: links
/// always, a regular file only when it is older than a minute and no running copy holds it.
/// Directories and every other name are left alone.
pub fn sweep_staged(dir: &Path) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let ours = entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with("dbento-update-"));
        // lstat, so a link is removed itself and never followed.
        let Some(meta) = ours
            .then(|| std::fs::symlink_metadata(entry.path()).ok())
            .flatten()
        else {
            continue;
        };
        if meta.is_symlink() {
            let _ = std::fs::remove_file(entry.path());
        } else if meta.is_file() && old_enough(&meta) {
            remove_unheld(&entry.path());
        }
    }
}

// An unreadable or future mtime is not trusted, so such a file is kept.
fn old_enough(meta: &std::fs::Metadata) -> bool {
    meta.modified()
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .is_some_and(|age| age >= SWEEP_GRACE)
}

// Only a file whose lock we can take is a crash leftover; the lock is held across the removal
// so no other copy can claim it in between. A filesystem without locks keeps everything.
fn remove_unheld(path: &Path) {
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    if file.try_lock().is_ok() {
        let _ = std::fs::remove_file(path);
    }
    drop(file);
}

/// Swaps `staged` over `target`. Only a regular file beside the target is accepted, which
/// keeps the swap a same-directory rename; the target itself is never removed on failure.
fn replace_file(staged: &Staged, target: &Path) -> Result<(), InstallError> {
    let path = staged.path();
    let beside = path.parent().is_some() && path.parent() == target.parent() && path != target;
    let regular = std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file());
    let result = if beside && regular {
        swap(path, target)
    } else {
        Err(InstallError::Io {
            path: path.to_path_buf(),
            message: "the staged update is not a file beside the install target".into(),
        })
    };
    if result.is_err() && path != target {
        let _ = std::fs::remove_file(path);
    }
    result
}

/// Unix: give the staged file the target's permission bits (never setuid, setgid or sticky),
/// then one atomic `rename` over it.
#[cfg(unix)]
fn swap(staged: &Path, target: &Path) -> Result<(), InstallError> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::metadata(target)
        .map_err(|e| failed(target, target, e))?
        .permissions()
        .mode();
    std::fs::set_permissions(staged, std::fs::Permissions::from_mode(mode & 0o777))
        .map_err(|e| failed(staged, target, e))?;
    std::fs::rename(staged, target).map_err(|e| failed(target, target, e))
}

/// Windows cannot rename over a running executable: `self-replace` handles the running one,
/// and any other target is moved aside, then restored if the staged file cannot take its place.
#[cfg(windows)]
fn swap(staged: &Path, target: &Path) -> Result<(), InstallError> {
    let running = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .ok()
        .zip(std::fs::canonicalize(target).ok())
        .is_some_and(|(exe, t)| exe == t);
    if running {
        self_replace::self_replace(staged).map_err(|e| failed(target, target, e))?;
        let _ = std::fs::remove_file(staged);
        return Ok(());
    }
    let mut old = target.as_os_str().to_owned();
    old.push(".old");
    let old = PathBuf::from(old);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(target, &old).map_err(|e| failed(target, target, e))?;
    // A failed restore leaves the previous version only at `.old`, so the error names it.
    std::fs::rename(staged, target).map_err(|e| match std::fs::rename(&old, target) {
        Ok(()) => failed(target, target, e),
        Err(restore) => InstallError::Io {
            message: format!(
                "{e}; the previous version could not be put back ({restore}) and is at {}",
                old.display()
            ),
            path: old.clone(),
        },
    })?;
    let _ = std::fs::remove_file(&old);
    Ok(())
}

/// Windows runs only a file named like an executable, so the installer is renamed to an
/// `-setup.exe` beside where it was staged; `/P /R` makes it passive and relaunch the app.
fn hand_off_installer(staged: &Staged) -> Result<Relaunch, InstallError> {
    let path = staged.path();
    if !std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file()) {
        // Removing a link removes only the link; a directory is never removed here.
        let _ = std::fs::remove_file(path);
        return Err(InstallError::Io {
            path: path.to_path_buf(),
            message: "the staged update is not a regular file".into(),
        });
    }
    let program = path.with_file_name(format!("dbento-update-{}-setup.exe", staged.version()));
    std::fs::rename(path, &program).map_err(|e| {
        let _ = std::fs::remove_file(path);
        failed(&program, &program, e)
    })?;
    let args = vec![OsString::from("/P"), OsString::from("/R")];
    Ok(Relaunch::Installer { program, args })
}
