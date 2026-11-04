//! Folder paths that group connections in a workspace, and the items a folder tree holds.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

/// A validated `/`-separated folder path such as `Prod/EU west`.
///
/// Every segment is non-empty, has no leading or trailing whitespace and no control
/// character. Paths order segment by segment, so a folder sorts right before its children.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FolderPath {
    text: String,
}

impl FolderPath {
    /// The path as written, segments joined by `/`.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The segments from the root folder down.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.text.split('/')
    }

    /// The last segment, the folder's own name.
    pub fn name(&self) -> &str {
        self.text.rsplit('/').next().unwrap_or(&self.text)
    }

    /// The enclosing folder, or `None` for a top-level folder.
    pub fn parent(&self) -> Option<FolderPath> {
        let (parent, _) = self.text.rsplit_once('/')?;
        Some(FolderPath {
            text: parent.to_owned(),
        })
    }
}

// Tree order: compare segment lists, so `Prod/EU` sorts before `Prod-x` even though
// `-` precedes `/` in plain string order.
impl Ord for FolderPath {
    fn cmp(&self, other: &Self) -> Ordering {
        self.segments().cmp(other.segments())
    }
}

impl PartialOrd for FolderPath {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for FolderPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl FromStr for FolderPath {
    type Err = FolderPathError;

    // Faults are checked from coarsest to finest, so a path reports the first that applies.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let path = || s.to_owned();
        if s.split('/').all(str::is_empty) {
            return Err(FolderPathError::Empty);
        }
        if s.split('/').any(str::is_empty) {
            return Err(FolderPathError::EmptySegment { path: path() });
        }
        if s.split('/').any(|seg| seg.trim() != seg) {
            return Err(FolderPathError::Untrimmed { path: path() });
        }
        if s.chars().any(char::is_control) {
            return Err(FolderPathError::Control { path: path() });
        }
        Ok(FolderPath { text: path() })
    }
}

/// Zero-width characters DataGrip users paste into names to force a sort order.
pub(crate) fn is_invisible(c: char) -> bool {
    matches!(c, '\u{200B}' | '\u{FEFF}')
}

/// `text` without zero-width characters, so a path written by hand names the folder it
/// looks like rather than a distinct one.
pub(crate) fn visible(text: &str) -> Cow<'_, str> {
    if text.contains(is_invisible) {
        Cow::Owned(text.chars().filter(|&c| !is_invisible(c)).collect())
    } else {
        Cow::Borrowed(text)
    }
}

/// Parses a path read from `dbento.toml`, zero-width characters dropped first.
pub(crate) fn parse_written(text: &str) -> Result<FolderPath, FolderPathError> {
    visible(text).parse()
}

/// Why a string is not a valid [`FolderPath`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FolderPathError {
    /// The path has no text, or only separators.
    #[error("folder path is empty")]
    Empty,
    /// Two separators touch, or the path starts or ends with one.
    #[error("folder path `{path}` has an empty segment")]
    EmptySegment {
        /// The rejected path.
        path: String,
    },
    /// A segment starts or ends with whitespace.
    #[error("folder path `{path}` has a segment with leading or trailing whitespace")]
    Untrimmed {
        /// The rejected path.
        path: String,
    },
    /// The path contains a control character.
    #[error("folder path `{path}` has a control character")]
    Control {
        /// The rejected path.
        path: String,
    },
}

/// One entry in a workspace tree: a folder or a connection, named by its id.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WorkspaceItem {
    /// A folder, by its full path.
    Folder(FolderPath),
    /// A connection, by its name in `dbento.toml`.
    Connection(String),
}
