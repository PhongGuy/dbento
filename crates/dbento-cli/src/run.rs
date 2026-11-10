//! `dbento run`: choosing the connection a saved query file runs on, and checking the file
//! is in that connection's query language.

use std::path::Path;

use dbento_core::{Engine, QueryLanguage, Workspace};

/// The connection whose `queries/<conn>/` folder holds `file`, judged by where the path the
/// user gave lies. Only the parent folder is resolved, so a symlink elsewhere cannot decide.
pub(crate) fn bound_connection(ws: &Workspace, file: &Path) -> anyhow::Result<Option<String>> {
    let parent = match file.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let parent = parent
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", file.display()))?;
    for conn in ws.connections() {
        let folder = ws.dir().join("queries").join(conn.name());
        if folder.canonicalize().is_ok_and(|f| parent.starts_with(f)) {
            return Ok(Some(conn.name().to_owned()));
        }
    }
    Ok(None)
}

/// Why `file` must not run on an `engine` connection: its extension is a known query
/// extension of another language. None when it matches or is not a query extension.
pub(crate) fn language_mismatch(file: &Path, engine: Engine) -> Option<String> {
    let ext = file.extension()?.to_str()?.to_ascii_lowercase();
    let own = engine.capabilities().language;
    let other = [
        QueryLanguage::Sql,
        QueryLanguage::MongoShell,
        QueryLanguage::SurrealQl,
        QueryLanguage::RedisCommands,
        QueryLanguage::PartiQl,
    ]
    .into_iter()
    .find(|l| l.file_extension() == ext)?;
    (other != own).then(|| {
        format!(
            "{} is a {} file (.{ext}), but this connection runs {} from .{} files",
            file.display(),
            other.name(),
            own.name(),
            own.file_extension()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_file_must_be_in_its_connections_language() {
        assert!(language_mismatch(Path::new("q.sql"), Engine::Postgres).is_none());
        assert!(language_mismatch(Path::new("q.JS"), Engine::Mongodb).is_none());
        assert!(language_mismatch(Path::new("q.txt"), Engine::Mongodb).is_none());
        assert!(language_mismatch(Path::new("q"), Engine::Mongodb).is_none());
        let e = language_mismatch(Path::new("dir/q.sql"), Engine::Mongodb).unwrap();
        assert!(
            e.contains("dir/q.sql") && e.contains("MongoDB shell") && e.contains(".js"),
            "{e}"
        );
        let e = language_mismatch(Path::new("q.js"), Engine::Sqlite).unwrap();
        assert!(e.contains("SQL") && e.contains(".sql"), "{e}");
        assert!(language_mismatch(Path::new("q.surql"), Engine::Postgres).is_some());
        assert!(language_mismatch(Path::new("q.redis"), Engine::Postgres).is_some());
        assert!(language_mismatch(Path::new("q.REDIS"), Engine::Redis).is_none());
        let e = language_mismatch(Path::new("q.sql"), Engine::Redis).unwrap();
        assert!(e.contains("Redis commands") && e.contains(".redis"), "{e}");
        let e = language_mismatch(Path::new("q.PartiQL"), Engine::Postgres).unwrap();
        assert!(e.contains("PartiQL") && e.contains(".partiql"), "{e}");
    }
}
