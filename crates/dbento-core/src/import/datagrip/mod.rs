//! Reading a DataGrip project into the importer-neutral model.

mod forest;
mod map;
mod queries;
mod xml;

pub(crate) use forest::clean_folder;
pub(crate) use map::{assign_name, base_name};

use crate::import::model::{ImportSet, ImportedConnection, ItemKind, WarningKind};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// What `read_datagrip` needs from the machine it runs on. Nothing is read from a real
/// JetBrains folder unless `config_dirs` names it.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct DataGripOptions {
    config_dirs: Vec<PathBuf>,
    home: Option<PathBuf>,
    default_user: Option<String>,
}

impl DataGripOptions {
    /// No config folders, no home folder, no default user.
    pub fn new() -> Self {
        Self::default()
    }

    /// JetBrains product folders to read consoles from, in scan order.
    pub fn config_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.config_dirs = dirs;
        self
    }

    /// The home folder that `$USER_HOME$` stands for.
    pub fn home(mut self, home: Option<PathBuf>) -> Self {
        self.home = home;
        self
    }

    /// The user for a Postgres or MySQL source that names none.
    pub fn default_user(mut self, user: Option<String>) -> Self {
        self.default_user = user;
        self
    }
}

/// Reads a DataGrip project (its folder or its `.idea` folder) into an import set: one
/// connection or skip per data source, then the project's query files and consoles.
pub fn read_datagrip(folder: &Path, opts: &DataGripOptions) -> Result<ImportSet, DataGripError> {
    // `$PROJECT_DIR$` must expand to an absolute root, or a relative folder is joined twice.
    // Absolute, not canonical, so a symlinked project keeps its own paths; an empty folder
    // stays as given and is refused as not a project.
    let folder = std::path::absolute(folder).unwrap_or_else(|_| folder.to_path_buf());
    let project = xml::read_project(&folder)?;
    let ctx = map::MapContext {
        root: &project.root,
        home: opts.home.as_deref(),
        default_user: opts.default_user.as_deref(),
    };
    // A blank or repeated uuid cannot say which source a local setting, mapping or console
    // belongs to, so such a source stays unbound instead of borrowing another's. A uuid
    // names a console folder, so anything but one plain path component is refused too.
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for src in &project.sources {
        *counts.entry(src.uuid.as_str()).or_default() += 1;
    }
    let usable = |uuid: &str| {
        let mut parts = Path::new(uuid).components();
        let plain = matches!(parts.next(), Some(std::path::Component::Normal(_)))
            && parts.next().is_none()
            && !uuid.contains(['/', '\\']);
        plain && !uuid.trim().is_empty() && counts.get(uuid) == Some(&1)
    };

    let mut set = ImportSet::default();
    let (mut used, mut imported, mut skipped) = (HashSet::new(), HashSet::new(), HashSet::new());
    for src in &project.sources {
        let ok_uuid = usable(&src.uuid);
        let local = if ok_uuid {
            project.local.get(&src.uuid)
        } else {
            None
        };
        match map::map_source(src, local, &ctx) {
            Ok(mapped) => {
                let forest_path = project.forest.get(&src.uuid).map(|p| &p[..]);
                let folder = forest::source_folder(forest_path, src.group.as_deref());
                let name = map::assign_name(&map::base_name(&src.name), &mut used);
                let mut warnings = mapped.warnings;
                let raw_color = project.colors.get(&src.uuid).filter(|_| ok_uuid);
                let color = match raw_color.map(|v| map::color(v)) {
                    Some(Ok(tag)) => tag,
                    Some(Err(w)) => {
                        warnings.push(w);
                        None
                    }
                    None => None,
                };
                set.add_connection(ImportedConnection::new(
                    src.uuid.clone(),
                    src.name.clone(),
                    name,
                    mapped.draft.with_folder(folder).with_color(color),
                ));
                for w in warnings {
                    set.warn(src.name.clone(), w);
                }
                if ok_uuid {
                    imported.insert(src.uuid.clone());
                }
            }
            Err(reason) => {
                set.skip(ItemKind::DataSource, src.name.clone(), reason);
                if ok_uuid {
                    skipped.insert(src.uuid.clone());
                }
            }
        }
    }
    // Object (table and schema) colours have no place in `dbento.toml`.
    if project.has_object_colors {
        set.warn("project".to_owned(), WarningKind::ColorsNotImported);
    }
    queries::collect(&project, &opts.config_dirs, &imported, &skipped, &mut set)?;
    Ok(set)
}

/// The product folders under a JetBrains config root (`~/.config/JetBrains`) that hold
/// consoles, newest first. Versions compare numerically, ties by name; an unreadable root
/// gives an empty list.
pub fn jetbrains_config_dirs(config_root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(config_root) else {
        return Vec::new();
    };
    let mut found: Vec<((u32, u32), String, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let version = product_version(&name)?;
            let path = e.path();
            path.join("consoles/db")
                .is_dir()
                .then_some((version, name, path))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    found.into_iter().map(|(_, _, path)| path).collect()
}

/// `(year, minor)` of a folder named like `DataGrip2024.10`: ASCII letters, four digits, a
/// dot, then digits.
fn product_version(name: &str) -> Option<(u32, u32)> {
    let rest = name.trim_start_matches(|c: char| c.is_ascii_alphabetic());
    if rest.len() == name.len() {
        return None;
    }
    let (year, minor) = rest.split_once('.')?;
    let digits = |s: &str, n: Option<usize>| {
        !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) && n.is_none_or(|n| s.len() == n)
    };
    if !digits(year, Some(4)) || !digits(minor, None) {
        return None;
    }
    Some((year.parse().ok()?, minor.parse().ok()?))
}

/// Why a DataGrip project could not be read. Messages never quote document text, which can
/// hold a password.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DataGripError {
    /// The folder holds no `dataSources.xml`, directly or under `.idea`.
    #[error("{} is not a DataGrip project (no dataSources.xml)", folder.display())]
    NotAProject {
        /// The folder that was given.
        folder: PathBuf,
    },
    /// A metadata file could not be read.
    #[error("cannot read {}: {message}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The failure kind.
        message: String,
    },
    /// A metadata file is not valid UTF-8.
    #[error("{} is not valid UTF-8", path.display())]
    NotUtf8 {
        /// The file.
        path: PathBuf,
    },
    /// A metadata file is not well-formed XML (or declares a DTD).
    #[error("{}:{line}: invalid XML: {message}", path.display())]
    Xml {
        /// The file.
        path: PathBuf,
        /// The line of the first error.
        line: u32,
        /// The cause, without any document text.
        message: String,
    },
}
