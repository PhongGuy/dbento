//! `dbento download`: one S3 object to a local file, refusing to overwrite or to exceed a size.

use std::path::{Path, PathBuf};

use dbento_core::{
    ConnectionConfig, DownloadError, DownloadOptions, Engine, ObjectKey, SessionOptions, Workspace,
    display_key,
};

use crate::Exit;
use crate::exec::{Interrupted, open};
use crate::interrupt::Interrupt;

/// Arguments of `dbento download`.
#[derive(clap::Args)]
pub(crate) struct DownloadArgs {
    /// The S3 connection name from dbento.toml.
    conn: String,
    /// The object, as `s3://bucket/key`.
    url: String,
    /// The file to write.
    #[arg(short, long, value_name = "PATH")]
    output: PathBuf,
    /// Replace the file when it already exists.
    #[arg(long)]
    force: bool,
    /// Refuse a larger object: bytes, or a KiB, MiB or GiB suffix.
    #[arg(long, value_name = "SIZE", default_value = "1GiB", value_parser = parse_size)]
    max_size: u64,
}

impl DownloadArgs {
    /// The connection name.
    pub(crate) fn conn(&self) -> &str {
        &self.conn
    }
}

/// Bytes, or a binary `KiB`/`MiB`/`GiB` suffix, case-insensitive, with an optional space.
pub(crate) fn parse_size(text: &str) -> Result<u64, String> {
    let bad = || format!("'{text}' is not a size; give bytes or a KiB, MiB or GiB suffix");
    let digits = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(digits);
    let shift = match unit.trim_start_matches(' ').to_ascii_lowercase().as_str() {
        "" => 0,
        "kib" => 10,
        "mib" => 20,
        "gib" => 30,
        _ => return Err(bad()),
    };
    let n: u64 = number.parse().map_err(|_| bad())?;
    n.checked_mul(1 << shift).ok_or_else(bad)
}

/// Downloads `args.url` from `conn`. A bad URL or a non-S3 connection is refused before any
/// connect (exit 3); an existing file or an object over `--max-size` is a refusal (exit 2).
pub(crate) async fn download(
    ws: &Workspace,
    conn: &ConnectionConfig,
    args: &DownloadArgs,
) -> anyhow::Result<Exit> {
    let Some(key) = ObjectKey::parse(&args.url).filter(|k| !k.key.is_empty()) else {
        anyhow::bail!("'{}' is not an s3://bucket/key URL", display_key(&args.url));
    };
    if conn.engine() != Engine::S3 {
        anyhow::bail!("'{}' is not an S3 connection", conn.name());
    }
    let shown = format!(
        "s3://{}/{}",
        display_key(&key.bucket),
        display_key(&key.key)
    );
    let interrupt =
        Interrupt::install().map_err(|e| anyhow::Error::new(e).context("cannot handle Ctrl-C"))?;
    let mut session = match open(ws, conn, SessionOptions::default(), &interrupt).await {
        Ok(session) => session,
        Err(e) if e.is::<Interrupted>() => return Ok(Exit::Failed),
        Err(e) => return Err(e),
    };
    interrupt.arm(session.cancel_handle());
    // A press while the session opened has no download to cancel yet, so stop here.
    if interrupt.stopped() {
        let _ = session.close().await;
        eprintln!("interrupted");
        return Ok(Exit::Failed);
    }
    let options = DownloadOptions::default()
        .with_max_size(args.max_size)
        .with_force(args.force);
    let result = session.download_object(&key, &args.output, options).await;
    // The download already settled; a failure closing a read-only session changes nothing.
    let _ = session.close().await;
    match result {
        Ok(report) => {
            eprintln!(
                "downloaded {shown} ({} bytes) to {}",
                report.bytes,
                report.path.display()
            );
            Ok(Exit::Ok)
        }
        Err(e) => {
            let (message, exit) = failure(&e, &shown, &args.output);
            eprintln!("{message}");
            Ok(exit)
        }
    }
}

/// The stderr line and exit code for a download that did not land.
fn failure(e: &DownloadError, shown: &str, dest: &Path) -> (String, Exit) {
    let dest = dest.display();
    match e {
        DownloadError::Exists => (
            format!("error: {dest} already exists; pass --force to replace it"),
            Exit::Refused,
        ),
        DownloadError::TooLarge { size, limit } => (
            format!(
                "error: {shown} is {size} bytes, over the {limit}-byte limit; \
                 raise --max-size to download it"
            ),
            Exit::Refused,
        ),
        DownloadError::Cancelled => ("interrupted".to_owned(), Exit::Failed),
        DownloadError::Destination(kind) => {
            (format!("error: cannot write {dest}: {kind}"), Exit::Failed)
        }
        // The store's words and a not-found key can carry raw key bytes, so they are escaped.
        other => (
            format!("error: {shown}: {}", display_key(&other.to_string())),
            Exit::Failed,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_size;
    #[test]
    fn sizes_take_bytes_or_binary_suffixes() {
        assert_eq!(parse_size("1024"), Ok(1024));
        assert_eq!(parse_size("1GiB"), Ok(1 << 30));
        assert_eq!(parse_size("3 mib"), Ok(3 << 20));
        assert_eq!(parse_size("8KiB"), Ok(8 << 10));
        for bad in ["", "1.5GiB", "-1", "1GB", "GiB"] {
            assert!(parse_size(bad).is_err(), "{bad}");
        }
    }
}
