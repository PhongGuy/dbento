//! `install`: the file swaps, the NSIS hand-off, the bundle path and the kinds left alone.
//! Permission bits, links and the bundle are Unix-only; the rest also runs on Windows.
#![cfg(feature = "update")]

#[cfg(unix)]
use crate::update_support::ModeGuard;
use crate::update_support::{leftovers, offer_as};
use dbento_core::update::{
    Format, InstallError, InstallKind, Reason, Relaunch, Staged, download, install, staging_dir,
    sweep_staged,
};
use std::ffi::OsString;
#[cfg(unix)]
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use wiremock::MockServer;

const NEW: &[u8] = b"new build";

async fn staged(dir: &Path, format: &str, body: &[u8]) -> (MockServer, Staged) {
    let s = MockServer::start().await;
    let a = offer_as(&s, format, body).await;
    let st = download(&a, dir, |_, _| {}).await.unwrap();
    (s, st)
}

fn executable(dir: &Path, name: &str) -> PathBuf {
    let target = dir.join(name);
    std::fs::write(&target, b"old").unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
    target
}

#[cfg(unix)]
fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o7777
}

#[test]
fn staging_is_beside_the_install_and_formats_must_fit() {
    let image = InstallKind::AppImage("/opt/apps/Dbento.AppImage".into());
    let bundle = InstallKind::MacApp("/Applications/Dbento.app".into());
    let cli = InstallKind::Standalone("/home/u/.local/bin/dbento".into());
    assert_eq!(staging_dir(&image), Ok(PathBuf::from("/opt/apps")));
    assert_eq!(staging_dir(&bundle), Ok(PathBuf::from("/Applications")));
    assert_eq!(staging_dir(&cli), Ok(PathBuf::from("/home/u/.local/bin")));
    assert_eq!(
        staging_dir(&InstallKind::WindowsNsis),
        Ok(std::env::temp_dir())
    );
    assert_eq!(
        staging_dir(&InstallKind::NotOwned(Reason::SystemPackage)),
        Err(InstallError::NotOwned(Reason::SystemPackage))
    );
    assert!(image.accepts(Format::AppImage) && !image.accepts(Format::Binary));
    assert!(bundle.accepts(Format::App) && !bundle.accepts(Format::AppImage));
    assert!(InstallKind::WindowsNsis.accepts(Format::Nsis));
    assert!(cli.accepts(Format::Binary) && !cli.accepts(Format::Nsis));
    assert!(!InstallKind::NotOwned(Reason::SourceBuild).accepts(Format::Binary));
}

#[tokio::test]
async fn a_standalone_binary_is_swapped_and_keeps_its_mode() {
    let dir = tempfile::tempdir().unwrap();
    let target = executable(dir.path(), "dbento");
    let (_s, st) = staged(dir.path(), "binary", NEW).await;
    assert_eq!(
        install(&InstallKind::Standalone(target.clone()), st),
        Ok(Relaunch::Nothing)
    );
    assert_eq!(std::fs::read(&target).unwrap(), NEW);
    #[cfg(unix)]
    assert_eq!(mode(&target), 0o755);
    assert_eq!(leftovers(dir.path()), ["dbento"]);
}

#[tokio::test]
async fn an_appimage_is_swapped_and_restarted() {
    let dir = tempfile::tempdir().unwrap();
    let target = executable(dir.path(), "Dbento.AppImage");
    let (_s, st) = staged(dir.path(), "appimage", NEW).await;
    assert_eq!(
        install(&InstallKind::AppImage(target.clone()), st),
        Ok(Relaunch::Restart(target.clone()))
    );
    assert_eq!(std::fs::read(&target).unwrap(), NEW);
    #[cfg(unix)]
    assert_eq!(mode(&target), 0o755);
    assert_eq!(leftovers(dir.path()), ["Dbento.AppImage"]);
}

#[cfg(unix)]
#[tokio::test]
async fn the_swap_drops_setuid_setgid_and_sticky_bits() {
    let dir = tempfile::tempdir().unwrap();
    let target = executable(dir.path(), "dbento");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o7755)).unwrap();
    let (_s, st) = staged(dir.path(), "binary", NEW).await;
    install(&InstallKind::Standalone(target.clone()), st).unwrap();
    assert_eq!(mode(&target), 0o755);
}

#[cfg(unix)]
#[tokio::test]
async fn a_read_only_directory_keeps_the_original() {
    let dir = tempfile::tempdir().unwrap();
    let target = executable(dir.path(), "dbento");
    let (_s, st) = staged(dir.path(), "binary", NEW).await;
    let guard = ModeGuard::set(dir.path(), 0o555);
    if std::fs::write(dir.path().join("probe"), b"").is_ok() {
        return; // running as root, which ignores directory permissions
    }
    let got = install(&InstallKind::Standalone(target.clone()), st);
    drop(guard);
    assert_eq!(
        got,
        Err(InstallError::NotWritable(dir.path().to_path_buf()))
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"old");
}

// Windows moves a directory aside like a file, so this rename failure is Unix-only.
#[cfg(unix)]
#[tokio::test]
async fn a_failed_swap_keeps_the_original_and_removes_the_staged_file() {
    let dir = tempfile::tempdir().unwrap();
    // A non-empty directory where the binary belongs makes the rename fail with an I/O error.
    let blocked = dir.path().join("blocked");
    std::fs::create_dir_all(blocked.join("inside")).unwrap();
    let (_s, st) = staged(dir.path(), "binary", NEW).await;
    let got = install(&InstallKind::Standalone(blocked.clone()), st);
    assert!(matches!(got, Err(InstallError::Io { .. })), "{got:?}");
    assert!(blocked.join("inside").is_dir());
    assert_eq!(leftovers(dir.path()), ["blocked"]);
}

#[tokio::test]
async fn a_file_staged_elsewhere_is_refused() {
    let (dir, other) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let target = executable(dir.path(), "dbento");
    let (_s, st) = staged(other.path(), "binary", NEW).await;
    let got = install(&InstallKind::Standalone(target.clone()), st);
    assert!(matches!(got, Err(InstallError::Io { .. })), "{got:?}");
    assert_eq!(std::fs::read(&target).unwrap(), b"old");
    assert!(leftovers(other.path()).is_empty());
}

#[tokio::test]
async fn the_nsis_installer_is_handed_back_passive_with_relaunch() {
    let dir = tempfile::tempdir().unwrap();
    let (_s, st) = staged(dir.path(), "nsis", NEW).await;
    let program = dir.path().join("dbento-update-99.0.0-setup.exe");
    let args = vec![OsString::from("/P"), OsString::from("/R")];
    assert_eq!(
        install(&InstallKind::WindowsNsis, st),
        Ok(Relaunch::Installer {
            program: program.clone(),
            args
        })
    );
    assert_eq!(std::fs::read(&program).unwrap(), NEW);
    assert_eq!(leftovers(dir.path()), ["dbento-update-99.0.0-setup.exe"]);
}

#[cfg(unix)]
#[tokio::test]
async fn a_staged_link_is_refused_for_nsis_and_only_the_link_is_removed() {
    let dir = tempfile::tempdir().unwrap();
    let (_s, st) = staged(dir.path(), "nsis", NEW).await;
    let elsewhere = dir.path().join("elsewhere");
    std::fs::write(&elsewhere, b"keep").unwrap();
    std::fs::remove_file(st.path()).unwrap();
    std::os::unix::fs::symlink(&elsewhere, st.path()).unwrap();
    let got = install(&InstallKind::WindowsNsis, st);
    assert!(matches!(got, Err(InstallError::Io { .. })), "{got:?}");
    assert_eq!(std::fs::read(&elsewhere).unwrap(), b"keep");
    assert_eq!(leftovers(dir.path()), ["elsewhere"]);
}

#[tokio::test]
async fn kinds_left_alone_are_refused_and_cleaned_up() {
    let dir = tempfile::tempdir().unwrap();
    for reason in [
        Reason::SystemPackage,
        Reason::SourceBuild,
        Reason::Translocated,
    ] {
        let (_s, st) = staged(dir.path(), "binary", NEW).await;
        assert_eq!(
            install(&InstallKind::NotOwned(reason), st),
            Err(InstallError::NotOwned(reason))
        );
        assert!(leftovers(dir.path()).is_empty(), "{reason:?}");
    }
}

#[cfg(unix)]
#[test]
fn stale_staged_files_are_swept_and_nothing_else() {
    let (dir, outside) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let kept = outside.path().join("kept");
    std::fs::write(&kept, b"keep").unwrap();
    for name in [
        "dbento-update-98.0.0",
        "dbento-update-98.0.0.AppImage.part",
        "Dbento.AppImage",
        "notes.txt",
    ] {
        std::fs::write(dir.path().join(name), b"x").unwrap();
    }
    for name in ["dbento-update-98.0.0", "dbento-update-98.0.0.AppImage.part"] {
        set_mtime(&dir.path().join(name), an_hour(-1));
    }
    std::os::unix::fs::symlink(&kept, dir.path().join("dbento-update-link")).unwrap();
    std::fs::create_dir(dir.path().join("dbento-update-dir")).unwrap();
    dbento_core::update::sweep_staged(dir.path());
    assert_eq!(
        leftovers(dir.path()),
        ["Dbento.AppImage", "dbento-update-dir", "notes.txt"]
    );
    assert_eq!(std::fs::read(&kept).unwrap(), b"keep");
}

#[cfg(unix)]
fn bundle_tarball(exe: &[u8]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for d in [
        "Dbento.app/",
        "Dbento.app/Contents/",
        "Dbento.app/Contents/MacOS/",
    ] {
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Directory);
        h.set_mode(0o755);
        h.set_size(0);
        b.append_data(&mut h, d, std::io::empty()).unwrap();
    }
    let mut h = tar::Header::new_gnu();
    h.set_mode(0o755);
    h.set_size(exe.len() as u64);
    b.append_data(&mut h, "Dbento.app/Contents/MacOS/dbento-app", exe)
        .unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&b.into_inner().unwrap()).unwrap();
    gz.finish().unwrap()
}

#[cfg(unix)]
#[tokio::test]
async fn a_mac_app_is_installed_from_its_tarball_and_restarted() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join("Dbento.app");
    std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
    std::fs::write(bundle.join("Contents/MacOS/dbento-app"), b"old exe").unwrap();
    let (_s, st) = staged(dir.path(), "app", &bundle_tarball(b"new exe")).await;
    assert_eq!(
        install(&InstallKind::MacApp(bundle.clone()), st),
        Ok(Relaunch::Restart(bundle.clone()))
    );
    assert_eq!(
        std::fs::read(bundle.join("Contents/MacOS/dbento-app")).unwrap(),
        b"new exe"
    );
    assert_eq!(leftovers(dir.path()), ["Dbento.app"]);
}

/// Set only in the copy that `a_running_cli_replaces_itself` starts.
const REPLACE_SELF: &str = "DBENTO_TEST_REPLACE_SELF";

/// The copy's half: installs a release over its own running executable, staged where the CLI
/// stages it. On Windows only this path reaches `self-replace`.
#[tokio::test]
#[ignore = "run by a_running_cli_replaces_itself, inside a copy of this binary"]
async fn replace_the_running_copy() {
    if std::env::var_os(REPLACE_SELF).is_none() {
        return; // never replace the test binary cargo built
    }
    let exe = std::env::current_exe().unwrap().canonicalize().unwrap();
    let kind = InstallKind::Standalone(exe);
    let (_s, st) = staged(&staging_dir(&kind).unwrap(), "binary", NEW).await;
    assert_eq!(install(&kind, st), Ok(Relaunch::Nothing));
}

// The test binary is copied out and rerun on one test, so the copy replaces itself while it runs.
#[test]
fn a_running_cli_replaces_itself() {
    let dir = tempfile::tempdir().unwrap();
    let copy = dir
        .path()
        .join(format!("dbento{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(std::env::current_exe().unwrap(), &copy).unwrap();
    let out = std::process::Command::new(&copy)
        .env(REPLACE_SELF, "1")
        .args([
            "update_install::replace_the_running_copy",
            "--exact",
            "--ignored",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "{out:?}"
    );
    assert_eq!(std::fs::read(&copy).unwrap(), NEW);
}

// A time an hour from now; `-1` is an hour ago.
fn an_hour(sign: i32) -> SystemTime {
    let hour = Duration::from_secs(3600);
    if sign < 0 {
        SystemTime::now() - hour
    } else {
        SystemTime::now() + hour
    }
}

// Ages or post-dates a file; a write handle is what Windows needs to set the time.
fn set_mtime(path: &Path, at: SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(at)
        .unwrap();
}

// A file another copy holds survives; an old unlocked leftover from a crash goes.
#[test]
fn a_locked_staged_file_survives_the_sweep() {
    let dir = tempfile::tempdir().unwrap();
    let held = dir
        .path()
        .join("dbento-update-98.0.0-11-1a2b-0.AppImage.part");
    let stale = dir
        .path()
        .join("dbento-update-98.0.0-12-3c4d-0.AppImage.part");
    for p in [&held, &stale] {
        std::fs::write(p, b"x").unwrap();
        set_mtime(p, an_hour(-1));
    }
    let holder = std::fs::File::open(&held).unwrap();
    holder.lock().unwrap();
    sweep_staged(dir.path());
    assert!(held.exists(), "the sweep removed a file another copy holds");
    assert!(!stale.exists(), "the sweep kept an old unlocked leftover");
    drop(holder);
    sweep_staged(dir.path());
    assert!(
        leftovers(dir.path()).is_empty(),
        "{:?}",
        leftovers(dir.path())
    );
}

// A file just created may not be locked yet, and a future time is not trusted: both stay.
#[test]
fn a_young_or_future_unlocked_file_survives_the_sweep() {
    let dir = tempfile::tempdir().unwrap();
    let fresh = dir.path().join("dbento-update-98.0.0-13-5e6f-0.part");
    let future = dir.path().join("dbento-update-98.0.0-14-7a8b-0.part");
    std::fs::write(&fresh, b"x").unwrap();
    std::fs::write(&future, b"x").unwrap();
    set_mtime(&future, an_hour(1));
    sweep_staged(dir.path());
    assert!(
        fresh.exists(),
        "the sweep removed a file inside the grace period"
    );
    assert!(
        future.exists(),
        "the sweep removed a file dated in the future"
    );
    set_mtime(&fresh, an_hour(-1));
    sweep_staged(dir.path());
    assert!(!fresh.exists(), "the sweep kept an old unlocked leftover");
}

#[tokio::test]
async fn a_held_download_survives_the_sweep_until_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let (_s, st) = staged(dir.path(), "appimage", NEW).await;
    let path = st.path().to_path_buf();
    set_mtime(&path, an_hour(-1));
    sweep_staged(dir.path());
    assert!(
        path.exists(),
        "the sweep removed a staged release still held"
    );
    drop(st);
    sweep_staged(dir.path());
    assert!(
        leftovers(dir.path()).is_empty(),
        "{:?}",
        leftovers(dir.path())
    );
}
