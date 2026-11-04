//! Collects project query files and JetBrains consoles into the import set, binding each to
//! the connection it belonged to. Symlinks are reported and never followed.

use super::DataGripError;
use super::xml::Project;
use crate::import::model::{ImportSet, ImportedQuery, ItemKind, SkipReason, WarningKind};
use crate::import::plan::normalize;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

const PROJECT_DIR: &str = "file://$PROJECT_DIR$/";

/// Adds the project's query files (bound through its mappings) and then the consoles of
/// every source with a usable uuid. `imported` and `skipped` are source uuids.
pub(crate) fn collect(
    project: &Project,
    config_dirs: &[PathBuf],
    imported: &HashSet<String>,
    skipped: &HashSet<String>,
    set: &mut ImportSet,
) -> Result<(), DataGripError> {
    let bound = bindings(project, imported, set);
    let mut roots = vec![project.root.join("queries")];
    if project.idea.join("queries") != roots[0] {
        roots.push(project.idea.join("queries"));
    }
    for root in &roots {
        walk(root, Path::new(""), &bound, set)?;
    }
    // Source order, not set order, so a run reads the same however the sets hash.
    let mut seen = HashSet::new();
    for src in &project.sources {
        let uuid = src.uuid.as_str();
        let usable = imported.contains(uuid) || skipped.contains(uuid);
        if usable && seen.insert(uuid) {
            consoles(uuid, imported.contains(uuid), config_dirs, set)?;
        }
    }
    Ok(())
}

/// Resolved mapped file to source uuid, for `$PROJECT_DIR$` mappings of imported sources.
/// A `..` in a mapping that would otherwise bind is reported once and never bound.
fn bindings(
    project: &Project,
    imported: &HashSet<String>,
    set: &mut ImportSet,
) -> HashMap<PathBuf, String> {
    let mut bound = HashMap::new();
    let mut reported = HashSet::new();
    for m in &project.mappings {
        let Some(rest) = m.url.strip_prefix(PROJECT_DIR) else {
            continue;
        };
        if !imported.contains(&m.uuid) {
            continue;
        }
        let resolved = project.root.join(rest);
        if Path::new(rest)
            .components()
            .any(|c| c == Component::ParentDir)
        {
            if reported.insert(resolved.clone()) {
                set.skip(
                    ItemKind::Query,
                    resolved.display().to_string(),
                    SkipReason::OutsideRoot,
                );
            }
        } else {
            bound.insert(normalize(&resolved), m.uuid.clone());
        }
    }
    bound
}

/// Directory entries by name, with a symlink reported and left out.
fn entries(
    dir: &Path,
    set: &mut ImportSet,
) -> Result<Vec<(PathBuf, std::fs::FileType)>, DataGripError> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| io(dir, &e))? {
        names.push(entry.map_err(|e| io(dir, &e))?.path());
    }
    names.sort();
    let mut out = Vec::new();
    for path in names {
        let kind = std::fs::symlink_metadata(&path)
            .map_err(|e| io(&path, &e))?
            .file_type();
        if kind.is_symlink() {
            set.skip(
                ItemKind::Query,
                path.display().to_string(),
                SkipReason::Symlink,
            );
        } else {
            out.push((path, kind));
        }
    }
    Ok(out)
}

/// Whether `dir` exists as a real folder; a symlink is reported and a missing one is not.
fn real_dir(dir: &Path, set: &mut ImportSet) -> Result<bool, DataGripError> {
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_symlink() => {
            set.skip(
                ItemKind::Query,
                dir.display().to_string(),
                SkipReason::Symlink,
            );
            Ok(false)
        }
        Ok(m) => Ok(m.is_dir()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io(dir, &e)),
    }
}

fn is_sql(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("sql"))
}

/// Reads a query file, warning when it is not UTF-8 (it is still copied byte for byte).
fn read_query(path: &Path, set: &mut ImportSet) -> Result<Vec<u8>, DataGripError> {
    let bytes = std::fs::read(path).map_err(|e| io(path, &e))?;
    if std::str::from_utf8(&bytes).is_err() {
        set.warn(path.display().to_string(), WarningKind::NotUtf8);
    }
    Ok(bytes)
}

/// Adds the `.sql` files below `dir`; `relative` is the path under the query root.
fn walk(
    dir: &Path,
    relative: &Path,
    bound: &HashMap<PathBuf, String>,
    set: &mut ImportSet,
) -> Result<(), DataGripError> {
    if !real_dir(dir, set)? {
        return Ok(());
    }
    for (path, kind) in entries(dir, set)? {
        let rel = relative.join(path.file_name().unwrap_or_default());
        if kind.is_dir() {
            walk(&path, &rel, bound, set)?;
        } else if kind.is_file() && is_sql(&path) {
            let content = read_query(&path, set)?;
            let connection = bound.get(&normalize(&path)).cloned();
            set.add_query(ImportedQuery::new(connection, rel, path, content));
        }
    }
    Ok(())
}

/// Adds the consoles of one source from each config folder. The first copy of a file name
/// wins; a different file under a taken name is renamed after its config folder.
fn consoles(
    uuid: &str,
    imported: bool,
    config_dirs: &[PathBuf],
    set: &mut ImportSet,
) -> Result<(), DataGripError> {
    let mut taken: HashMap<String, Vec<u8>> = HashMap::new();
    for config in config_dirs {
        let dir = config.join("consoles/db").join(uuid);
        if !real_dir(&dir, set)? {
            continue;
        }
        for (path, kind) in entries(&dir, set)? {
            if !kind.is_file() || !is_sql(&path) {
                continue;
            }
            if !imported {
                set.skip(
                    ItemKind::Query,
                    path.display().to_string(),
                    SkipReason::SourceSkipped,
                );
                continue;
            }
            let content = read_query(&path, set)?;
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let Some(name) = free_name(&name, config, &content, &taken) else {
                continue;
            };
            taken.insert(name.clone(), content.clone());
            set.add_query(ImportedQuery::new(
                Some(uuid.to_owned()),
                PathBuf::from(name),
                path,
                content,
            ));
        }
    }
    Ok(())
}

/// The name to file `content` under, or `None` when an identical file already holds it.
fn free_name(
    name: &str,
    config: &Path,
    content: &[u8],
    taken: &HashMap<String, Vec<u8>>,
) -> Option<String> {
    let mut candidate = name.to_owned();
    let stem = name
        .strip_suffix(".sql")
        .or_else(|| name.strip_suffix(".SQL"))
        .unwrap_or(name);
    let product = config.file_name().unwrap_or_default().to_string_lossy();
    let mut n = 1;
    while let Some(have) = taken.get(&candidate) {
        if have == content {
            return None;
        }
        n += 1;
        candidate = if n == 2 {
            format!("{stem}-{product}.sql")
        } else {
            format!("{stem}-{product}-{}.sql", n - 1)
        };
    }
    Some(candidate)
}

fn io(path: &Path, e: &std::io::Error) -> DataGripError {
    DataGripError::Io {
        path: path.to_path_buf(),
        message: e.kind().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::free_name;
    use std::collections::HashMap;
    use std::path::Path;

    #[test]
    fn a_fourth_console_under_one_name_takes_the_next_numbered_product_name() {
        let config = Path::new("/jb/DataGrip2024.2");
        let mut taken: HashMap<String, Vec<u8>> = HashMap::new();
        let mut names = Vec::new();
        for body in ["a", "b", "c", "d"] {
            let name = free_name("console.sql", config, body.as_bytes(), &taken).unwrap();
            taken.insert(name.clone(), body.as_bytes().to_vec());
            names.push(name);
        }
        assert_eq!(
            names,
            [
                "console.sql",
                "console-DataGrip2024.2.sql",
                "console-DataGrip2024.2-2.sql",
                "console-DataGrip2024.2-3.sql"
            ]
        );
        assert_eq!(free_name("console.sql", config, b"c", &taken), None);
    }
}
