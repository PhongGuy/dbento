//! The measured cases, run through the core API against live stress tables.

use crate::measure::{MemReading, Sample};
use anyhow::{Context, Result, bail};
use dbento_core::{
    Engine, Limits, ResultBatch, Secret, Session, SessionOptions, Workspace, connect,
};
use std::path::{Path, PathBuf};
use std::time::Instant;
use tempfile::TempDir;

/// One measurement: an engine and a query shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Case {
    TtfrPg,
    TtfrSqlite,
    StreamPg,
    StreamSqlite,
    NarrowPg,
    NarrowSqlite,
    WidePg,
    WideSqlite,
}

impl Case {
    pub const ALL: [Case; 8] = [
        Case::TtfrPg,
        Case::TtfrSqlite,
        Case::StreamPg,
        Case::StreamSqlite,
        Case::NarrowPg,
        Case::NarrowSqlite,
        Case::WidePg,
        Case::WideSqlite,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Case::TtfrPg => "ttfr_pg",
            Case::TtfrSqlite => "ttfr_sqlite",
            Case::StreamPg => "stream_pg",
            Case::StreamSqlite => "stream_sqlite",
            Case::NarrowPg => "narrow_pg",
            Case::NarrowSqlite => "narrow_sqlite",
            Case::WidePg => "wide_pg",
            Case::WideSqlite => "wide_sqlite",
        }
    }

    pub fn parse(name: &str) -> Option<Case> {
        Case::ALL.into_iter().find(|c| c.name() == name)
    }

    pub fn engine(self) -> Engine {
        match self {
            Case::TtfrPg | Case::StreamPg | Case::NarrowPg | Case::WidePg => Engine::Postgres,
            Case::TtfrSqlite | Case::StreamSqlite | Case::NarrowSqlite | Case::WideSqlite => {
                Engine::Sqlite
            }
        }
    }

    pub fn sql(self) -> &'static str {
        match self {
            Case::TtfrPg | Case::TtfrSqlite | Case::StreamPg | Case::StreamSqlite => {
                "SELECT * FROM big"
            }
            Case::NarrowPg | Case::NarrowSqlite => "SELECT * FROM big WHERE id <= 1000000",
            Case::WidePg | Case::WideSqlite => "SELECT * FROM wide",
        }
    }
}

/// The parts of a `postgres://user:password@host:port/database` URL.
#[derive(Clone, PartialEq)]
pub struct PgUrl {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
}

// Redacting by hand: the derived Debug would print the password.
impl std::fmt::Debug for PgUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgUrl")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &"<redacted>")
            .field("database", &self.database)
            .finish()
    }
}

impl PgUrl {
    /// Splits the URL; user, password, port and database are all required.
    pub fn parse(url: &str) -> Result<PgUrl> {
        // The URL carries the password, so an error never shows it.
        let bad = || "not a postgres://user:password@host:port/database URL (value not shown)";
        let rest = url.strip_prefix("postgres://").with_context(bad)?;
        let (auth, rest) = rest.split_once('@').with_context(bad)?;
        let (user, password) = auth.split_once(':').with_context(bad)?;
        let (hostport, database) = rest.split_once('/').with_context(bad)?;
        let (host, port) = hostport.rsplit_once(':').with_context(bad)?;
        let port: u16 = port.parse().with_context(bad)?;
        if [user, host, database].iter().any(|s| s.is_empty()) {
            bail!(bad());
        }
        Ok(PgUrl {
            host: host.into(),
            port,
            user: user.into(),
            password: password.into(),
            database: database.into(),
        })
    }
}

/// The databases a bench run measures, as a throwaway workspace plus the Postgres password.
pub struct Targets {
    dir: TempDir,
    ws: Workspace,
    pg: Option<PgUrl>,
    sqlite: Option<PathBuf>,
}

impl Targets {
    /// Writes a `dbento.toml` naming whichever targets are given and opens it.
    pub fn new(pg: Option<PgUrl>, sqlite: Option<PathBuf>) -> Result<Targets> {
        let dir = tempfile::tempdir()?;
        let mut toml = String::from("dbento = 1\n");
        let plain = |s: &str| !s.contains(['"', '\\', '\n']);
        if let Some(p) = &pg {
            if ![&p.host, &p.user, &p.database]
                .into_iter()
                .all(|s| plain(s))
            {
                bail!("the Postgres URL contains a quote or backslash");
            }
            toml += &format!(
                "[connections.pg]\nengine = \"postgres\"\nhost = \"{}\"\nport = {}\ndatabase = \"{}\"\nuser = \"{}\"\nsecret = {{ env = \"DBENTO_BENCH_PG_PASSWORD\" }}\n",
                p.host, p.port, p.database, p.user
            );
        }
        if let Some(path) = &sqlite {
            let path = std::path::absolute(path)?;
            let s = path.to_str().context("the SQLite path is not UTF-8")?;
            if !plain(s) {
                bail!("the SQLite path contains a quote or backslash: {s:?}");
            }
            toml += &format!("[connections.lite]\nengine = \"sqlite\"\npath = \"{s}\"\n");
        }
        std::fs::write(dir.path().join("dbento.toml"), toml)?;
        let ws = Workspace::open(dir.path())?;
        Ok(Targets {
            dir,
            ws,
            pg,
            sqlite,
        })
    }

    /// The workspace folder holding the generated `dbento.toml`.
    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// Opens a read-write session on the target for `engine`, naming the flag when it is unset.
    pub async fn session(&self, engine: Engine) -> Result<Session> {
        let (name, secret) = match engine {
            Engine::Postgres => {
                let pg = self
                    .pg
                    .as_ref()
                    .context("no Postgres target: pass --pg-url")?;
                ("pg", Some(Secret::new(&pg.password)))
            }
            Engine::Sqlite
            | Engine::Mysql
            | Engine::Mssql
            | Engine::Duckdb
            | Engine::Mongodb
            | Engine::Surrealdb
            | Engine::Redis
            | _ => {
                self.sqlite
                    .as_ref()
                    .context("no SQLite target: pass --sqlite")?;
                ("lite", None)
            }
        };
        let conn = self.ws.connection(name).context("connection missing")?;
        Ok(connect(&self.ws, conn, secret, SessionOptions::default()).await?)
    }
}

/// Runs `case` once in this process and returns its sample.
pub async fn measure(case: Case, targets: &Targets) -> Result<Sample> {
    let mut session = targets.session(case.engine()).await?;
    // The first statement pays connection warm-up, which no case should count.
    drain(&mut session, "SELECT 1").await?;
    let mut script = session.script(case.sql());
    // The baseline is read first so the timer starts right before `next`, which prepares on Postgres.
    let before = MemReading::now()?;
    let start = Instant::now();
    let pending = script
        .next()
        .await
        .context("the query produced no statement")??;
    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.0;
    let (value, rows) = match case {
        Case::TtfrPg | Case::TtfrSqlite => {
            let mut stream = pending.execute(Limits::default()).await?;
            let rows = loop {
                match stream.next_batch().await? {
                    Some(b) if !b.is_empty() => break b.len() as u64,
                    Some(_) => {}
                    None => bail!("the stream ended before a row"),
                }
            };
            (ms(start), rows)
        }
        Case::StreamPg | Case::StreamSqlite => {
            let mut stream = pending.execute(Limits::default()).await?;
            while stream.next_batch().await?.is_some() {}
            let elapsed = ms(start);
            (elapsed, stream.outcome().context("no outcome")?.rows)
        }
        Case::NarrowPg | Case::NarrowSqlite | Case::WidePg | Case::WideSqlite => {
            let mut stream = pending.execute(Limits::default()).await?;
            let mut kept: Vec<ResultBatch> = Vec::new();
            while let Some(b) = stream.next_batch().await? {
                kept.push(b);
            }
            let grown = MemReading::now()?.grown_kib_since(&before) as f64 / 1024.0;
            let rows: u64 = kept.iter().map(|b| b.len() as u64).sum();
            std::hint::black_box(&kept);
            (per_million(grown, rows), rows)
        }
    };
    Ok(Sample {
        case: case.name().into(),
        value,
        rows,
    })
}

/// Scales `mib` of growth over `rows` rows to MiB per 1,000,000 rows (zero rows count as one).
fn per_million(mib: f64, rows: u64) -> f64 {
    mib * 1_000_000.0 / rows.max(1) as f64
}

/// Runs one statement to its end, dropping the rows.
async fn drain(session: &mut Session, sql: &str) -> Result<()> {
    let mut script = session.script(sql);
    while let Some(p) = script.next().await {
        let mut stream = p?.execute(Limits::default()).await?;
        while stream.next_batch().await?.is_some() {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rejected_url_error_never_echoes_the_password() {
        for url in [
            "postgres://u:hunter2secret@h/db",
            "postgres://u:hunter2secret",
            "postgres://u:hunter2secret@h:x/db",
        ] {
            let err = format!("{:#}", PgUrl::parse(url).unwrap_err());
            assert!(!err.contains("hunter2secret"), "{err}");
        }
    }

    #[test]
    fn growth_scales_to_a_million_rows() {
        assert_eq!(per_million(10.0, 500_000), 20.0);
        assert_eq!(per_million(3.0, 3_000_000), 1.0);
        assert_eq!(per_million(5.0, 0), 5_000_000.0);
    }

    #[test]
    fn every_case_has_its_engine() {
        for c in Case::ALL {
            let want = if c.name().ends_with("_pg") {
                Engine::Postgres
            } else {
                Engine::Sqlite
            };
            assert_eq!(c.engine(), want, "{}", c.name());
        }
    }
}
