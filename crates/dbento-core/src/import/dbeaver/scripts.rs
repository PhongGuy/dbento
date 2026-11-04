//! Collecting a DBeaver project's `Scripts/**/*.sql`, each bound to the connection its
//! `default-datasource` names when that connection was imported. Symlinks are never followed.

use super::DBeaverError;
use super::project::io;
use crate::import::model::{ImportSet, ImportedQuery, ItemKind, SkipReason, WarningKind};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// Adds every regular `.sql` (any case) below `<root>/Scripts`, entries in name order.
/// `relative` is the path below `Scripts/`; the binding is `bindings["Scripts/<rel with />"]`
/// when it is in `imported`. Symlinks are skipped (ItemKind::Query, Symlink) and never followed.
pub(crate) fn collect(
    root: &Path,
    bindings: &BTreeMap<String, String>,
    imported: &HashSet<String>,
    set: &mut ImportSet,
) -> Result<(), DBeaverError> {
    let walk = Walk { bindings, imported };
    walk.dir(&root.join("Scripts"), Path::new(""), set)
}

struct Walk<'a> {
    bindings: &'a BTreeMap<String, String>,
    imported: &'a HashSet<String>,
}

impl Walk<'_> {
    /// Adds the scripts of `dir`, whose path below `Scripts/` is `relative`. A missing
    /// folder adds nothing; a symlinked one is reported instead of entered.
    fn dir(&self, dir: &Path, relative: &Path, set: &mut ImportSet) -> Result<(), DBeaverError> {
        match std::fs::symlink_metadata(dir) {
            Ok(m) if m.file_type().is_symlink() => {
                symlink(dir, set);
                return Ok(());
            }
            Ok(m) if m.is_dir() => {}
            Ok(_) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(io(dir, e)),
        }
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(dir).map_err(|e| io(dir, e))? {
            paths.push(entry.map_err(|e| io(dir, e))?.path());
        }
        paths.sort();
        for path in paths {
            let rel = relative.join(path.file_name().unwrap_or_default());
            let kind = std::fs::symlink_metadata(&path)
                .map_err(|e| io(&path, e))?
                .file_type();
            if kind.is_symlink() {
                symlink(&path, set);
            } else if kind.is_dir() {
                self.dir(&path, &rel, set)?;
            } else if kind.is_file() && is_sql(&path) {
                self.file(path, rel, set)?;
            }
        }
        Ok(())
    }

    /// Adds one script byte for byte, warning when it is not UTF-8.
    fn file(&self, path: PathBuf, rel: PathBuf, set: &mut ImportSet) -> Result<(), DBeaverError> {
        let content = std::fs::read(&path).map_err(|e| io(&path, e))?;
        if std::str::from_utf8(&content).is_err() {
            set.warn(path.display().to_string(), WarningKind::NotUtf8);
        }
        // Bindings are keyed with `/` whatever the platform separator.
        let parts: Vec<_> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect();
        let key = format!("Scripts/{}", parts.join("/"));
        let connection = self
            .bindings
            .get(&key)
            .filter(|id| self.imported.contains(*id))
            .cloned();
        set.add_query(ImportedQuery::new(connection, rel, path, content));
        Ok(())
    }
}

fn symlink(path: &Path, set: &mut ImportSet) {
    set.skip(
        ItemKind::Query,
        path.display().to_string(),
        SkipReason::Symlink,
    );
}

fn is_sql(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("sql"))
}
