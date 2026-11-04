//! DataGrip's folder forest: which folder each data source sits in.

use super::map::is_invisible;
use crate::FolderPath;
use std::collections::HashMap;
use std::sync::Arc;

/// The deepest folder nesting kept; a deeper source imports without a folder.
pub(crate) const MAX_DEPTH: usize = 16;

/// The most name bytes a folder path may hold; a longer path imports without a folder.
pub(crate) const MAX_PATH_BYTES: usize = 1024;

/// The most folder lines read; a forest with more gives no folders, so a crafted file
/// cannot make the folder map large.
pub(crate) const MAX_FOLDERS: usize = 1 << 18;

/// Which file the forest text came from; only folder lines differ between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ForestForm {
    /// `db-forest-configuration`: folder lines are `id:parent:uuid:name`.
    V2,
    /// `db-tree-configuration`: folder lines are `id:parent:name`.
    Legacy,
}

/// Raw folder segments per data-source uuid that `keep` accepts; sources at the root, with a
/// broken chain, or in a forest past `MAX_FOLDERS` folders are absent.
pub(crate) fn parse_forest(
    data: &str,
    form: ForestForm,
    keep: impl Fn(&str) -> bool,
) -> HashMap<String, Arc<[String]>> {
    // Folder lines come before the first line of dashes, data-source lines after it.
    let mut folders: HashMap<u64, (u64, &str)> = HashMap::new();
    let mut sources: Vec<(u64, &str)> = Vec::new();
    let mut in_sources = false;
    for line in data.split('\n').map(str::trim).filter(|l| !l.is_empty()) {
        if line.chars().all(|c| c == '-') {
            in_sources = true;
        } else if in_sources {
            // Only sources the project has are kept, so the map is bounded by those.
            if let Some((_, parent, uuid)) = split_line(line, 3).filter(|&(_, _, u)| keep(u)) {
                sources.push((parent, uuid));
            }
        } else if let Some((id, parent, rest)) = split_line(line, form.folder_fields()) {
            let name = match form {
                ForestForm::V2 => rest.split_once(':').map(|(_, name)| name),
                ForestForm::Legacy => Some(rest),
            };
            if let Some(name) = name {
                folders.insert(id, (parent, name));
                if folders.len() > MAX_FOLDERS {
                    return HashMap::new();
                }
            }
        }
    }

    // Resolve each starting folder once and share the path, so sources in the same
    // folder hold one copy however many there are.
    let mut memo: HashMap<u64, Option<Arc<[String]>>> = HashMap::new();
    let mut out = HashMap::new();
    for (parent, uuid) in sources {
        let resolved = memo
            .entry(parent)
            .or_insert_with(|| resolve(&folders, parent))
            .clone();
        if let Some(path) = resolved {
            out.insert(uuid.to_owned(), path);
        }
    }
    out
}

/// The folder path from the root down to `folder`, or `None` at the root, on a missing
/// parent, a cycle, or a path past `MAX_DEPTH` levels or `MAX_PATH_BYTES` bytes.
fn resolve(folders: &HashMap<u64, (u64, &str)>, mut folder: u64) -> Option<Arc<[String]>> {
    let mut names = Vec::new();
    let mut bytes = 0;
    while folder != 0 {
        let &(parent, name) = folders.get(&folder)?;
        bytes += name.len();
        if names.len() == MAX_DEPTH || bytes > MAX_PATH_BYTES {
            return None;
        }
        names.push(name);
        folder = parent;
    }
    if names.is_empty() {
        return None;
    }
    Some(names.iter().rev().map(|&n| n.to_owned()).collect())
}

// Digits only: `u64::from_str` would also take a leading '+'.
fn decimal(field: &str) -> Option<u64> {
    if field.is_empty() || !field.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    field.parse().ok()
}

impl ForestForm {
    fn folder_fields(self) -> usize {
        match self {
            ForestForm::V2 => 4,
            ForestForm::Legacy => 3,
        }
    }
}

/// Splits `id:parent:rest` into at most `fields` parts, with decimal `id` and `parent`.
/// For four fields, `rest` still holds `uuid:name`.
fn split_line(line: &str, fields: usize) -> Option<(u64, u64, &str)> {
    let mut parts = line.splitn(3, ':');
    let id = decimal(parts.next()?)?;
    let parent = decimal(parts.next()?)?;
    let rest = parts.next()?;
    if fields == 4 && !rest.contains(':') {
        return None;
    }
    Some((id, parent, rest))
}

/// Splits names on '/', strips U+200B, U+FEFF and control chars, trims, drops empty
/// segments, then `FolderPath::from_str`.
pub(crate) fn clean_folder<'a>(segments: impl IntoIterator<Item = &'a str>) -> Option<FolderPath> {
    let cleaned: Vec<String> = segments
        .into_iter()
        .flat_map(|name| name.split('/'))
        .map(|seg| {
            let kept: String = seg
                .chars()
                .filter(|&c| !is_invisible(c) && !c.is_control())
                .collect();
            kept.trim().to_owned()
        })
        .filter(|seg| !seg.is_empty())
        .collect();
    cleaned.join("/").parse().ok()
}

/// A non-empty cleaned forest path wins; else `group` split on '/'; else None.
pub(crate) fn source_folder(forest: Option<&[String]>, group: Option<&str>) -> Option<FolderPath> {
    forest
        .and_then(|segs| clean_folder(segs.iter().map(String::as_str)))
        .or_else(|| group.and_then(|g| clean_folder(g.split('/'))))
}

#[cfg(test)]
mod tests;
