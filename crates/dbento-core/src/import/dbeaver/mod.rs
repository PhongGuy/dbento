//! Reading a DBeaver project into the importer-neutral model.

mod credentials;
mod map;
mod project;
mod scripts;

use crate::import::datagrip::{assign_name, base_name};
use crate::import::model::{ImportSet, ImportedConnection, ItemKind};
use crate::import::store::ImportSecrets;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// How `read_dbeaver` reads a project: the user to fall back on and whether saved
/// passwords are kept for the keychain.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct DBeaverOptions {
    default_user: Option<String>,
    secrets: bool,
}

impl DBeaverOptions {
    /// No default user, with saved passwords kept.
    pub fn new() -> Self {
        DBeaverOptions {
            default_user: None,
            secrets: true,
        }
    }

    /// The user for a Postgres or MySQL connection that names none.
    pub fn default_user(mut self, user: Option<String>) -> Self {
        self.default_user = user;
        self
    }

    /// Whether saved passwords are read; users are read either way.
    pub fn secrets(mut self, on: bool) -> Self {
        self.secrets = on;
        self
    }
}

impl Default for DBeaverOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// A read DBeaver project: the import set and the saved passwords of its imported
/// connections, keyed by connection id.
#[non_exhaustive]
#[derive(Debug)]
pub struct DBeaverRead {
    set: ImportSet,
    secrets: ImportSecrets,
}

impl DBeaverRead {
    /// The connections, scripts, skips and warnings.
    pub fn set(&self) -> &ImportSet {
        &self.set
    }

    /// The saved passwords to store after the plan is applied.
    pub fn secrets(&self) -> &ImportSecrets {
        &self.secrets
    }

    /// The set and the secrets, apart.
    pub fn into_parts(self) -> (ImportSet, ImportSecrets) {
        (self.set, self.secrets)
    }
}

/// Reads one DBeaver project (its folder or its `.dbeaver` folder): connections, then scripts.
pub fn read_dbeaver(folder: &Path, opts: &DBeaverOptions) -> Result<DBeaverRead, DBeaverError> {
    let folder = std::path::absolute(folder).unwrap_or_else(|_| folder.to_path_buf());
    let project = project::read_project(&folder, opts.secrets)?;
    let ctx = map::MapContext {
        root: &project.root,
        default_user: opts.default_user.as_deref(),
        connection_types: &project.connection_types,
    };
    let mut set = ImportSet::default();
    let mut secrets = ImportSecrets::default();
    // Only imported connections take names, so a skipped one never pushes the next to `-2`.
    let (mut used, mut imported) = (HashSet::new(), HashSet::new());
    for mut raw in project.connections {
        match map::map_connection(&raw, &ctx) {
            Ok(mapped) => {
                let name = assign_name(&base_name(&raw.name), &mut used);
                set.add_connection(ImportedConnection::new(
                    &raw.id,
                    &raw.name,
                    name,
                    mapped.draft,
                ));
                for w in mapped.warnings {
                    set.warn(&raw.name, w);
                }
                if mapped.keep_secret
                    && opts.secrets
                    && let Some(password) = raw.password.take()
                {
                    secrets.insert(raw.id.clone(), password);
                }
                imported.insert(raw.id);
            }
            Err(reason) => set.skip(ItemKind::DataSource, &raw.name, reason),
        }
    }
    scripts::collect(&project.root, &project.bindings, &imported, &mut set)?;
    Ok(DBeaverRead { set, secrets })
}

/// Why a DBeaver project could not be read. Messages never quote file contents.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DBeaverError {
    /// The folder holds no `.dbeaver/data-sources*.json`.
    #[error(
        "{} is not a DBeaver project (no .dbeaver/data-sources*.json){}",
        folder.display(),
        projects_here(projects)
    )]
    NotAProject {
        /// The folder that was given.
        folder: PathBuf,
        /// Sub-folders that are DBeaver projects, for the hint.
        projects: Vec<String>,
    },
    /// The project encrypts its configuration with a user password.
    #[error("{} is an encrypted DBeaver project, which cannot be imported", folder.display())]
    EncryptedProject {
        /// The project folder.
        folder: PathBuf,
    },
    /// A project file could not be read.
    #[error("cannot read {}: {message}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The failure kind.
        message: String,
    },
    /// A project file is not valid UTF-8.
    #[error("{} is not valid UTF-8", path.display())]
    NotUtf8 {
        /// The file.
        path: PathBuf,
    },
    /// A project file is not valid JSON.
    #[error("{}:{line}: invalid JSON: {message}", path.display())]
    Json {
        /// The file.
        path: PathBuf,
        /// The line of the first error.
        line: usize,
        /// The cause, without any document text.
        message: String,
    },
    /// The credentials file is too large, not DBeaver's cipher, or not its JSON shape.
    #[error("{} cannot be decrypted as DBeaver credentials", path.display())]
    Credentials {
        /// The file.
        path: PathBuf,
    },
}

fn projects_here(projects: &[String]) -> String {
    if projects.is_empty() {
        String::new()
    } else {
        format!("; projects here: {}", projects.join(", "))
    }
}

#[cfg(test)]
mod tests;
