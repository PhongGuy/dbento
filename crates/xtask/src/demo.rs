//! The demo database: `make demo` builds `shop.sqlite` from the committed `seed.sql`.

use std::path::Path;

use anyhow::{Context, Result};

/// Builds the SQLite database `out` from the SQL script `seed`, replacing any file at `out`.
pub fn build_db(seed: &Path, out: &Path) -> Result<()> {
    let sql =
        std::fs::read_to_string(seed).with_context(|| format!("reading {}", seed.display()))?;
    // Build beside `out` and rename into place, so a failed seed never leaves half a database.
    let partial = out.with_extension("sqlite.partial");
    let _ = std::fs::remove_file(&partial);
    let built = rusqlite::Connection::open(&partial).and_then(|db| {
        db.execute_batch(&sql)?;
        db.close().map_err(|(_, e)| e)
    });
    if let Err(e) = built {
        let _ = std::fs::remove_file(&partial);
        return Err(e).with_context(|| format!("running {}", seed.display()));
    }
    std::fs::rename(&partial, out)?;
    Ok(())
}
