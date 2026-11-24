//! Downloads one S3 object to disk through a hidden sibling partial file.

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use super::browse::{DownloadError, DownloadOptions, DownloadReport, ObjectBrowseError, ObjectKey};
use super::client::{S3Client, Sdk};
use super::driver::S3Session;
use super::errors::{browse_error, classify};
use crate::aws::AwsCancel;

#[cfg(test)]
mod support;
#[cfg(test)]
mod tests;

impl S3Session {
    /// Downloads `key` to `dest` under the session's cancel handle.
    pub(crate) async fn download_object(
        &self,
        key: &ObjectKey,
        dest: &Path,
        opts: &DownloadOptions,
    ) -> Result<DownloadReport, DownloadError> {
        if self.shared.closed.load(Ordering::Relaxed) {
            return Err(DownloadError::Remote(ObjectBrowseError::SessionEnded {
                reason: "the session was closed".to_owned(),
            }));
        }
        download(&self.client, &self.shared.cancel, key, dest, opts).await
    }
}

/// Refuses an existing destination before any request, checks the size by HEAD, then streams
/// the GET into a random hidden sibling and renames it into place only once every expected byte
/// is on disk. Each response and chunk waits at most `idle_timeout`, and cancel drops the whole
/// run; either way the partial file is deleted when its `NamedTempFile` drops.
pub(crate) async fn download(
    client: &S3Client,
    cancel: &AwsCancel,
    key: &ObjectKey,
    dest: &Path,
    opts: &DownloadOptions,
) -> Result<DownloadReport, DownloadError> {
    if !opts.force && dest.symlink_metadata().is_ok() {
        return Err(DownloadError::Exists);
    }
    let name = dest
        .file_name()
        .ok_or(DownloadError::Destination(io::ErrorKind::InvalidInput))?;
    let dir = match dest.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let place = |e: io::Error| DownloadError::Destination(e.kind());
    let remote =
        |e| DownloadError::Remote(browse_error(e, &format!("s3://{}/{}", key.bucket, key.key)));
    let run = async {
        let meta = idle(opts.idle_timeout, client.head_object(&key.bucket, &key.key))
            .await?
            .map_err(|e| remote(classify(e)))?;
        if meta.size > opts.max_size {
            return Err(DownloadError::TooLarge {
                size: meta.size,
                limit: opts.max_size,
            });
        }
        let mut body = idle(opts.idle_timeout, client.get_object(&key.bucket, &key.key))
            .await?
            .map_err(|e: Sdk<_>| remote(classify(e)))?;
        // A GET without a Content-Length falls back to the size the HEAD reported.
        let expected = body.length.unwrap_or(meta.size);
        if expected > opts.max_size {
            return Err(DownloadError::TooLarge {
                size: expected,
                limit: opts.max_size,
            });
        }
        let mut prefix = OsString::from(".");
        prefix.push(name);
        prefix.push(".");
        let tmp = tempfile::Builder::new()
            .prefix(&prefix)
            .suffix(".partial")
            .rand_bytes(8)
            .tempfile_in(dir)
            .map_err(place)?;
        let mut out = tokio::fs::File::from_std(tmp.reopen().map_err(place)?);
        let mut written = 0u64;
        while let Some(chunk) = idle(opts.idle_timeout, body.stream.next()).await? {
            let chunk = chunk.map_err(|_| DownloadError::Truncated)?;
            written += chunk.len() as u64;
            if written > expected {
                return Err(DownloadError::Truncated);
            }
            out.write_all(&chunk).await.map_err(place)?;
        }
        if written != expected {
            return Err(DownloadError::Truncated);
        }
        out.sync_all().await.map_err(place)?;
        drop(out);
        // No-clobber unless forced; a failed hard link is a destination error with no fallback,
        // since check-then-rename would reopen the race.
        let placed = if opts.force {
            tmp.persist(dest).map(drop)
        } else {
            tmp.persist_noclobber(dest).map(drop)
        };
        placed.map_err(|e| {
            let kind = e.error.kind();
            if kind == io::ErrorKind::AlreadyExists {
                DownloadError::Exists
            } else {
                DownloadError::Destination(kind)
            }
        })?;
        Ok(DownloadReport {
            bytes: written,
            path: dest.to_owned(),
        })
    };
    cancel
        .guard(run)
        .await
        .map_err(|_| DownloadError::Cancelled)?
}

async fn idle<F: Future>(limit: Duration, f: F) -> Result<F::Output, DownloadError> {
    tokio::time::timeout(limit, f)
        .await
        .map_err(|_| DownloadError::Timeout)
}
