//! The public names for objects the S3 browser lists, reads and downloads.

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
