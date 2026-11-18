//! The rules decided by a file's name alone, and which files the content rules read.

use potluck_kind::{Code, Finding};

/// Database files, matched at the end of the basename, each optionally followed by a
/// [`JOURNAL_SUFFIXES`] entry.
const DATABASE_SUFFIXES: &[&str] = &[".db", ".sqlite", ".sqlite3", ".duckdb"];
/// The side files SQLite and DuckDB keep next to an open database.
const JOURNAL_SUFFIXES: &[&str] = &["-wal", "-shm", "-journal"];
/// DuckDB's own write-ahead log, which does not follow the journal pattern.
const DUCKDB_WAL: &str = ".duckdb.wal";
/// Private keys and keychains; `.pem` is left out because it usually holds a public certificate.
const KEY_SUFFIXES: &[&str] = &[".key", ".p12", ".pfx", ".keychain", ".keychain-db"];

/// The last segment of a `/`-separated repository path.
pub(crate) fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Refuses a database or key file by its basename, in any ASCII case at any depth.
pub(crate) fn path_rule(path: &str) -> Option<Finding> {
    let name = basename(path).to_ascii_lowercase();
    let rule = if is_database(&name) {
        "database_file"
    } else if KEY_SUFFIXES.iter().any(|s| name.ends_with(s)) {
        "key_file"
    } else {
        return None;
    };
    Some(Finding::new(Code::EnvFile, rule, path))
}

/// Whether the content rules read this file: only a `dbento.toml`, by basename.
pub(crate) fn reads(path: &str) -> bool {
    basename(path).eq_ignore_ascii_case("dbento.toml")
}

// Drop one journal suffix if present, then look for a database extension underneath it.
fn is_database(name: &str) -> bool {
    let stem = JOURNAL_SUFFIXES
        .iter()
        .find_map(|s| name.strip_suffix(s))
        .unwrap_or(name);
    name.ends_with(DUCKDB_WAL) || DATABASE_SUFFIXES.iter().any(|s| stem.ends_with(s))
}
