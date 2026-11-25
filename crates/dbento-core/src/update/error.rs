//! Why an update check or download failed: one enum per operation.

use std::path::PathBuf;

use super::Reason;

/// Why an update check failed.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CheckError {
    /// This build has no manifest URL, download page or trusted key, or no release targets it.
    #[error("update checks are not configured for this build")]
    Disabled,
    /// The server could not be reached.
    #[error("could not reach the update server: {0}")]
    Offline(String),
    /// The server took longer than its time limit.
    #[error("the update server timed out")]
    Timeout,
    /// The manifest request answered with a non-success status.
    #[error("the update server answered HTTP {0}")]
    Http(u16),
    /// The manifest, or a configured URL, breaks the manifest rules.
    #[error("the update manifest is invalid: {0}")]
    ManifestInvalid(String),
    /// The manifest signature is missing, malformed or does not verify.
    #[error("the update manifest signature is invalid: {0}")]
    ManifestSignature(String),
    /// The manifest is signed by none of the trusted keys.
    #[error("the update manifest is signed by an unknown key")]
    UnknownKey,
    /// The release has no artifact for this target id.
    #[error("the release has no download for {0}")]
    NoEntryForTarget(String),
}

/// Why an artifact download failed. Every failure leaves no partial file behind.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DownloadError {
    /// The server could not be reached.
    #[error("could not reach the download server: {0}")]
    Offline(String),
    /// The download took longer than its time limit.
    #[error("the download timed out")]
    Timeout,
    /// The server answered with a non-success status other than 404.
    #[error("the download server answered HTTP {0}")]
    Http(u16),
    /// The server has no artifact at the manifest's URL.
    #[error("the update artifact is missing from the server")]
    ArtifactMissing,
    /// The artifact is longer or shorter than the manifest states.
    #[error("the update artifact should be {expected} bytes, got {got}")]
    SizeMismatch {
        /// The size the manifest lists.
        expected: u64,
        /// The bytes received, or the count that first passed `expected`.
        got: u64,
    },
    /// The artifact's SHA-256 differs from the manifest's.
    #[error("the update artifact does not match its checksum")]
    HashMismatch,
    /// The staging directory does not accept a new file.
    #[error("cannot write the update to {}", .0.display())]
    NotWritable(PathBuf),
    /// Writing or moving the staged file failed.
    #[error("could not write {}: {message}", path.display())]
    Io {
        /// The file being written or moved.
        path: PathBuf,
        /// The system's description of the failure.
        message: String,
    },
}

/// Why installing a staged update failed. The install target is unchanged on every error.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    /// The install belongs to something the updater leaves alone.
    #[error("this install is not updated in place: {0}")]
    NotOwned(Reason),
    /// The install's directory does not accept the new files.
    #[error("cannot write the update into {}", .0.display())]
    NotWritable(PathBuf),
    /// Reading the staged update or replacing the install failed.
    #[error("could not install {}: {message}", path.display())]
    Io {
        /// The file or directory the failure concerns.
        path: PathBuf,
        /// The system's description of the failure.
        message: String,
    },
}

/// Why the release notes could not be loaded. The update itself is still offered.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NotesError {
    /// The manifest lists no release notes for this release.
    #[error("this release has no release notes")]
    NotPublished,
    /// The server could not be reached.
    #[error("could not reach the release notes server: {0}")]
    Offline(String),
    /// The server took longer than its time limit.
    #[error("the release notes server timed out")]
    Timeout,
    /// The server answered with a non-success status.
    #[error("the release notes server answered HTTP {0}")]
    Http(u16),
    /// The file is larger than any release notes may be.
    #[error("the release notes are too large")]
    TooLarge,
    /// The file is longer or shorter than the manifest states.
    #[error("the release notes are not the size the manifest states")]
    SizeMismatch,
    /// The file's SHA-256 differs from the manifest's.
    #[error("the release notes do not match their checksum")]
    HashMismatch,
    /// The file uses a schema this version does not read.
    #[error("the release notes use unsupported schema {0}")]
    UnsupportedSchema(u64),
    /// The file is not valid release notes.
    #[error("the release notes are invalid: {0}")]
    Invalid(String),
}
