//! Which installs the updater may replace.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::{Format, Os, Product};

/// Whether this build came from the release workflow, which alone sets `DBENTO_RELEASE_BUILD`
/// at compile time. Callers pass it to [`install_kind`].
pub const RELEASE_BUILD: bool = option_env!("DBENTO_RELEASE_BUILD").is_some();

/// What kind of install a running executable belongs to, and so what may replace it.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallKind {
    /// A CLI binary the updater may replace in place.
    Standalone(PathBuf),
    /// The app running from this AppImage file.
    AppImage(PathBuf),
    /// The app running from this `.app` bundle directory.
    MacApp(PathBuf),
    /// The app installed by the NSIS installer.
    WindowsNsis,
    /// An install the updater must leave alone.
    NotOwned(Reason),
}

impl InstallKind {
    /// Whether a release packaged as `format` can replace this install.
    pub fn accepts(&self, format: Format) -> bool {
        matches!(
            (self, format),
            (InstallKind::Standalone(_), Format::Binary)
                | (InstallKind::AppImage(_), Format::AppImage)
                | (InstallKind::MacApp(_), Format::App)
                | (InstallKind::WindowsNsis, Format::Nsis)
        )
    }
}

/// Why the updater leaves an install alone.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Reason {
    /// It was built from source rather than by the release workflow.
    #[error("it was built from source, so download releases by hand")]
    SourceBuild,
    /// A system package manager installed it.
    #[error("it was installed by the system package manager")]
    SystemPackage,
    /// It is the CLI bundled with the app.
    #[error("it ships with the Dbento app, so update the app instead")]
    ShippedWithApp,
    /// It runs from a mounted disk image.
    #[error("it runs from a read-only disk image")]
    ReadOnlyVolume,
    /// macOS App Translocation runs it from a temporary copy.
    #[error("macOS runs it from a temporary location; move it to Applications first")]
    Translocated,
    /// No rule recognises its layout.
    #[error("the updater does not manage this install layout")]
    Unrecognised,
}

/// Classifies the install `exe` belongs to, applying the rules in order. Pure: the caller
/// passes [`RELEASE_BUILD`], canonicalizes `exe`, passes `$APPIMAGE` as `appimage_env`, and
/// sets `sibling_app` when a `dbento-app[.exe]` sits beside `exe`.
pub fn install_kind(
    product: Product,
    os: Os,
    release_build: bool,
    exe: &Path,
    appimage_env: Option<&Path>,
    sibling_app: bool,
) -> InstallKind {
    if !release_build {
        return InstallKind::NotOwned(Reason::SourceBuild);
    }
    let text = exe.to_string_lossy();
    // A path the caller failed to canonicalize could disguise a package-managed binary,
    // so it is never replaced.
    if !canonical(os, &text) {
        return InstallKind::NotOwned(Reason::Unrecognised);
    }
    // Judged on the text like `canonical`, since a Windows host reads `//usr` as a network share.
    let mut segments = text.split('/').filter(|seg| !seg.is_empty());
    if os == Os::Linux && segments.next() == Some("usr") && segments.next() != Some("local") {
        return InstallKind::NotOwned(Reason::SystemPackage);
    }
    match product {
        Product::Cli => {
            let in_appimage = os == Os::Linux && appimage_env.is_some();
            let in_bundle = os == Os::Macos && inside_app_bundle(exe);
            if in_appimage || in_bundle || sibling_app {
                InstallKind::NotOwned(Reason::ShippedWithApp)
            } else {
                InstallKind::Standalone(exe.to_path_buf())
            }
        }
        Product::App => app_kind(os, &text, exe, appimage_env),
    }
}

// The app's layouts, checked so a mounted dmg or a translocated copy wins over its bundle.
fn app_kind(os: Os, text: &str, exe: &Path, appimage_env: Option<&Path>) -> InstallKind {
    if os == Os::Macos && text.starts_with("/Volumes/") {
        return InstallKind::NotOwned(Reason::ReadOnlyVolume);
    }
    if os == Os::Macos && text.contains("/AppTranslocation/") {
        return InstallKind::NotOwned(Reason::Translocated);
    }
    if let (Os::Linux, Some(appimage)) = (os, appimage_env) {
        return InstallKind::AppImage(appimage.to_path_buf());
    }
    if os == Os::Macos
        && let Some(bundle) = mac_bundle(exe)
    {
        return InstallKind::MacApp(bundle);
    }
    if os == Os::Windows {
        return InstallKind::WindowsNsis;
    }
    InstallKind::NotOwned(Reason::Unrecognised)
}

/// Whether `text` is absolute for `os` and free of `.` and `..` segments. Judged on the text
/// rather than the host's path rules, since `os` need not be the host.
fn canonical(os: Os, text: &str) -> bool {
    let (absolute, separators): (bool, &[char]) = match os {
        Os::Windows => {
            let drive =
                matches!(text.as_bytes(), [d, b':', b'\\' | b'/', ..] if d.is_ascii_alphabetic());
            (drive || text.starts_with(r"\\"), &['\\', '/'])
        }
        Os::Linux | Os::Macos => (text.starts_with('/'), &['/']),
    };
    absolute && !text.split(separators).any(|seg| seg == "." || seg == "..")
}

/// Whether any directory above `exe` is a `.app` bundle, at any depth.
fn inside_app_bundle(exe: &Path) -> bool {
    exe.ancestors().skip(1).any(|dir| {
        dir.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .to_ascii_lowercase()
                .ends_with(".app")
        })
    })
}

/// The `X.app` directory when `exe` sits at `X.app/Contents/MacOS/<exe>`.
fn mac_bundle(exe: &Path) -> Option<PathBuf> {
    let named = |dir: &&Path, name: &str| dir.file_name() == Some(OsStr::new(name));
    let macos = exe.parent().filter(|d| named(d, "MacOS"))?;
    let contents = macos.parent().filter(|d| named(d, "Contents"))?;
    let app = contents.parent()?;
    let is_app = app.file_name()?.to_string_lossy().ends_with(".app");
    is_app.then(|| app.to_path_buf())
}
