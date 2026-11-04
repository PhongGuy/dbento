//! Planned edits to `dbento.toml` that keep the file's comments, order and line endings.
//!
//! An edit rewrites the loaded text with `toml_edit` and re-validates the result into a
//! `Workspace` before anything is written.

use crate::folder::{FolderPath, WorkspaceItem, visible};
use crate::workspace::{
    AuthLevel, Color, Engine, OpenError, SecretSource, TlsMode, Workspace, color_word, valid_name,
};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, InlineTable, Item, Key, Table, Value};

/// A connection's keys as written in `dbento.toml`; `None` means the key is absent.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionSpec {
    /// `engine`.
    pub engine: Engine,
    /// `path` (SQLite).
    pub path: Option<String>,
    /// `host` (Postgres and MySQL).
    pub host: Option<String>,
    /// `port` (Postgres and MySQL).
    pub port: Option<u16>,
    /// `database` (Postgres and MySQL).
    pub database: Option<String>,
    /// `user` (Postgres and MySQL).
    pub user: Option<String>,
    /// `tls` (Postgres and MySQL).
    pub tls: Option<TlsMode>,
    /// `tls_ca` (Postgres and MySQL), as written.
    pub tls_ca: Option<String>,
    /// `tls_cert` (Postgres and MySQL), as written.
    pub tls_cert: Option<String>,
    /// `tls_key` (Postgres and MySQL), as written.
    pub tls_key: Option<String>,
    /// `read_only_user` (SQL Server).
    pub read_only_user: Option<String>,
    /// `secret` (Postgres and MySQL).
    pub secret: Option<SecretSource>,
    /// `read_only`.
    pub read_only: Option<bool>,
    /// `color`.
    pub color: Option<Color>,
    /// `folder`.
    pub folder: Option<FolderPath>,
    /// `statement_timeout`, as a humantime duration.
    pub statement_timeout: Option<String>,
    /// `uri` (MongoDB).
    pub uri: Option<String>,
    /// `namespace` (SurrealDB).
    pub namespace: Option<String>,
    /// `auth_level` (SurrealDB).
    pub auth_level: Option<AuthLevel>,
}

impl ConnectionSpec {
    /// A spec with only `engine` set.
    pub fn new(engine: Engine) -> Self {
        ConnectionSpec {
            engine,
            path: None,
            host: None,
            port: None,
            database: None,
            user: None,
            tls: None,
            tls_ca: None,
            tls_cert: None,
            tls_key: None,
            read_only_user: None,
            secret: None,
            read_only: None,
            color: None,
            folder: None,
            statement_timeout: None,
            uri: None,
            namespace: None,
            auth_level: None,
        }
    }

    /// Every key in the order a new entry writes them, with its TOML value when present.
    fn values(&self) -> [(&'static str, Option<Value>); 19] {
        let engine = self.engine.name();
        let tls = |t: &TlsMode| match t {
            TlsMode::Verify => "verify",
            TlsMode::Off => "off",
        };
        let secret = |s: &SecretSource| match s {
            SecretSource::Keychain => Value::from("keychain"),
            SecretSource::None => Value::from("none"),
            SecretSource::Env(name) => {
                let mut env = InlineTable::new();
                env.insert("env", Value::from(name.as_str()));
                Value::InlineTable(env)
            }
        };
        let text = |s: &Option<String>| s.as_deref().map(Value::from);
        [
            ("engine", Some(Value::from(engine))),
            ("uri", text(&self.uri)),
            ("path", text(&self.path)),
            ("host", text(&self.host)),
            ("port", self.port.map(|p| Value::from(i64::from(p)))),
            ("database", text(&self.database)),
            ("namespace", text(&self.namespace)),
            ("user", text(&self.user)),
            ("auth_level", self.auth_level.map(|l| Value::from(l.name()))),
            ("secret", self.secret.as_ref().map(secret)),
            ("tls", self.tls.as_ref().map(|t| Value::from(tls(t)))),
            ("tls_ca", text(&self.tls_ca)),
            ("tls_cert", text(&self.tls_cert)),
            ("tls_key", text(&self.tls_key)),
            ("read_only_user", text(&self.read_only_user)),
            ("read_only", self.read_only.map(Value::from)),
            ("color", self.color.map(|c| Value::from(color_word(c)))),
            (
                "folder",
                self.folder.as_ref().map(|f| Value::from(f.as_str())),
            ),
            ("statement_timeout", text(&self.statement_timeout)),
        ]
    }
}

/// One change to `dbento.toml`.
// One edit is built per user action and consumed at once, so its size never matters and
// boxing the spec would only make every caller clumsier.
#[allow(clippy::large_enum_variant)]
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum Edit {
    /// Adds `name`, or rewrites `previous` (renamed when `previous != name`).
    PutConnection {
        /// The connection being edited; `None` adds a new one.
        previous: Option<String>,
        /// The name the connection has after the edit.
        name: String,
        /// Every key the entry should have.
        spec: ConnectionSpec,
    },
    /// Removes the connection's entry.
    DeleteConnection {
        /// The connection to remove.
        name: String,
    },
    /// Declares the folder, or sets or clears its own colour.
    PutFolder {
        /// The folder to declare or recolour.
        path: FolderPath,
        /// Its own colour; `None` clears it.
        color: Option<Color>,
    },
    /// Renames the folder and its whole subtree, connections included.
    RenameFolder {
        /// The folder being renamed.
        from: FolderPath,
        /// The path it has after the edit.
        to: FolderPath,
    },
    /// Removes the folder; its direct contents move up to its parent.
    DeleteFolder {
        /// The folder to remove.
        path: FolderPath,
    },
    /// Moves a connection or a folder into `to`.
    Move {
        /// The connection or folder to move.
        item: WorkspaceItem,
        /// The folder it lands in; `None` is the top level.
        to: Option<FolderPath>,
    },
}

/// Why an edit could not be planned. Nothing was written.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    /// Another connection already has the new name.
    #[error("a connection named `{name}` already exists")]
    NameTaken {
        /// The taken name.
        name: String,
    },
    /// The new name has characters outside `[A-Za-z0-9_-]`.
    #[error("connection name `{name}` must match [A-Za-z0-9_-]+")]
    InvalidName {
        /// The rejected name.
        name: String,
    },
    /// The connection to edit is not in the file.
    #[error("no connection named `{name}` in dbento.toml")]
    NotFound {
        /// The missing name.
        name: String,
    },
    /// The entry is an inline table, which cannot be edited without reformatting it.
    #[error("`{entry}` is written as an inline table in dbento.toml; edit it by hand")]
    Unmergeable {
        /// The connection name or folder path, or `connections` or `folders` for the
        /// whole table.
        entry: String,
    },
    /// The folder to edit is neither declared nor implied.
    #[error("no folder `{path}` in dbento.toml")]
    FolderNotFound {
        /// The missing folder.
        path: FolderPath,
    },
    /// The target folder is already declared or implied.
    #[error("a folder `{path}` already exists")]
    FolderExists {
        /// The existing folder.
        path: FolderPath,
    },
    /// The target path is the folder itself or sits below it.
    #[error("cannot move folder `{path}` into itself")]
    IntoItself {
        /// The folder being moved.
        path: FolderPath,
    },
    /// A new folder path holds a character that does not show on screen.
    #[error("folder path {:?} has an invisible character", path.as_str())]
    Invisible {
        /// The rejected path.
        path: FolderPath,
    },
    /// A rename would move saved queries onto an existing folder.
    #[error("{} already exists; move it before renaming", path.display())]
    QueriesExist {
        /// The existing `queries/<new>/` folder, relative to the workspace directory.
        path: PathBuf,
    },
    /// The edited file fails the workspace rules.
    #[error("{message}")]
    Invalid {
        /// The validation error's text.
        message: String,
    },
}

/// Why an edit plan could not be written to `dbento.toml`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WriteError {
    /// The file no longer holds the text the plan was made from.
    #[error("dbento.toml changed since it was loaded; reload the workspace and redo the edit")]
    Changed,
    /// The file is a symbolic link, which an atomic replace would turn into a plain file.
    #[error("{} is a symbolic link; edit it by hand", path.display())]
    Symlink {
        /// The linked path; relative to the workspace directory for an `EditPlan`.
        path: PathBuf,
    },
    /// A filesystem operation failed.
    #[error("cannot write {}: {message}", path.display())]
    Io {
        /// The path being written; relative to the workspace directory for an `EditPlan`.
        path: PathBuf,
        /// The underlying error's text.
        message: String,
    },
}

impl WriteError {
    // An edit's messages name files below the workspace relative to it, so no local
    // absolute path reaches the user.
    fn relative_to(self, dir: &Path) -> Self {
        let strip = |path: PathBuf| {
            path.strip_prefix(dir)
                .map(Path::to_path_buf)
                .unwrap_or(path)
        };
        match self {
            WriteError::Symlink { path } => WriteError::Symlink { path: strip(path) },
            WriteError::Io { path, message } => WriteError::Io {
                path: strip(path),
                message,
            },
            other => other,
        }
    }
}

/// A validated edit: the new file text and the workspace it describes.
#[derive(Debug)]
pub struct EditPlan {
    base: String,
    new_text: String,
    workspace: Workspace,
    renamed: Option<(String, String)>,
    remap: Option<Remap>,
}

/// How a folder edit moves paths: the subtree at `from` goes under `to` (the top level
/// when `None`), and `from` itself is gone when `deleted`.
#[derive(Debug)]
struct Remap {
    from: FolderPath,
    to: Option<FolderPath>,
    deleted: bool,
}

impl EditPlan {
    /// The full text `dbento.toml` will have.
    pub fn new_text(&self) -> &str {
        &self.new_text
    }

    /// The workspace parsed from the new text.
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// The workspace parsed from the new text, by value.
    pub fn into_workspace(self) -> Workspace {
        self.workspace
    }

    /// `(old, new)` when the edit renames a connection.
    pub fn renamed_connection(&self) -> Option<(&str, &str)> {
        self.renamed
            .as_ref()
            .map(|(old, new)| (old.as_str(), new.as_str()))
    }

    /// The path `old` has after this edit; `None` when the edit deletes that folder.
    pub fn folder_after(&self, old: &FolderPath) -> Option<FolderPath> {
        let Some(r) = &self.remap else {
            return Some(old.clone());
        };
        if r.deleted && old == &r.from {
            return None;
        }
        match shift(old.as_str(), &r.from, r.to.as_ref()) {
            Some(new) => new.parse().ok(),
            None => Some(old.clone()),
        }
    }

    /// Writes the new text over `dbento.toml` atomically, keeping its permissions.
    ///
    /// Fails with `Changed` when the file no longer holds the text the plan was made from,
    /// and with `Symlink` when it is a symbolic link.
    pub fn apply(&self) -> Result<(), WriteError> {
        let dir = self.workspace.dir();
        crate::write::replace_file(
            dir,
            &dir.join("dbento.toml"),
            Some(&self.base),
            &self.new_text,
            ".dbento.toml.edit.tmp",
        )
        .map_err(|e| e.relative_to(dir))
    }

    /// For a rename, moves `queries/<old>/` to `queries/<new>/` when the first exists.
    pub fn move_queries(&self) -> Result<(), WriteError> {
        let Some((old, new)) = &self.renamed else {
            return Ok(());
        };
        let queries = self.workspace.dir().join("queries");
        let (from, to) = (queries.join(old), queries.join(new));
        match std::fs::rename(&from, &to) {
            Err(e) if e.kind() != ErrorKind::NotFound => Err(WriteError::Io {
                path: to,
                message: e.to_string(),
            }
            .relative_to(self.workspace.dir())),
            _ => Ok(()),
        }
    }
}

/// Plans `edit` against the text `ws` was loaded from, without writing anything.
///
/// Fails with `EditError` when the name is bad or taken, the entry is missing or inline,
/// or the result breaks a workspace rule.
pub fn plan_edit(ws: &Workspace, edit: &Edit) -> Result<EditPlan, EditError> {
    let base = ws.source_text();
    let mut doc: DocumentMut =
        base.parse()
            .map_err(|e: toml_edit::TomlError| EditError::Invalid {
                message: e.to_string(),
            })?;
    let mut unchanged = false;
    let mut renamed = None;
    let mut remap = None;
    match edit {
        Edit::PutConnection {
            previous,
            name,
            spec,
        } => {
            if !valid_name(name) {
                return Err(EditError::InvalidName { name: name.clone() });
            }
            if let Some(prev) = previous
                && ws.connection(prev).is_none()
            {
                return Err(EditError::NotFound { name: prev.clone() });
            }
            if previous.as_deref() != Some(name) && ws.connection(name).is_some() {
                return Err(EditError::NameTaken { name: name.clone() });
            }
            match previous {
                None => add_entry(&mut doc, name, spec)?,
                Some(prev) if prev == name => {
                    let old = ws
                        .connection_spec(prev)
                        .ok_or_else(|| EditError::NotFound { name: prev.clone() })?;
                    update_entry(&mut doc, name, &old, spec)?;
                    unchanged = old == *spec;
                }
                Some(prev) => {
                    entry_table(&mut doc, prev)?;
                    let queries = ws.dir().join("queries").join(name);
                    if queries.symlink_metadata().is_ok() {
                        let path = queries.strip_prefix(ws.dir()).unwrap_or(&queries);
                        return Err(EditError::QueriesExist {
                            path: path.to_path_buf(),
                        });
                    }
                    let old = ws
                        .connection_spec(prev)
                        .ok_or_else(|| EditError::NotFound { name: prev.clone() })?;
                    rename_entry(&mut doc, prev, name)?;
                    update_entry(&mut doc, name, &old, spec)?;
                    renamed = Some((prev.clone(), name.clone()));
                }
            }
        }
        Edit::DeleteConnection { name } => {
            if ws.connection(name).is_none() {
                return Err(EditError::NotFound { name: name.clone() });
            }
            entry_table(&mut doc, name)?;
            // The entry's decor goes with it, so comment lines directly above a header go too.
            connections(&mut doc)?.remove(name);
        }
        Edit::PutFolder { path, color } => {
            match ws.folders().iter().find(|f| f.path() == path) {
                Some(f) if f.color() == *color => unchanged = true,
                Some(_) => {}
                None => refuse_invisible(path)?,
            }
            if !unchanged {
                put_folder(&mut doc, path, *color)?;
            }
        }
        Edit::RenameFolder { from, to } => {
            unchanged = rename_folder(ws, &mut doc, from, to)?;
            remap = Some(Remap {
                from: from.clone(),
                to: Some(to.clone()),
                deleted: false,
            });
        }
        Edit::DeleteFolder { path } => {
            if !has_folder(ws, path) {
                return Err(EditError::FolderNotFound { path: path.clone() });
            }
            // Each direct subfolder moves up one level and must not land on a folder there.
            let parent = path.parent();
            for f in ws.folders() {
                if f.path().parent().as_ref() == Some(path) {
                    let landing = child(parent.as_ref(), f.path().name())?;
                    if landing != *path && has_folder(ws, &landing) {
                        return Err(EditError::FolderExists { path: landing });
                    }
                }
            }
            if doc.contains_key("folders") {
                section(&mut doc, "folders")?.retain(|key, _| visible(key) != path.as_str());
            }
            relocate(ws, &mut doc, path, parent.as_ref())?;
            remap = Some(Remap {
                from: path.clone(),
                to: parent,
                deleted: true,
            });
        }
        Edit::Move {
            item: WorkspaceItem::Connection(name),
            to,
        } => {
            let conn = ws
                .connection(name)
                .ok_or_else(|| EditError::NotFound { name: name.clone() })?;
            if conn.folder() == to.as_ref() {
                unchanged = true;
            } else {
                if let Some(t) = to
                    && !has_folder(ws, t)
                {
                    refuse_invisible(t)?;
                }
                set_folder(ws, &mut doc, name, to.clone())?;
            }
        }
        Edit::Move {
            item: WorkspaceItem::Folder(from),
            to,
        } => {
            if !has_folder(ws, from) {
                return Err(EditError::FolderNotFound { path: from.clone() });
            }
            if let Some(t) = to
                && inside(t.as_str(), from)
            {
                return Err(EditError::IntoItself { path: from.clone() });
            }
            let target = child(to.as_ref(), from.name())?;
            unchanged = rename_folder(ws, &mut doc, from, &target)?;
            remap = Some(Remap {
                from: from.clone(),
                to: Some(target),
                deleted: false,
            });
        }
    }

    // toml_edit always writes LF and ends with a newline, so a CRLF file is converted
    // back as a whole and a missing final newline stays missing.
    let mut new_text = doc.to_string();
    if unchanged {
        new_text = base.to_owned();
    } else {
        if base.contains("\r\n") {
            new_text = new_text.replace("\r\n", "\n").replace('\n', "\r\n");
        }
        if !base.is_empty() && !base.ends_with('\n') {
            let trimmed = new_text.trim_end_matches(['\r', '\n']).len();
            new_text.truncate(trimmed);
        }
    }
    let file = ws.dir().join("dbento.toml");
    let workspace = Workspace::from_text(ws.dir().to_path_buf(), &file, &new_text).map_err(
        |e: OpenError| EditError::Invalid {
            message: e.to_string(),
        },
    )?;
    Ok(EditPlan {
        new_text,
        workspace,
        base: base.to_owned(),
        renamed,
        remap,
    })
}

/// The `connections` table, created implicit when absent; an inline one cannot be edited.
fn connections(doc: &mut DocumentMut) -> Result<&mut Table, EditError> {
    section(doc, "connections")
}

/// The top-level table `name`, created implicit when absent; an inline one cannot be edited.
fn section<'a>(doc: &'a mut DocumentMut, name: &str) -> Result<&'a mut Table, EditError> {
    let item = doc.entry(name).or_insert_with(|| {
        let mut t = Table::new();
        t.set_implicit(true);
        Item::Table(t)
    });
    item.as_table_mut()
        .ok_or_else(|| EditError::Unmergeable { entry: name.into() })
}

/// Refuses a path with a character that renders as nothing, so two folders that look
/// alike are never created by an edit.
fn refuse_invisible(path: &FolderPath) -> Result<(), EditError> {
    let invisible = |c: char| {
        matches!(c, '\u{AD}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}' | '\u{061C}' | '\u{180E}' | '\u{FEFF}')
    };
    if path.as_str().chars().any(invisible) {
        return Err(EditError::Invisible { path: path.clone() });
    }
    Ok(())
}

/// Whether the folder is declared or implied.
fn has_folder(ws: &Workspace, path: &FolderPath) -> bool {
    ws.folders().iter().any(|f| f.path() == path)
}

/// Whether `p` is `from` or sits below it.
fn inside(p: &str, from: &FolderPath) -> bool {
    shift(p, from, None).is_some()
}

/// The path `p` has once the subtree at `from` moves under `to` (the top level when
/// `None`), or `None` when `p` is outside it. An empty result is `from` moved to the top.
fn shift(p: &str, from: &FolderPath, to: Option<&FolderPath>) -> Option<String> {
    let tail = p
        .strip_prefix(from.as_str())
        .filter(|rest| rest.is_empty() || rest.starts_with('/'))?;
    Some(match to {
        Some(to) => format!("{to}{tail}"),
        None => tail.trim_start_matches('/').to_owned(),
    })
}

/// The folder `name` inside `parent`, or at the top level.
fn child(parent: Option<&FolderPath>, name: &str) -> Result<FolderPath, EditError> {
    let text = match parent {
        Some(p) => format!("{p}/{name}"),
        None => name.to_owned(),
    };
    parse_path(&text)
}

/// Parses a folder path an edit computed.
fn parse_path(text: &str) -> Result<FolderPath, EditError> {
    text.parse()
        .map_err(|e: crate::folder::FolderPathError| EditError::Invalid {
            message: e.to_string(),
        })
}

/// Renames the folder's subtree to `to`; returns true when nothing changes.
fn rename_folder(
    ws: &Workspace,
    doc: &mut DocumentMut,
    from: &FolderPath,
    to: &FolderPath,
) -> Result<bool, EditError> {
    if from == to {
        return Ok(true);
    }
    if !has_folder(ws, from) {
        return Err(EditError::FolderNotFound { path: from.clone() });
    }
    if inside(to.as_str(), from) {
        return Err(EditError::IntoItself { path: from.clone() });
    }
    if has_folder(ws, to) {
        return Err(EditError::FolderExists { path: to.clone() });
    }
    refuse_invisible(to)?;
    relocate(ws, doc, from, Some(to))?;
    Ok(false)
}

/// Moves every folder key and connection folder at or below `from` under `to` (the top
/// level when `None`). A key that maps to `from` itself at the top level is left alone,
/// since only a delete does that and it has removed the key first.
fn relocate(
    ws: &Workspace,
    doc: &mut DocumentMut,
    from: &FolderPath,
    to: Option<&FolderPath>,
) -> Result<(), EditError> {
    if doc.contains_key("folders") {
        rename_keys(section(doc, "folders")?, |p| {
            shift(&visible(p), from, to).filter(|new| !new.is_empty())
        })?;
    }
    for conn in ws.connections() {
        let Some(new) = conn.folder().and_then(|f| shift(f.as_str(), from, to)) else {
            continue;
        };
        let folder = if new.is_empty() {
            None
        } else {
            Some(parse_path(&new)?)
        };
        set_folder(ws, doc, conn.name(), folder)?;
    }
    Ok(())
}

/// Sets or removes one connection's `folder` key, leaving its other keys untouched.
fn set_folder(
    ws: &Workspace,
    doc: &mut DocumentMut,
    name: &str,
    folder: Option<FolderPath>,
) -> Result<(), EditError> {
    let old = ws
        .connection_spec(name)
        .ok_or_else(|| EditError::NotFound { name: name.into() })?;
    let mut spec = old.clone();
    spec.folder = folder;
    update_entry(doc, name, &old, &spec)
}

/// Sets or clears the folder's `color` in place, or appends a new `[folders.<path>]`
/// after the last table when the folder is not declared yet.
fn put_folder(
    doc: &mut DocumentMut,
    path: &FolderPath,
    color: Option<Color>,
) -> Result<(), EditError> {
    let last = last_position(doc.as_table()).unwrap_or(0);
    let folders = section(doc, "folders")?;
    // A hand-written key may differ from the path by zero-width characters only.
    let written = folders
        .iter()
        .map(|(key, _)| key)
        .find(|key| visible(key) == path.as_str())
        .map(str::to_owned);
    let key = written.as_deref().unwrap_or(path.as_str());
    let color = color.map(|c| Value::from(color_word(c)));
    match folders.get_mut(key) {
        Some(item) => {
            let entry = item
                .as_table_mut()
                .ok_or_else(|| EditError::Unmergeable { entry: key.into() })?;
            match color {
                None => {
                    entry.remove("color");
                }
                Some(mut value) => {
                    if let Some(Item::Value(previous)) = entry.get("color") {
                        *value.decor_mut() = previous.decor().clone();
                    }
                    entry.insert("color", Item::Value(value));
                }
            }
        }
        None => {
            let mut table = Table::new();
            table.set_position(Some(last + 1));
            table.decor_mut().set_prefix("\n");
            if let Some(value) = color {
                table.insert("color", Item::Value(value));
            }
            folders.insert(key, Item::Table(table));
        }
    }
    Ok(())
}

/// Appends `[connections.<name>]` after the last table, so it never lands inside a
/// later `[folders]` section, with the keys in the spec's order.
fn add_entry(doc: &mut DocumentMut, name: &str, spec: &ConnectionSpec) -> Result<(), EditError> {
    let last = last_position(doc.as_table()).unwrap_or(0);
    let mut table = Table::new();
    table.set_position(Some(last + 1));
    table.decor_mut().set_prefix("\n");
    for (key, value) in spec.values() {
        if let Some(value) = value {
            table.insert(key, Item::Value(value));
        }
    }
    connections(doc)?.insert(name, Item::Table(table));
    Ok(())
}

/// The highest document position of any table at or below `table`.
fn last_position(table: &Table) -> Option<isize> {
    let nested = table.iter().flat_map(|(_, item)| match item {
        Item::Table(t) => vec![last_position(t)],
        Item::ArrayOfTables(a) => a.iter().map(last_position).collect(),
        _ => Vec::new(),
    });
    nested.chain([table.position()]).flatten().max()
}

/// The connection's own table; an inline one cannot be edited without reformatting it.
fn entry_table<'a>(doc: &'a mut DocumentMut, name: &str) -> Result<&'a mut Table, EditError> {
    connections(doc)?
        .get_mut(name)
        .and_then(Item::as_table_mut)
        .ok_or_else(|| EditError::Unmergeable { entry: name.into() })
}

/// Renames the connection's entry in place.
fn rename_entry(doc: &mut DocumentMut, old: &str, new: &str) -> Result<(), EditError> {
    rename_keys(connections(doc)?, |name| {
        (name == old).then(|| new.to_owned())
    })
}

/// Renames every key `rename` maps: each entry is taken out and put back in order, a renamed
/// key carrying the old key's decor, so a dotted entry and a header keep their place.
/// A renamed entry must be a table; an inline one is refused.
fn rename_keys(
    table: &mut Table,
    rename: impl Fn(&str) -> Option<String>,
) -> Result<(), EditError> {
    let names: Vec<String> = table.iter().map(|(k, _)| k.to_owned()).collect();
    for name in names {
        let Some((mut key, item)) = table.remove_entry(&name) else {
            continue;
        };
        if let Some(new) = rename(&name) {
            if !item.is_table() {
                return Err(EditError::Unmergeable { entry: name });
            }
            let mut renamed = Key::new(new);
            *renamed.leaf_decor_mut() = key.leaf_decor().clone();
            *renamed.dotted_decor_mut() = key.dotted_decor().clone();
            key = renamed;
        }
        table.insert_formatted(&key, item);
    }
    Ok(())
}

/// Touches only the keys whose value changed: a changed value keeps its comment and
/// place, a cleared key goes, and a new key ends the entry.
fn update_entry(
    doc: &mut DocumentMut,
    name: &str,
    old: &ConnectionSpec,
    new: &ConnectionSpec,
) -> Result<(), EditError> {
    let entry = entry_table(doc, name)?;
    for ((key, before), (_, after)) in old.values().into_iter().zip(new.values()) {
        if before.as_ref().map(ToString::to_string) == after.as_ref().map(ToString::to_string) {
            continue;
        }
        match after {
            None => {
                entry.remove(key);
            }
            Some(mut value) => {
                if let Some(Item::Value(previous)) = entry.get(key) {
                    *value.decor_mut() = previous.decor().clone();
                }
                entry.insert(key, Item::Value(value));
            }
        }
    }
    // Session settings are Postgres-only; inline, sub-table and dotted forms are all one key here.
    if new.engine != Engine::Postgres {
        entry.remove("settings");
    }
    Ok(())
}
