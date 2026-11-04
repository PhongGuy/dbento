//! The generated stress tables `big` and `wide`, as SQL for both engines.

use anyhow::{Context, Result};
use std::path::Path;

/// Row count of `big`, the table the stream cases read.
pub const BIG_ROWS: u64 = 10_000_000;
/// Row count of `wide`, the 20-column table the memory cases read.
pub const WIDE_ROWS: u64 = 1_000_000;

/// The Postgres statements in run order, with no trailing semicolon.
/// Run them one by one: VACUUM refuses to run inside a transaction.
pub fn pg_stress_statements(big: u64, wide: u64) -> Vec<String> {
    [
        r#"CREATE TABLE big (id int8 NOT NULL, h text NOT NULL, ts timestamptz NOT NULL)"#,
        r#"INSERT INTO big SELECT g, md5(g::text), timestamptz '2026-01-01 00:00:00+00' - g * interval '1 second' FROM generate_series(1::int8, {big}) g"#,
        r#"CREATE TABLE wide (id int8 NOT NULL, t1 text, t2 text, t3 text, t4 text, t5 text, t6 text, n1 int8, n2 int8, n3 int8, n4 int8, n5 int8, ts1 timestamptz, ts2 timestamptz, ts3 timestamptz, ts4 timestamptz, flag bool, amount numeric(12,3), u uuid, doc jsonb)"#,
        r#"INSERT INTO wide SELECT g, md5(g::text), md5((g + 1)::text), md5((g + 2)::text), md5((g + 3)::text), md5((g + 4)::text), md5((g + 5)::text), g * 3, g * 5, g * 7, g * 11, g * 13, timestamptz '2026-01-01 00:00:00+00' - g * interval '1 second', timestamptz '2026-01-01 00:00:00+00' - g * interval '1 minute', timestamptz '2026-01-01 00:00:00+00' + g * interval '1 second', timestamptz '2026-01-01 00:00:00+00' + g * interval '1 minute', g % 2 = 0, (g % 1000000) / 1000.0, md5(g::text)::uuid, jsonb_build_object('id', g, 'tag', md5(g::text)) FROM generate_series(1::int8, {wide}) g"#,
        r#"VACUUM ANALYZE big"#,
        r#"VACUUM ANALYZE wide"#,
    ]
    .iter()
    .map(|s| s.replace("{big}", &big.to_string()).replace("{wide}", &wide.to_string()))
    .collect()
}

/// The SQLite script that creates and fills both tables.
pub fn sqlite_stress_sql(big: u64, wide: u64) -> String {
    r#"PRAGMA journal_mode = OFF;
PRAGMA synchronous = OFF;
CREATE TABLE big (id INTEGER NOT NULL, h TEXT NOT NULL, ts TEXT NOT NULL);
INSERT INTO big WITH RECURSIVE c(g) AS (SELECT 1 UNION ALL SELECT g + 1 FROM c WHERE g < {big})
SELECT g, lower(hex(randomblob(16))), datetime('2026-01-01 00:00:00', '-' || g || ' seconds') FROM c;
CREATE TABLE wide (id INTEGER NOT NULL, t1 TEXT, t2 TEXT, t3 TEXT, t4 TEXT, t5 TEXT, t6 TEXT, n1 INTEGER, n2 INTEGER, n3 INTEGER, n4 INTEGER, n5 INTEGER, ts1 TEXT, ts2 TEXT, ts3 TEXT, ts4 TEXT, flag INTEGER, amount REAL, u TEXT, doc TEXT);
INSERT INTO wide WITH RECURSIVE c(g) AS (SELECT 1 UNION ALL SELECT g + 1 FROM c WHERE g < {wide})
SELECT g, lower(hex(randomblob(16))), lower(hex(randomblob(16))), lower(hex(randomblob(16))), lower(hex(randomblob(16))), lower(hex(randomblob(16))), lower(hex(randomblob(16))), g * 3, g * 5, g * 7, g * 11, g * 13, datetime('2026-01-01 00:00:00', '-' || g || ' seconds'), datetime('2026-01-01 00:00:00', '-' || g || ' minutes'), datetime('2026-01-01 00:00:00', '+' || g || ' seconds'), datetime('2026-01-01 00:00:00', '+' || g || ' minutes'), g % 2 = 0, (g % 1000000) / 1000.0, lower(hex(randomblob(16))), json_object('id', g, 'tag', lower(hex(randomblob(16)))) FROM c;"#
        .replace("{big}", &big.to_string())
        .replace("{wide}", &wide.to_string())
}

/// Builds the SQLite stress database at `path`, replacing any file already there.
pub fn build_sqlite_stress(path: &Path, big: u64, wide: u64) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    rusqlite::Connection::open(path)?
        .execute_batch(&sqlite_stress_sql(big, wide))
        .context("building the SQLite stress tables")
}

/// True when both tables exist with exactly these row counts. Opens read-only so a
/// missing file is never created; any error means not ready.
pub fn sqlite_stress_ready(path: &Path, big: u64, wide: u64) -> bool {
    let count = |c: &rusqlite::Connection, table: &str| -> Option<i64> {
        c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .ok()
    };
    let Ok(c) =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return false;
    };
    count(&c, "big") == i64::try_from(big).ok() && count(&c, "wide") == i64::try_from(wide).ok()
}
