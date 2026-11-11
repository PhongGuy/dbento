//! `dbento run`: choosing the connection a saved query file runs on, and checking the file
//! is in that connection's query language.

use std::path::Path;

use dbento_core::{Engine, QueryLanguage, Workspace};

/// The connection `file` binds to by core's saved-query rule, judged by where the path the
/// user gave lies. Only the parent folder is resolved, so a symlink elsewhere cannot decide.
pub(crate) fn bound_connection(ws: &Workspace, file: &Path) -> anyhow::Result<Option<String>> {
    let parent = match file.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let parent = parent
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", file.display()))?;
    // Find the connection folder by its real path, so a linked folder still binds, then hand
    // the path below it to core's rule as if it lay under `queries/<conn>/`.
    let Some(name) = file.file_name() else {
        return Ok(None);
    };
    for conn in ws.connections() {
        let folder = ws.dir().join("queries").join(conn.name());
        let Ok(folder) = folder.canonicalize() else {
            continue;
        };
        if let Ok(below) = parent.strip_prefix(&folder) {
            let rel = Path::new("queries")
                .join(conn.name())
                .join(below)
                .join(name);
            return Ok(ws.bound_connection(&rel));
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

    const PARENT: &str = "dbento = 1\n[connections.stayzy]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\ndatabases = [\"booking\"]\n[connections.plain]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\n";

    fn workspace(toml: &str) -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
        let ws = Workspace::open(dir.path()).unwrap();
        (dir, ws)
    }

    #[test]
    fn a_file_binds_by_the_shared_rule_from_its_real_folder() {
        let (dir, ws) = workspace(PARENT);
        for (rel, want) in [
            ("queries/stayzy/q.sql", Some("stayzy")),
            ("queries/stayzy/booking/q.sql", Some("stayzy/booking")),
            ("queries/stayzy/reports/q.sql", None),
            ("queries/plain/sub/q.sql", Some("plain")),
            ("loose.sql", None),
        ] {
            let path = dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "select 1").unwrap();
            assert_eq!(
                bound_connection(&ws, &path).unwrap().as_deref(),
                want,
                "{rel}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_connection_folder_still_binds() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(outside.path().join("booking")).unwrap();
        std::fs::write(outside.path().join("booking/q.sql"), "select 1").unwrap();
        let (dir, ws) = workspace(PARENT);
        std::fs::create_dir(dir.path().join("queries")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("queries/stayzy")).unwrap();
        let file = dir.path().join("queries/stayzy/booking/q.sql");
        assert_eq!(
            bound_connection(&ws, &file).unwrap().as_deref(),
            Some("stayzy/booking")
        );
    }
}
