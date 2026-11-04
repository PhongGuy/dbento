//! DuckDB files for the CLI and MCP tests, which cannot use an empty file as a database.

use std::path::Path;

/// Creates the DuckDB database at `path` by running `setup` with extension autoload off and
/// DuckDB's home beside the file, then closes it so a Dbento session can take the lock.
pub fn duckdb_file(path: &Path, setup: &str) {
    let home = path.parent().expect("a file path").join("duckdb-home");
    let cfg = duckdb::Config::default()
        .enable_autoload_extension(false)
        .and_then(|c| c.with("home_directory", home.to_str().expect("a UTF-8 path")))
        .expect("DuckDB config");
    duckdb::Connection::open_with_flags(path, cfg)
        .expect("create the DuckDB file")
        .execute_batch(setup)
        .expect("run the DuckDB setup");
}
