//! Why an update check or download failed: one enum per operation.

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
