//! The app's command line, read before GPUI starts so it works without a display.

use std::ffi::OsString;
use std::path::PathBuf;

/// What the command line asks the app to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Launch {
    /// Print the usage and exit.
    Help,
    /// Print the version and exit.
    Version,
    /// Open the window, on an absolute workspace folder when one was given.
    Open(Option<PathBuf>),
}

/// A command line the app refuses.
#[derive(Debug, PartialEq, Eq)]
pub enum LaunchError {
    /// A flag the app does not know.
    UnknownFlag(String),
    /// More than one workspace folder.
    ExtraArgument(PathBuf),
    /// The workspace folder does not exist.
    NotFound(PathBuf),
    /// The workspace path is not a folder.
    NotADirectory(PathBuf),
    /// The workspace path cannot be made absolute.
    Unresolvable(PathBuf, String),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownFlag(flag) => write!(f, "unknown option {flag}"),
            Self::ExtraArgument(p) => write!(f, "unexpected argument {}", p.display()),
            Self::NotFound(p) => write!(f, "workspace {} does not exist", p.display()),
            Self::NotADirectory(p) => write!(f, "workspace {} is not a directory", p.display()),
            Self::Unresolvable(p, e) => write!(f, "cannot resolve {}: {e}", p.display()),
        }
    }
}

impl std::error::Error for LaunchError {}

/// The text `--help` prints, and an unusable command line repeats.
pub const USAGE: &str = "\
Usage: dbento-app [WORKSPACE]

Opens the Dbento database client, on the workspace folder WORKSPACE when one is given.

Options:
  -h, --help     Print this help
  -V, --version  Print the version";

/// Reads the arguments after the program name. Help and version win over everything else;
/// an unknown flag, a second folder, or a folder that is not there is refused.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Launch, LaunchError> {
    let mut folder = None;
    for arg in args {
        match arg.to_str() {
            Some("--help" | "-h") => return Ok(Launch::Help),
            Some("--version" | "-V") => return Ok(Launch::Version),
            _ if arg.as_encoded_bytes().starts_with(b"-") => {
                return Err(LaunchError::UnknownFlag(arg.to_string_lossy().into_owned()));
            }
            _ if folder.is_some() => return Err(LaunchError::ExtraArgument(arg.into())),
            _ => folder = Some(PathBuf::from(arg)),
        }
    }
    folder.map(workspace).transpose().map(Launch::Open)
}

/// Checks the folder exists and makes it absolute now, before anything can change the
/// working directory.
fn workspace(dir: PathBuf) -> Result<PathBuf, LaunchError> {
    let abs = std::path::absolute(&dir)
        .map_err(|e| LaunchError::Unresolvable(dir.clone(), e.to_string()))?;
    match std::fs::metadata(&abs) {
        Ok(m) if m.is_dir() => Ok(abs),
        Ok(_) => Err(LaunchError::NotADirectory(dir)),
        Err(_) => Err(LaunchError::NotFound(dir)),
    }
}

/// Whether a Wayland or X11 display is named, given an environment lookup.
pub fn has_display(var: impl Fn(&str) -> Option<OsString>) -> bool {
    ["WAYLAND_DISPLAY", "DISPLAY"]
        .iter()
        .any(|v| var(v).is_some_and(|s| !s.is_empty()))
}
