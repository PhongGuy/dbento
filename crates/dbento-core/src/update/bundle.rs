//! Installing a `.app` update: a guarded tarball extraction beside the bundle and a directory swap.

use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use super::InstallError;

/// Extracts the `.app` tarball at `archive` beside `bundle` and swaps it in under the bundle's
/// own name. The archive must be a regular file in `bundle`'s directory. Any failure leaves
/// `bundle` as it was; the work directory and the archive are always removed.
#[cfg_attr(not(test), expect(dead_code, reason = "install calls it"))]
pub(crate) fn install_bundle(
    archive: &Path,
    version: &str,
    bundle: &Path,
) -> Result<(), InstallError> {
    let parent = bundle.parent().filter(|d| !d.as_os_str().is_empty());
    let beside = parent.is_some() && archive.parent() == parent && archive != bundle;
    let regular = std::fs::symlink_metadata(archive).is_ok_and(|m| m.is_file());
    let Some(parent) = parent.filter(|_| beside && regular) else {
        // Removing a link removes only the link, and a directory is never removed here.
        if archive != bundle {
            let _ = std::fs::remove_file(archive);
        }
        return Err(InstallError::Io {
            path: archive.to_path_buf(),
            message: "the staged update is not a file beside the install target".into(),
        });
    };
    let work = parent.join(format!(".dbento-update-{version}"));
    let aside = parent.join(format!(".dbento-old-{version}.app"));
    // The aside copy is spare only while a bundle is in place; if an earlier swap failed in
    // both directions it is the only original left, so it is never removed then.
    let in_place = || std::fs::symlink_metadata(bundle).is_ok_and(|m| m.is_dir());
    let result = extract_bundle(archive, &work, bundle).and_then(|new| {
        if in_place() {
            remove_tree(&aside);
        }
        swap_dirs(&new, bundle, &aside)
    });
    if in_place() {
        remove_tree(&aside);
    }
    remove_tree(&work);
    let _ = std::fs::remove_file(archive);
    result
}

/// Unpacks the gzip tarball at `archive` into a fresh `work` directory and returns the bundle
/// it holds: the first entry's top-level directory. Archive errors name the archive.
fn extract_bundle(archive: &Path, work: &Path, bundle: &Path) -> Result<PathBuf, InstallError> {
    remove_tree(work);
    #[cfg_attr(not(unix), expect(unused_mut, reason = "only unix sets a mode"))]
    let mut dir = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut dir, 0o700);
    dir.create(work).map_err(|e| failed(work, bundle, e))?;

    let bad = |message: String| InstallError::Io {
        path: archive.to_path_buf(),
        message,
    };
    let file = std::fs::File::open(archive).map_err(|e| bad(e.to_string()))?;
    // Permissions are not preserved, so tar keeps only 0o777 and drops setuid bits; the mask
    // also clears group and other write, so no installed file is writable by others.
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    tar.set_mask(0o022);
    let mut top: Option<OsString> = None;
    for entry in tar.entries().map_err(|e| bad(e.to_string()))? {
        let mut entry = entry.map_err(|e| bad(e.to_string()))?;
        check_entry(&entry, &mut top).map_err(|m| bad(m.into()))?;
        if !entry.unpack_in(work).map_err(|e| bad(e.to_string()))? {
            return Err(bad("the update has a path outside its bundle".into()));
        }
    }
    top.map(|t| work.join(t))
        .filter(|t| std::fs::symlink_metadata(t).is_ok_and(|m| m.is_dir()))
        .ok_or_else(|| bad("the update holds no bundle directory".into()))
}

/// Refuses an entry that could write outside the bundle or replace it: tar's `unpack_in`
/// strips a leading `/`, skips `..` silently and never checks a link's target. The first
/// entry must be the `*.app` directory, recorded in `top`; every later entry lies inside it.
fn check_entry<R: std::io::Read>(
    entry: &tar::Entry<'_, R>,
    top: &mut Option<OsString>,
) -> Result<(), &'static str> {
    use std::path::Component;
    // Every component plain, so a path or link target can only go down.
    let normal = |p: &Path| -> Option<Vec<OsString>> {
        let parts: Option<Vec<OsString>> = p
            .components()
            .map(|c| match c {
                Component::Normal(n) => Some(n.to_os_string()),
                Component::Prefix(_)
                | Component::RootDir
                | Component::CurDir
                | Component::ParentDir => None,
            })
            .collect();
        parts.filter(|v| !v.is_empty())
    };
    let inside = |parts: &[OsString], top: &Option<OsString>| {
        parts.len() > 1 && top.as_ref() == parts.first()
    };
    let path = entry
        .path()
        .map_err(|_| "the update has an unreadable path")?;
    let parts = normal(&path).ok_or("the update has a path outside its bundle")?;
    let kind = entry.header().entry_type();
    let Some(first) = parts.first() else {
        return Err("the update has an empty path");
    };
    if top.is_none() {
        let name = first.to_string_lossy();
        if !kind.is_dir() || parts.len() != 1 || !name.ends_with(".app") {
            return Err("the update does not start with an app bundle directory");
        }
        *top = Some(first.clone());
        return Ok(());
    }
    if !inside(&parts, top) {
        return Err("the update has a path outside its bundle");
    }
    if kind.is_dir() || kind.is_file() {
        return Ok(());
    }
    if !(kind.is_symlink() || kind.is_hard_link()) {
        return Err("the update has an entry that is not a file, directory or link");
    }
    let target = entry
        .link_name()
        .ok()
        .flatten()
        .ok_or("the update has a link without a target")?;
    // A symlink resolves from its own directory, a hard link from the archive root. Symlink
    // targets with `..` are refused, so a bundle linking upward would need this rule revisited.
    let ok = match normal(&target) {
        Some(_) if kind.is_symlink() => true,
        Some(t) => inside(&t, top),
        None => false,
    };
    if ok {
        Ok(())
    } else {
        Err("the update has a link that leaves its bundle")
    }
}

/// Moves `target` to `aside` and `new` into its place. If `new` cannot take the place, the
/// original goes back, so `target` is never left missing; the error names `target`.
fn swap_dirs(new: &Path, target: &Path, aside: &Path) -> Result<(), InstallError> {
    std::fs::rename(target, aside).map_err(|e| failed(target, target, e))?;
    std::fs::rename(new, target).map_err(|e| {
        let _ = std::fs::rename(aside, target);
        failed(target, target, e)
    })
}

/// Best-effort removal of `path` without following a link at it.
fn remove_tree(path: &Path) {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => {
            allow_owner_write(path, &m);
            drop(std::fs::remove_dir_all(path));
        }
        Ok(_) => drop(std::fs::remove_file(path)),
        Err(_) => {}
    }
}

/// Gives every real directory under `dir` owner write, so an update carrying a read-only
/// directory can still be emptied. Links are never followed; `meta` is `dir`'s own lstat.
fn allow_owner_write(dir: &Path, meta: &std::fs::Metadata) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = meta.permissions().mode();
        if mode & 0o700 != 0o700 {
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode | 0o700));
        }
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if let Ok(m) = std::fs::symlink_metadata(entry.path())
                && m.is_dir()
            {
                allow_owner_write(&entry.path(), &m);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = (dir, meta);
}

/// The install error for an I/O failure at `path`: a refused permission is
/// [`InstallError::NotWritable`] naming `target`'s directory, anything else is
/// [`InstallError::Io`].
pub(crate) fn failed(path: &Path, target: &Path, e: std::io::Error) -> InstallError {
    if e.kind() == ErrorKind::PermissionDenied {
        let dir = target.parent().unwrap_or(target);
        return InstallError::NotWritable(dir.to_path_buf());
    }
    InstallError::Io {
        path: path.to_path_buf(),
        message: e.to_string(),
    }
}

// Two attributes, not `all(test, unix)`: clippy's allow-unwrap-in-tests matches only `cfg(test)`.
#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::{install_bundle, swap_dirs};
    use crate::update::InstallError;
    use std::fs::Permissions;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};

    const V: &str = "99.0.0";

    #[derive(Clone, Copy)]
    enum E<'a> {
        Dir(&'a str),
        DirMode(&'a str, u32),
        File(&'a str, &'a [u8], u32),
        Link(&'a str, &'a str),
        Hard(&'a str, &'a str),
    }

    // Names are written raw, so absolute and `..` paths reach the archive as an attacker's would.
    fn tarball(entries: &[E]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for e in entries {
            let (name, kind, link, data, mode): (&str, tar::EntryType, &str, &[u8], u32) = match *e
            {
                E::Dir(n) => (n, tar::EntryType::Directory, "", &[][..], 0o755),
                E::DirMode(n, m) => (n, tar::EntryType::Directory, "", &[][..], m),
                E::File(n, d, m) => (n, tar::EntryType::Regular, "", d, m),
                E::Link(n, t) => (n, tar::EntryType::Symlink, t, &[][..], 0o777),
                E::Hard(n, t) => (n, tar::EntryType::Link, t, &[][..], 0o644),
            };
            let mut h = tar::Header::new_gnu();
            h.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
            h.as_old_mut().linkname[..link.len()].copy_from_slice(link.as_bytes());
            h.set_entry_type(kind);
            h.set_size(data.len() as u64);
            h.set_mode(mode);
            h.set_cksum();
            b.append(&h, data).unwrap();
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&b.into_inner().unwrap()).unwrap();
        gz.finish().unwrap()
    }

    // A bundle with a framework-style `Versions/Current -> A` link, as real bundles carry.
    fn good(top: &str) -> Vec<u8> {
        let p = |rest: &str| format!("{top}/{rest}");
        let (root, contents, macos) = (p(""), p("Contents/"), p("Contents/MacOS/"));
        let (exe, plist) = (p("Contents/MacOS/dbento-app"), p("Contents/Info.plist"));
        let (versions, a, current) = (
            p("Contents/Versions/"),
            p("Contents/Versions/A/"),
            p("Contents/Versions/Current"),
        );
        tarball(&[
            E::Dir(&root),
            E::Dir(&contents),
            E::Dir(&macos),
            E::File(&exe, b"new exe", 0o755),
            E::File(&plist, b"new plist", 0o644),
            E::Dir(&versions),
            E::Dir(&a),
            E::Link(&current, "A"),
        ])
    }

    fn bundle_in(dir: &Path) -> PathBuf {
        let bundle = dir.join("Dbento.app");
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        std::fs::write(bundle.join("Contents/MacOS/dbento-app"), b"old exe").unwrap();
        std::fs::write(bundle.join("Contents/Info.plist"), b"old plist").unwrap();
        bundle
    }

    fn old_exe(bundle: &Path) -> Vec<u8> {
        std::fs::read(bundle.join("Contents/MacOS/dbento-app")).unwrap()
    }

    // The archive as `download` stages it: beside the bundle, under the staged name.
    fn archive_in(dir: &Path, body: &[u8]) -> PathBuf {
        let path = dir.join(format!("dbento-update-{V}.app.tar.gz"));
        std::fs::write(&path, body).unwrap();
        path
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o7777
    }

    // Restores a directory's mode even when an assertion panics first.
    struct ModeGuard(PathBuf);
    impl ModeGuard {
        fn set(p: &Path, mode: u32) -> ModeGuard {
            std::fs::set_permissions(p, Permissions::from_mode(mode)).unwrap();
            ModeGuard(p.to_path_buf())
        }
    }
    impl Drop for ModeGuard {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, Permissions::from_mode(0o755));
        }
    }

    #[test]
    fn a_bundle_is_extracted_beside_and_swapped_in() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = bundle_in(dir.path());
        let archive = archive_in(dir.path(), &good("Dbento.app"));
        assert_eq!(install_bundle(&archive, V, &bundle), Ok(()));
        assert_eq!(old_exe(&bundle), b"new exe");
        assert_eq!(mode(&bundle.join("Contents/MacOS/dbento-app")), 0o755);
        let link = std::fs::read_link(bundle.join("Contents/Versions/Current")).unwrap();
        assert_eq!(link, PathBuf::from("A"));
        assert_eq!(leftovers(dir.path()), ["Dbento.app"]);
    }

    #[test]
    fn the_installed_bundle_keeps_its_name() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = bundle_in(dir.path());
        let archive = archive_in(dir.path(), &good("Other.app"));
        install_bundle(&archive, V, &bundle).unwrap();
        assert_eq!(
            std::fs::read(bundle.join("Contents/Info.plist")).unwrap(),
            b"new plist"
        );
        assert_eq!(leftovers(dir.path()), ["Dbento.app"]);
    }

    #[test]
    fn a_damaged_archive_is_refused_and_leaves_the_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = bundle_in(dir.path());
        for body in [b"not a tarball".to_vec(), tarball(&[])] {
            let archive = archive_in(dir.path(), &body);
            let got = install_bundle(&archive, V, &bundle);
            assert!(matches!(got, Err(InstallError::Io { .. })), "{got:?}");
            assert_eq!(old_exe(&bundle), b"old exe");
            assert_eq!(leftovers(dir.path()), ["Dbento.app"]);
        }
    }

    #[test]
    fn an_archive_not_beside_the_bundle_is_refused() {
        let (dir, other) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let bundle = bundle_in(dir.path());
        let archive = archive_in(other.path(), &good("Dbento.app"));
        let got = install_bundle(&archive, V, &bundle);
        assert!(matches!(got, Err(InstallError::Io { .. })), "{got:?}");
        assert_eq!(old_exe(&bundle), b"old exe");
        assert_eq!(leftovers(dir.path()), ["Dbento.app"]);
    }

    #[test]
    fn an_unwritable_parent_refuses_the_install_and_keeps_the_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = bundle_in(dir.path());
        let archive = archive_in(dir.path(), &good("Dbento.app"));
        let guard = ModeGuard::set(dir.path(), 0o555);
        if std::fs::write(dir.path().join("probe"), b"").is_ok() {
            return; // running as root, which ignores directory permissions
        }
        let got = install_bundle(&archive, V, &bundle);
        drop(guard);
        assert_eq!(
            got,
            Err(InstallError::NotWritable(dir.path().to_path_buf()))
        );
        assert_eq!(old_exe(&bundle), b"old exe");
    }

    #[test]
    fn hostile_or_malformed_archives_are_refused_and_leave_the_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = bundle_in(dir.path());
        let outside = dir.path().join("evil");
        let abs = outside.to_str().unwrap().to_owned();
        let top = E::Dir("Dbento.app/");
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("absolute path", tarball(&[E::File(&abs, b"x", 0o644)])),
            (
                "parent dir",
                tarball(&[top, E::File("Dbento.app/../evil", b"x", 0o644)]),
            ),
            (
                "link up and out",
                tarball(&[top, E::Link("Dbento.app/Contents", "../../evil")]),
            ),
            (
                "link chain",
                tarball(&[
                    top,
                    E::Dir("Dbento.app/x/"),
                    E::Link("Dbento.app/x/l1", ".."),
                    E::Link("Dbento.app/x/l3", "l1/../.."),
                ]),
            ),
            (
                "absolute link",
                tarball(&[top, E::Link("Dbento.app/Contents", "/etc")]),
            ),
            (
                "hard link out",
                tarball(&[top, E::Hard("Dbento.app/x", "Dbento.app/../evil")]),
            ),
            (
                "absolute hard link",
                tarball(&[top, E::Hard("Dbento.app/x", "/etc/passwd")]),
            ),
            ("two bundles", tarball(&[top, E::Dir("Second.app/")])),
            (
                "bundle is a file",
                tarball(&[E::File("Dbento.app", b"x", 0o755)]),
            ),
            ("not a bundle", tarball(&[E::File("Dbento/x", b"x", 0o644)])),
        ];
        for (name, body) in cases {
            let archive = archive_in(dir.path(), &body);
            let got = install_bundle(&archive, V, &bundle);
            assert!(
                matches!(got, Err(InstallError::Io { .. })),
                "{name}: {got:?}"
            );
            assert!(!outside.exists(), "{name}: wrote outside");
            assert_eq!(old_exe(&bundle), b"old exe", "{name}");
            assert_eq!(leftovers(dir.path()), ["Dbento.app"], "{name}");
        }
    }

    #[test]
    fn setuid_and_group_or_other_write_bits_are_dropped_on_extraction() {
        for file_mode in [0o4755, 0o777] {
            let dir = tempfile::tempdir().unwrap();
            let bundle = bundle_in(dir.path());
            let body = tarball(&[
                E::Dir("Dbento.app/"),
                E::File("Dbento.app/tool", b"t", file_mode),
            ]);
            install_bundle(&archive_in(dir.path(), &body), V, &bundle).unwrap();
            assert_eq!(mode(&bundle.join("tool")), 0o755, "from {file_mode:o}");
        }
    }

    // A crash between the two renames of an earlier swap leaves the original only at the aside
    // path; a later install must not delete it while nothing is in place at the bundle path.
    #[test]
    fn the_aside_copy_is_kept_when_no_bundle_is_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("Dbento.app");
        let aside = dir.path().join(format!(".dbento-old-{V}.app"));
        std::fs::create_dir(&aside).unwrap();
        std::fs::write(aside.join("marker"), b"original").unwrap();
        let got = install_bundle(&archive_in(dir.path(), &good("Dbento.app")), V, &bundle);
        assert!(matches!(got, Err(InstallError::Io { .. })), "{got:?}");
        assert_eq!(std::fs::read(aside.join("marker")).unwrap(), b"original");
        assert!(!bundle.exists());
    }

    // A bundle may carry a directory without owner write; cleanup must still remove it, or the
    // leftover work or aside directory blocks every later install of that version.
    #[test]
    fn read_only_directories_in_the_update_are_still_cleaned_up() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = bundle_in(dir.path());
        let ro = |tail: &[E<'static>]| {
            let mut entries = vec![
                E::Dir("Dbento.app/"),
                E::File("Dbento.app/ro/f", b"f", 0o644),
                E::DirMode("Dbento.app/ro/", 0o555),
            ];
            entries.extend_from_slice(tail);
            tarball(&entries)
        };
        let got = install_bundle(
            &archive_in(dir.path(), &ro(&[E::Dir("Second.app/")])),
            V,
            &bundle,
        );
        assert!(matches!(got, Err(InstallError::Io { .. })), "{got:?}");
        assert_eq!(leftovers(dir.path()), ["Dbento.app"]);
        for _ in 0..2 {
            install_bundle(&archive_in(dir.path(), &ro(&[])), V, &bundle).unwrap();
            assert_eq!(leftovers(dir.path()), ["Dbento.app"]);
        }
        assert_eq!(mode(&bundle.join("ro")) & 0o777, 0o555);
    }

    #[test]
    fn a_failed_swap_restores_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let (bundle, aside) = (dir.path().join("Dbento.app"), dir.path().join("old.app"));
        std::fs::create_dir(&bundle).unwrap();
        std::fs::write(bundle.join("marker"), b"original").unwrap();
        let missing = dir.path().join("work/Dbento.app");
        assert!(swap_dirs(&missing, &bundle, &aside).is_err());
        assert_eq!(std::fs::read(bundle.join("marker")).unwrap(), b"original");
        assert!(!aside.exists());
    }
}
