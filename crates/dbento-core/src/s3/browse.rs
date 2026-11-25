//! The public names for objects the S3 browser lists, reads and downloads.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// One object's address: a bucket and the raw key inside it.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectKey {
    /// The bucket name.
    pub bucket: String,
    /// The key as stored, undecoded; empty means the bucket root.
    pub key: String,
}

impl ObjectKey {
    /// The object `key` in `bucket`.
    pub fn new(bucket: impl Into<String>, key: impl Into<String>) -> Self {
        ObjectKey {
            bucket: bucket.into(),
            key: key.into(),
        }
    }

    /// `s3://bucket/key` with a non-empty bucket; the key may be empty. No decoding.
    pub fn parse(url: &str) -> Option<ObjectKey> {
        let rest = url.strip_prefix("s3://")?;
        let (bucket, key) = rest.split_once('/').unwrap_or((rest, ""));
        (!bucket.is_empty()).then(|| ObjectKey::new(bucket, key))
    }
}

/// What a listing or a HEAD says about one object. `last_modified` is RFC 3339 UTC and `etag`
/// has its surrounding quotes stripped.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectMeta {
    /// The raw key, decoded from the wire but not escaped for display.
    pub key: String,
    /// Size in bytes.
    pub size: u64,
    /// When the object was last written, as RFC 3339 UTC.
    pub last_modified: Option<String>,
    /// The entity tag without its surrounding quotes.
    pub etag: Option<String>,
    /// The storage class, such as `STANDARD`.
    pub storage_class: Option<String>,
    /// The MIME type; only a HEAD or GET reports it.
    pub content_type: Option<String>,
}

/// The most bytes one [`ObjectRead`] returns, whatever it asks for.
pub const MAX_READ_BYTES: u32 = 1 << 20;

/// Where a listing resumes: the store's continuation token, opaque to the caller.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ObjectCursor {
    pub(crate) token: String,
}

impl ObjectCursor {
    /// A cursor resuming at `token`, for tests that need a page token without a store.
    #[doc(hidden)]
    pub fn new(token: impl Into<String>) -> Self {
        ObjectCursor {
            token: token.into(),
        }
    }
}

/// One page request of a bucket listing.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectList {
    /// The bucket to list.
    pub bucket: String,
    /// Only keys starting with this; empty lists from the bucket root.
    pub prefix: String,
    /// Groups keys past this character into prefixes; `None` lists every key below `prefix`.
    pub delimiter: Option<char>,
    /// Where the page resumes; `None` for the first page.
    pub cursor: Option<ObjectCursor>,
    /// How many keys and prefixes the page aims for, clamped to 1..=1000 when sent.
    pub want: u32,
}

impl ObjectList {
    /// The first page of one level under `prefix` in `bucket`, up to 1,000 entries.
    pub fn new(bucket: impl Into<String>, prefix: impl Into<String>) -> Self {
        ObjectList {
            bucket: bucket.into(),
            prefix: prefix.into(),
            delimiter: Some('/'),
            cursor: None,
            want: 1000,
        }
    }

    /// Lists every key below the prefix instead of one level.
    #[must_use]
    pub fn recursive(mut self) -> Self {
        self.delimiter = None;
        self
    }

    /// Resumes where the page that returned `cursor` stopped.
    #[must_use]
    pub fn after(mut self, cursor: ObjectCursor) -> Self {
        self.cursor = Some(cursor);
        self
    }

    /// Aims for `n` entries per page.
    #[must_use]
    pub fn want(mut self, n: u32) -> Self {
        self.want = n;
        self
    }
}

/// One page of a listing: the prefixes one level down, the objects, and where the next page
/// starts when there is one.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectPage {
    /// Prefixes ending in the delimiter, in listing order.
    pub prefixes: Vec<String>,
    /// The objects, in listing order.
    pub objects: Vec<ObjectMeta>,
    /// The cursor for the next page; `None` on the last one.
    pub next: Option<ObjectCursor>,
}

/// A request for an object's first bytes.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectRead {
    /// The object to read.
    pub key: ObjectKey,
    /// The most bytes wanted, capped at [`MAX_READ_BYTES`].
    pub max_bytes: u32,
}

impl ObjectRead {
    /// The first `max_bytes` bytes of `key`.
    pub fn new(key: ObjectKey, max_bytes: u32) -> Self {
        ObjectRead { key, max_bytes }
    }
}

/// An object's first bytes, with its whole size.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectChunk {
    /// The bytes read from the start of the object.
    pub bytes: Vec<u8>,
    /// The object's whole size in bytes.
    pub total_size: u64,
    /// True when `bytes` is shorter than the object.
    pub truncated: bool,
    /// The MIME type the store reports.
    pub content_type: Option<String>,
}

/// Why the object browser could not answer.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ObjectBrowseError {
    /// Not an S3 session.
    #[error("unsupported: {reason}")]
    Unsupported {
        /// Why, for the user.
        reason: String,
    },
    /// No such bucket or key.
    #[error("not found: {key}")]
    NotFound {
        /// What was asked for, as `s3://bucket/key`.
        key: String,
    },
    /// The credentials may not read this.
    #[error("access denied: {message}")]
    AccessDenied {
        /// The store's words.
        message: String,
    },
    /// The bucket lives in another region.
    #[error("wrong region: {hint}")]
    WrongRegion {
        /// How to fix the connection.
        hint: String,
    },
    /// Any other refusal by the store or the network.
    #[error("{message} ({code})")]
    Database {
        /// The S3 error code, `HTTP <status>`, or `transport`.
        code: String,
        /// The store's words.
        message: String,
        /// What to try, where one helps.
        hint: Option<String>,
    },
    /// The session timeout passed.
    #[error("the browse timed out")]
    Timeout,
    /// The session ended, now or earlier.
    #[error("the session is closed: {reason}")]
    SessionEnded {
        /// Why it ended.
        reason: String,
    },
}

/// The largest object a download takes unless told otherwise: 1 GiB.
pub const DEFAULT_MAX_DOWNLOAD: u64 = 1 << 30;

/// How a download behaves: its size cap, whether it replaces an existing file, and how long it
/// waits for the next bytes before giving up.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct DownloadOptions {
    /// The largest object accepted, in bytes.
    pub max_size: u64,
    /// Replace an existing destination instead of refusing it.
    pub force: bool,
    /// The longest wait for any one response or body chunk.
    pub idle_timeout: Duration,
    /// Where the download reports its bytes, if anyone watches.
    pub progress: Option<DownloadProgress>,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        DownloadOptions {
            max_size: DEFAULT_MAX_DOWNLOAD,
            force: false,
            idle_timeout: Duration::from_secs(60),
            progress: None,
        }
    }
}

impl DownloadOptions {
    /// Accepts objects up to `n` bytes.
    #[must_use]
    pub fn with_max_size(mut self, n: u64) -> Self {
        self.max_size = n;
        self
    }

    /// Replaces an existing destination when `f` is true.
    #[must_use]
    pub fn with_force(mut self, f: bool) -> Self {
        self.force = f;
        self
    }

    /// Waits at most `d` for each response or body chunk.
    #[must_use]
    pub fn with_idle_timeout(mut self, d: Duration) -> Self {
        self.idle_timeout = d;
        self
    }

    /// Reports the object's size and each written chunk to `p`.
    #[must_use]
    pub fn with_progress(mut self, p: DownloadProgress) -> Self {
        self.progress = Some(p);
        self
    }
}

/// A shared counter a running download updates: the bytes written so far and the object's
/// size once its HEAD answers. Clones watch the same download.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct DownloadProgress {
    inner: Arc<Counts>,
}

/// The total starts at `u64::MAX`, which stands for a size not yet known.
#[derive(Debug)]
struct Counts {
    written: AtomicU64,
    total: AtomicU64,
}

impl Default for Counts {
    fn default() -> Self {
        Counts {
            written: AtomicU64::new(0),
            total: AtomicU64::new(u64::MAX),
        }
    }
}

impl DownloadProgress {
    /// A counter with nothing written and no size known.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The bytes written to the partial file so far.
    #[must_use]
    pub fn written(&self) -> u64 {
        self.inner.written.load(Ordering::Relaxed)
    }

    /// The object's size, once the HEAD has reported it.
    #[must_use]
    pub fn total(&self) -> Option<u64> {
        Some(self.inner.total.load(Ordering::Relaxed)).filter(|t| *t != u64::MAX)
    }

    #[cfg(feature = "s3")]
    pub(crate) fn set_total(&self, n: u64) {
        self.inner.total.store(n, Ordering::Relaxed);
    }

    #[cfg(feature = "s3")]
    pub(crate) fn add(&self, n: u64) {
        self.inner.written.fetch_add(n, Ordering::Relaxed);
    }
}

/// A finished download: how many bytes landed and where.
#[non_exhaustive]
#[derive(Debug)]
pub struct DownloadReport {
    /// The bytes written, equal to the object's length.
    pub bytes: u64,
    /// The destination file.
    pub path: PathBuf,
}

/// Why a download did not land. Every failure leaves no partial file behind.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    /// The object is larger than `max_size`.
    #[error("the object is {size} bytes, over the {limit}-byte limit")]
    TooLarge {
        /// The object's size in bytes.
        size: u64,
        /// The limit it broke.
        limit: u64,
    },
    /// The destination already exists, as a file, directory or link.
    #[error("the destination already exists")]
    Exists,
    /// The destination could not be written.
    #[error("cannot write the destination: {0}")]
    Destination(std::io::ErrorKind),
    /// The body ended short of, or ran past, its expected length.
    #[error("the download was cut short")]
    Truncated,
    /// The download was cancelled.
    #[error("the download was cancelled")]
    Cancelled,
    /// The store refused the HEAD or GET.
    #[error("{0}")]
    Remote(ObjectBrowseError),
    /// No response or bytes arrived within the idle timeout.
    #[error("the download stalled past its idle timeout")]
    Timeout,
}

#[cfg(feature = "s3")]
mod calls {
    use std::sync::atomic::Ordering;

    use super::{
        MAX_READ_BYTES, ObjectBrowseError, ObjectChunk, ObjectCursor, ObjectKey, ObjectList,
        ObjectMeta, ObjectPage, ObjectRead,
    };
    use crate::s3::client::{ListCall, MAX_KEYS};
    use crate::s3::driver::S3Session;
    use crate::s3::errors::{S3Failure, browse_error, classify};

    impl S3Session {
        /// One page of `req`, asking for at most 1,000 entries whatever `want` says.
        pub(crate) async fn list_objects(
            &self,
            req: &ObjectList,
        ) -> Result<ObjectPage, ObjectBrowseError> {
            let call = ListCall {
                bucket: req.bucket.clone(),
                prefix: req.prefix.clone(),
                delimiter: req.delimiter,
                token: req.cursor.as_ref().map(|c| c.token.clone()),
                max_keys: req.want.clamp(1, MAX_KEYS.unsigned_abs()),
            };
            let at = ObjectKey::new(req.bucket.as_str(), req.prefix.as_str());
            let listed = self
                .browse(&at, async {
                    self.client.list_objects(&call).await.map_err(classify)
                })
                .await?;
            Ok(ObjectPage {
                prefixes: listed.prefixes,
                objects: listed.objects,
                next: listed.next_token.map(|token| ObjectCursor { token }),
            })
        }

        pub(crate) async fn stat_object(
            &self,
            key: &ObjectKey,
        ) -> Result<ObjectMeta, ObjectBrowseError> {
            let call = async {
                self.client
                    .head_object(&key.bucket, &key.key)
                    .await
                    .map_err(classify)
            };
            self.browse(key, call).await
        }

        /// The object's first bytes, at most [`MAX_READ_BYTES`]. An empty object answers a
        /// range with `InvalidRange`, so that reads as empty.
        pub(crate) async fn read_object(
            &self,
            req: &ObjectRead,
        ) -> Result<ObjectChunk, ObjectBrowseError> {
            let cap = req.max_bytes.min(MAX_READ_BYTES);
            let key = &req.key;
            let call = async {
                match self.client.get_range(&key.bucket, &key.key, cap).await {
                    Ok(read) => Ok(Some(read)),
                    Err(e) => {
                        let failure = classify(e);
                        if failure == S3Failure::InvalidRange {
                            Ok(None)
                        } else {
                            Err(failure)
                        }
                    }
                }
            };
            let Some(read) = self.browse(key, call).await? else {
                return Ok(ObjectChunk {
                    bytes: Vec::new(),
                    total_size: 0,
                    truncated: false,
                    content_type: None,
                });
            };
            // The range asks for one byte at least, so a zero cap drops it here.
            let mut bytes = read.bytes;
            bytes.truncate(usize::try_from(cap).unwrap_or(usize::MAX));
            Ok(ObjectChunk {
                truncated: (bytes.len() as u64) < read.total_size,
                bytes,
                total_size: read.total_size,
                content_type: read.content_type,
            })
        }

        /// Runs one browse call under the session timeout, refusing it on a closed session.
        async fn browse<T>(
            &self,
            at: &ObjectKey,
            call: impl Future<Output = Result<T, S3Failure>>,
        ) -> Result<T, ObjectBrowseError> {
            if self.shared.closed.load(Ordering::Relaxed) {
                return Err(ObjectBrowseError::SessionEnded {
                    reason: "the session was closed".to_owned(),
                });
            }
            let out = match self.shared.timeout {
                Some(limit) => tokio::time::timeout(limit, call)
                    .await
                    .map_err(|_| ObjectBrowseError::Timeout)?,
                None => call.await,
            };
            out.map_err(|f| browse_error(f, &format!("s3://{}/{}", at.bucket, at.key)))
        }
    }
}
