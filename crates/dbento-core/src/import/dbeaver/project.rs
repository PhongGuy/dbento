//! Finding a DBeaver project folder and reading its storages, credentials and script
//! bindings into raw records. Fields are picked leniently from `serde_json::Value`, so a
//! parse error never carries document text.

use super::DBeaverError;
use super::credentials::read_credentials;
use crate::Secret;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

/// How deep a legacy `folders` parent chain is followed; the bound also stops a cycle.
const MAX_FOLDER_DEPTH: usize = 16;

/// One project as read from disk, before any mapping.
#[derive(Debug)]
pub(crate) struct RawProject {
    /// The project folder, absolute.
    pub(crate) root: PathBuf,
    /// Connections in reading order, keeping the first definition of an id.
    pub(crate) connections: Vec<RawConnection>,
    /// Script path relative to the project (`Scripts/a.sql`) to connection id.
    pub(crate) bindings: BTreeMap<String, String>,
    /// Connection type id to its `color`, from each storage's `connection-types`.
    pub(crate) connection_types: BTreeMap<String, String>,
}

/// One `connections` entry of a storage, with its login from the storage's credentials.
#[derive(Debug, Default)]
pub(crate) struct RawConnection {
    pub(crate) id: String,
    /// Empty when absent.
    pub(crate) provider: String,
    /// Empty when absent.
    pub(crate) driver: String,
    /// The id when absent.
    pub(crate) name: String,
    /// The resolved slash path.
    pub(crate) folder: Option<String>,
    pub(crate) read_only: bool,
    pub(crate) config: RawConfig,
    pub(crate) user: Option<String>,
    /// None when secrets were not requested.
    pub(crate) password: Option<Secret>,
}

/// A connection's `configuration` object, every value as text.
#[derive(Default)]
pub(crate) struct RawConfig {
    pub(crate) host: Option<String>,
    pub(crate) port: Option<String>,
    pub(crate) database: Option<String>,
    pub(crate) url: Option<String>,
    /// `configurationType`.
    pub(crate) configuration_type: Option<String>,
    /// `auth-model`.
    pub(crate) auth_model: Option<String>,
    /// `type`, DBeaver's connection type such as `dev` or `prod`.
    pub(crate) connection_type: Option<String>,
    pub(crate) color: Option<String>,
    pub(crate) properties: BTreeMap<String, String>,
    pub(crate) handlers: BTreeMap<String, RawHandler>,
}

/// One network handler (`postgre_ssl`, `ssh_tunnel`, ...) of a connection.
#[derive(Default)]
pub(crate) struct RawHandler {
    pub(crate) enabled: bool,
    pub(crate) properties: BTreeMap<String, String>,
}

// The URL and property values can hold passwords, so Debug shows property names only.
impl fmt::Debug for RawConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("database", &self.database)
            .field("url", &self.url.as_ref().map(|_| "<redacted>"))
            .field("configuration_type", &self.configuration_type)
            .field("auth_model", &self.auth_model)
            .field("connection_type", &self.connection_type)
            .field("color", &self.color)
            .field("properties", &self.properties.keys().collect::<Vec<_>>())
            .field("handlers", &self.handlers)
            .finish()
    }
}

impl fmt::Debug for RawHandler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawHandler")
            .field("enabled", &self.enabled)
            .field("properties", &self.properties.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Reads the project at `folder` (or at its parent when `folder` is its `.dbeaver`).
/// With `secrets` false no password is kept. Errors never quote file contents.
pub(crate) fn read_project(folder: &Path, secrets: bool) -> Result<RawProject, DBeaverError> {
    let given = std::path::absolute(folder).map_err(|e| io(folder, e))?;
    let root = if !storages(&given.join(".dbeaver")).is_empty() {
        given
    } else if given.file_name().is_some_and(|n| n == ".dbeaver")
        && !storages(&given).is_empty()
        && let Some(parent) = given.parent()
    {
        parent.to_path_buf()
    } else {
        return Err(DBeaverError::NotAProject {
            folder: folder.to_path_buf(),
            projects: projects_below(&given),
        });
    };
    let meta = root.join(".dbeaver");

    // An encrypted project is refused before any storage is read.
    if let Some(settings) = read_object(&meta.join("project-settings.json"))?
        && ["secureProject", "useProjectPassword"]
            .iter()
            .any(|k| flag(settings.get(*k)))
    {
        return Err(DBeaverError::EncryptedProject { folder: root });
    }

    let mut seen = BTreeSet::new();
    let mut connections = Vec::new();
    let mut connection_types = BTreeMap::new();
    for name in storages(&meta) {
        let Some(storage) = read_object(&meta.join(&name))? else {
            continue;
        };
        let suffix = &name["data-sources".len()..name.len() - ".json".len()];
        let mut creds = read_credentials(
            &meta.join(format!("credentials-config{suffix}.json")),
            secrets,
        )?;
        let empty = Map::new();
        let folders = object(storage.get("folders")).unwrap_or(&empty);
        for (id, t) in object(storage.get("connection-types"))
            .into_iter()
            .flatten()
        {
            if let Some(color) = text(t.get("color")) {
                connection_types.entry(id.clone()).or_insert(color);
            }
        }
        // serde_json's map order depends on a feature flag, so ids are sorted explicitly.
        let mut entries: Vec<_> = object(storage.get("connections"))
            .map(|m| m.iter().collect())
            .unwrap_or_default();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        for (id, value) in entries {
            if !seen.insert(id.clone()) {
                continue;
            }
            let mut conn = connection(id, object(Some(value)).unwrap_or(&empty), folders);
            if let Some(c) = creds.remove(id) {
                conn.user = c.user;
                conn.password = c.password;
            }
            connections.push(conn);
        }
    }

    let bindings = read_object(&meta.join("project-metadata.json"))?
        .map(|m| {
            object(m.get("resources"))
                .into_iter()
                .flatten()
                .filter_map(|(path, r)| {
                    let id = r.get("default-datasource")?.as_str()?;
                    Some((path.clone(), id.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(RawProject {
        root,
        connections,
        bindings,
        connection_types,
    })
}

fn connection(id: &str, c: &Map<String, Value>, folders: &Map<String, Value>) -> RawConnection {
    let empty = Map::new();
    let cfg = object(c.get("configuration")).unwrap_or(&empty);
    let handlers = object(cfg.get("handlers"))
        .map(|m| {
            m.iter()
                .filter_map(|(name, h)| {
                    let h = h.as_object()?;
                    Some((
                        name.clone(),
                        RawHandler {
                            enabled: flag(h.get("enabled")),
                            properties: strings(h.get("properties")),
                        },
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    RawConnection {
        id: id.to_string(),
        provider: text(c.get("provider")).unwrap_or_default(),
        driver: text(c.get("driver")).unwrap_or_default(),
        name: text(c.get("name")).unwrap_or_else(|| id.to_string()),
        folder: text(c.get("folder")).map(|f| folder_path(f, folders)),
        read_only: flag(c.get("read-only")),
        config: RawConfig {
            host: text(cfg.get("host")),
            port: text(cfg.get("port")),
            database: text(cfg.get("database")),
            url: text(cfg.get("url")),
            configuration_type: text(cfg.get("configurationType")),
            auth_model: text(cfg.get("auth-model")),
            connection_type: text(cfg.get("type")),
            color: text(cfg.get("color")),
            properties: strings(cfg.get("properties")),
            handlers,
        },
        user: None,
        password: None,
    }
}

// A legacy `folders` entry chains to its `parent`; a folder not listed is used as written.
fn folder_path(name: String, folders: &Map<String, Value>) -> String {
    let mut path = name.clone();
    let mut current = name;
    for _ in 0..MAX_FOLDER_DEPTH {
        let Some(parent) = folders.get(&current).and_then(|f| text(f.get("parent"))) else {
            break;
        };
        path = format!("{parent}/{path}");
        current = parent;
    }
    path
}

// Storage file names in reading order: `data-sources.json` first, then the rest by name.
fn storages(meta: &Path) -> Vec<String> {
    let Ok(dir) = std::fs::read_dir(meta) else {
        return Vec::new();
    };
    let mut names: Vec<String> = dir
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("data-sources") && n.ends_with(".json"))
        .collect();
    names.sort_by_key(|n| (n != "data-sources.json", n.clone()));
    names
}

fn projects_below(folder: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir() && !storages(&e.path().join(".dbeaver")).is_empty())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    found.sort();
    found
}

// Reads a JSON object file; a missing file is None.
fn read_object(path: &Path) -> Result<Option<Map<String, Value>>, DBeaverError> {
    let bytes = match std::fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        r => r.map_err(|e| io(path, e))?,
    };
    if std::str::from_utf8(&bytes).is_err() {
        return Err(DBeaverError::NotUtf8 {
            path: path.to_path_buf(),
        });
    }
    let json = |line, message: String| DBeaverError::Json {
        path: path.to_path_buf(),
        line,
        message,
    };
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(m)) => Ok(Some(m)),
        Ok(_) => Err(json(1, "expected an object".into())),
        Err(e) => {
            let text = e.to_string();
            let message = text.split(" at line").next().unwrap_or_default();
            Err(json(e.line(), message.to_string()))
        }
    }
}

pub(super) fn io(path: &Path, e: std::io::Error) -> DBeaverError {
    DBeaverError::Io {
        path: path.to_path_buf(),
        message: e.kind().to_string(),
    }
}

fn object(v: Option<&Value>) -> Option<&Map<String, Value>> {
    v?.as_object()
}

// A string is kept, a number or bool becomes its text, anything else is absent.
fn text(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

// JSON `true` or the string "true" in any case, as Java's `Boolean.parseBoolean` reads it.
fn flag(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

fn strings(v: Option<&Value>) -> BTreeMap<String, String> {
    object(v)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.clone(), text(Some(v))?)))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
