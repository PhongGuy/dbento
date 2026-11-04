//! Reading a Beekeeper Studio data folder (`app.db` and its `.key`) into the
//! importer-neutral model. Saved passwords are decrypted only to go to the keychain.

mod crypto;
mod db;
mod map;
mod queries;

use crate::import::datagrip::{assign_name, base_name};
use crate::import::model::{ImportSet, ImportedConnection, ItemKind, WarningKind};
use crate::import::store::ImportSecrets;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// How `read_beekeeper` reads: the user to fall back on and whether saved passwords are read.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct BeekeeperOptions {
    default_user: Option<String>,
    secrets: bool,
}

impl BeekeeperOptions {
    /// No default user, with saved passwords read.
    pub fn new() -> Self {
        BeekeeperOptions {
            default_user: None,
            secrets: true,
        }
    }

    /// The user for a Postgres or MySQL connection that names none.
    pub fn default_user(mut self, user: Option<String>) -> Self {
        self.default_user = user;
        self
    }

    /// Whether saved passwords are read; off, `.key` is never opened.
    pub fn secrets(mut self, on: bool) -> Self {
        self.secrets = on;
        self
    }
}

impl Default for BeekeeperOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// A read data folder: the import set and the saved passwords keyed by `saved_connection.id`.
#[non_exhaustive]
#[derive(Debug)]
pub struct BeekeeperRead {
    set: ImportSet,
    secrets: ImportSecrets,
}

impl BeekeeperRead {
    /// The connections, queries, skips and warnings.
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

/// Reads a data folder or its `app.db`: connections, then saved queries.
pub fn read_beekeeper(
    path: &Path,
    opts: &BeekeeperOptions,
) -> Result<BeekeeperRead, BeekeeperError> {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let (app_db, root) = db::locate(&path)?;
    let raw = db::read_db(&app_db)?;
    let mut set = ImportSet::default();
    let mut secrets = ImportSecrets::default();
    // With secrets off the key file is never opened; a missing one only costs the passwords.
    let key = if opts.secrets {
        let key_path = root.join(".key");
        let key = crypto::read_key(&key_path)?;
        if key.is_none() {
            set.warn(key_path.display().to_string(), WarningKind::KeyMissing);
        }
        key
    } else {
        None
    };
    let ctx = map::MapContext {
        root: &root,
        default_user: opts.default_user.as_deref(),
    };
    // Only imported connections take names, so a skipped one never pushes the next to `-2`.
    let mut used = HashSet::new();
    for c in &raw.connections {
        if c.name_not_utf8 {
            set.warn(&c.name, WarningKind::NotUtf8);
        }
        let mapped = match map::map_connection(c, &ctx) {
            Ok(mapped) => mapped,
            Err(reason) => {
                set.skip(ItemKind::DataSource, &c.name, reason);
                continue;
            }
        };
        let id = c.id.to_string();
        let name = assign_name(&base_name(&c.name), &mut used);
        set.add_connection(ImportedConnection::new(&id, &c.name, name, mapped.draft));
        for w in mapped.warnings {
            set.warn(&c.name, w);
        }
        // Decrypt only a password the connection will keep.
        let cipher = c.password.as_ref().filter(|p| !p.as_str().is_empty());
        if let (true, Some(key), Some(cipher)) = (mapped.keep_secret, &key, cipher) {
            match crypto::decrypt_string(key, cipher.as_str()) {
                Some(secret) => secrets.insert(id, secret),
                None => set.warn(&c.name, WarningKind::SecretUnreadable),
            }
        }
    }
    queries::collect(&app_db, raw.queries, &mut set);
    Ok(BeekeeperRead { set, secrets })
}

/// Why a Beekeeper Studio data folder could not be read. Messages never quote file contents.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BeekeeperError {
    /// The path is neither a folder holding `app.db` nor an `app.db` file.
    #[error("{} is not a Beekeeper Studio data folder (no app.db)", path.display())]
    NotABeekeeperDir {
        /// The path given.
        path: PathBuf,
    },
    /// `app.db` could not be opened, often because Beekeeper Studio holds it.
    #[error("cannot open {}: {message}; close Beekeeper Studio and retry", path.display())]
    Open {
        /// The `app.db` path.
        path: PathBuf,
        /// What went wrong, without any file contents.
        message: String,
    },
    /// `app.db` opened but has no usable `saved_connection` table.
    #[error("{} is not a Beekeeper Studio database (no saved_connection table)", path.display())]
    NotABeekeeperDb {
        /// The `app.db` path.
        path: PathBuf,
    },
    /// A read from `app.db` failed partway.
    #[error("cannot read {}: {message}", path.display())]
    Query {
        /// The `app.db` path.
        path: PathBuf,
        /// What went wrong, naming tables and columns but never values.
        message: String,
    },
    /// The `.key` file exists but could not be read or decrypted.
    #[error("{} cannot be decrypted as a Beekeeper Studio key file", path.display())]
    Key {
        /// The `.key` path.
        path: PathBuf,
    },
}

#[cfg(test)]
mod tests;
