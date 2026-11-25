//! S3 object browsing: the command language and its helpers.

pub(crate) mod browse;
#[cfg(feature = "s3")]
pub(crate) mod client;
#[cfg(feature = "s3")]
pub(crate) mod command;
#[cfg(feature = "s3")]
pub(crate) mod download;
#[cfg(feature = "s3")]
pub(crate) mod driver;
#[cfg(feature = "s3")]
pub(crate) mod errors;
pub(crate) mod lex;
pub(crate) mod names;
// The read-only allowlist the client's methods mirror; only its own test reads it.
#[cfg(test)]
pub(crate) mod ops;
#[cfg(all(test, feature = "s3"))]
pub(crate) mod replay;
#[cfg(feature = "s3")]
pub(crate) mod stream;

pub use browse::{
    DEFAULT_MAX_DOWNLOAD, DownloadError, DownloadOptions, DownloadProgress, DownloadReport,
    MAX_READ_BYTES, ObjectBrowseError, ObjectChunk, ObjectCursor, ObjectKey, ObjectList,
    ObjectMeta, ObjectPage, ObjectRead,
};
