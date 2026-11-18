//! The rules that read the contents of `dbento.toml`.

use std::path::{Path, PathBuf};

use dbento_workspace::{OpenError, Workspace};
use potluck_kind::{Blob, Code, Finding};

use crate::detail::{segment, with_detail};

/// The format version a `dbento.toml` declares: its root `dbento` integer as a decimal string.
///
/// Anything toml's parser rejects (non-UTF-8, a syntax error, a duplicate key) or a `dbento` that
/// is missing or not an integer gives `None`. BOM, CRLF and comments are left to the parser.
pub(crate) fn detect_format(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let table = text.parse::<toml::Table>().ok()?;
    match table.get("dbento")? {
        toml::Value::Integer(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Refuses a `dbento.toml` Dbento would not open, and hands back the workspace when it opens.
///
/// A deletion is never refused; a blob too large to read fails closed. Details are key paths
/// built from names, never from the error's text, so no value can leak into a finding.
pub(crate) fn check_content(
    path: &str,
    new: Option<Blob<'_>>,
) -> (Vec<Finding>, Option<Workspace>) {
    let bytes = match new {
        None => return (Vec::new(), None),
        Some(Blob::Bytes(bytes)) => bytes,
        // Too large to read, or a side this build cannot read: fail closed.
        Some(_) => {
            return (
                vec![Finding::new(Code::SecretValue, "too_large", path)],
                None,
            );
        }
    };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return (vec![Finding::new(Code::SecretValue, "parse", path)], None);
    };
    match open(text) {
        Ok(workspace) => (Vec::new(), Some(workspace)),
        Err(e) => (vec![finding(path, &e)], None),
    }
}

/// Opens `text` as a root `dbento.toml`, the one way both sides of a change are opened.
pub(crate) fn open(text: &str) -> Result<Workspace, OpenError> {
    Workspace::from_text(
        PathBuf::from("/workspace"),
        Path::new("/workspace/dbento.toml"),
        text,
    )
}

// One rule per error code, its detail destructured from the variant's own fields.
fn finding(path: &str, e: &OpenError) -> Finding {
    let secret = |rule| Finding::new(Code::SecretValue, rule, path);
    match e {
        OpenError::Password { key } => with_detail(secret("password"), key.clone()),
        OpenError::Secret {
            connection, key, ..
        } => with_detail(
            secret("secret"),
            format!("connections.{}.{key}", segment(connection)),
        ),
        OpenError::Invalid { connection, .. } => with_detail(
            secret("invalid"),
            format!("connections.{}", segment(connection)),
        ),
        OpenError::InvalidName { name } => with_detail(
            secret("invalid_name"),
            format!("connections.{}", segment(name)),
        ),
        OpenError::InvalidFolder { folder, .. } => with_detail(
            secret("invalid_folder"),
            format!("folders.{}", segment(folder)),
        ),
        OpenError::UnsupportedVersion { .. } => {
            Finding::new(Code::FormatUnsupported, "unsupported_version", path)
        }
        // A parse error, or a variant added later: refused without a detail.
        _ => secret("parse"),
    }
}
