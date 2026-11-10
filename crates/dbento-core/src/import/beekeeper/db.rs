//! Reading Beekeeper Studio's `app.db` read-only into raw rows. Columns are probed so every
//! schema generation reads; an absent optional column takes its migration default.

use super::BeekeeperError;
use crate::FolderPath;
use crate::import::datagrip::clean_folder;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, Row};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A stored encrypted value; Debug never shows it.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Ciphertext(String);

impl Ciphertext {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Ciphertext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Ciphertext(..)")
    }
}

/// One `saved_connection` row; an absent column holds its migration default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawConnection {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) name_not_utf8: bool,
    pub(crate) connection_type: Option<String>,
    pub(crate) host: Option<String>,
    pub(crate) port: Option<i64>,
    pub(crate) username: Option<String>,
    pub(crate) default_database: Option<String>,
    pub(crate) password: Option<Ciphertext>,
    pub(crate) ssh_enabled: bool,
    pub(crate) ssl: bool,
    pub(crate) ssl_ca_file: Option<String>,
    pub(crate) ssl_cert_file: Option<String>,
    pub(crate) ssl_key_file: Option<String>,
    pub(crate) ssl_reject_unauthorized: bool,
    pub(crate) label_color: Option<String>,
    pub(crate) workspace_id: i64,
    pub(crate) socket_path_enabled: bool,
    pub(crate) socket_path: Option<String>,
    pub(crate) read_only: bool,
    pub(crate) iam_enabled: bool,
    pub(crate) azure_enabled: bool,
    pub(crate) folder: Option<FolderPath>,
}

impl Default for RawConnection {
    fn default() -> Self {
        Self {
            id: 0,
            name: String::new(),
            name_not_utf8: false,
            connection_type: None,
            host: None,
            port: None,
            username: None,
            default_database: None,
            password: None,
            ssh_enabled: false,
            ssl: false,
            ssl_ca_file: None,
            ssl_cert_file: None,
            ssl_key_file: None,
            ssl_reject_unauthorized: true,
            label_color: None,
            workspace_id: -1,
            socket_path_enabled: false,
            socket_path: None,
            read_only: false,
            iam_enabled: false,
            azure_enabled: false,
            folder: None,
        }
    }
}

/// One `favorite_query` row; `folder` is the `query_folder` names, root first, lossy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawQuery {
    pub(crate) id: i64,
    pub(crate) title: String,
    pub(crate) title_not_utf8: bool,
    pub(crate) text: Option<Vec<u8>>,
    pub(crate) folder: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct RawDb {
    pub(crate) connections: Vec<RawConnection>,
    pub(crate) queries: Vec<RawQuery>,
}

const CONNECTION_COLUMNS: &[&str] = &[
    "id",
    "name",
    "connectionType",
    "host",
    "port",
    "username",
    "defaultDatabase",
    "password",
    "sshEnabled",
    "ssl",
    "sslCaFile",
    "sslCertFile",
    "sslKeyFile",
    "sslRejectUnauthorized",
    "labelColor",
    "workspaceId",
    "socketPathEnabled",
    "socketPath",
    "readOnlyMode",
    "iamAuthOptions",
    "azureAuthOptions",
    "connectionFolderId",
];
const FOLDER_COLUMNS: &[&str] = &["id", "name", "parentId"];
const QUERY_COLUMNS: &[&str] = &["id", "title", "text", "queryFolderId"];

/// `(app.db, data folder)`: a directory holding a regular file `app.db`, or a regular file
/// named `app.db` and its parent. Anything else is `NotABeekeeperDir { path }`.
pub(crate) fn locate(path: &Path) -> Result<(PathBuf, PathBuf), BeekeeperError> {
    let not_dir = || BeekeeperError::NotABeekeeperDir {
        path: path.to_path_buf(),
    };
    if path.is_dir() {
        let app_db = path.join("app.db");
        return if app_db.is_file() {
            Ok((app_db, path.to_path_buf()))
        } else {
            Err(not_dir())
        };
    }
    match path.parent() {
        Some(parent) if path.is_file() && path.file_name() == Some("app.db".as_ref()) => {
            Ok((path.to_path_buf(), parent.to_path_buf()))
        }
        _ => Err(not_dir()),
    }
}

/// Opens `app_db` read-only with a 2 s busy timeout and reads every row.
pub(crate) fn read_db(app_db: &Path) -> Result<RawDb, BeekeeperError> {
    let err = |e| classify(app_db, e);
    // No CREATE, URI or immutable: a busy or crashed source must fail to open, never be read
    // half-written, and the folder must stay byte-identical.
    let mut conn = Connection::open_with_flags(
        app_db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(err)?;
    conn.busy_timeout(Duration::from_secs(2)).map_err(err)?;
    // One read transaction gives every table the same snapshot. The probe is its first
    // statement, which is where SQLite reports most open failures.
    let tx = conn.transaction().map_err(err)?;
    let tables: HashSet<String> = tx
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .and_then(|mut s| s.query_map([], |r| r.get(0))?.collect())
        .map_err(err)?;
    let reader = Reader {
        conn: &tx,
        path: app_db,
        tables: &tables,
    };

    let conn_cols = reader.columns("saved_connection", CONNECTION_COLUMNS)?;
    if !["id", "name", "connectionType"]
        .iter()
        .all(|c| conn_cols.contains(c))
    {
        return Err(BeekeeperError::NotABeekeeperDb {
            path: app_db.to_path_buf(),
        });
    }
    let conn_folders = reader.folders("connection_folder")?;
    let connections = reader.rows("saved_connection", &conn_cols, |c| {
        let (name, name_not_utf8) = c.text("name")?.unwrap_or_default();
        let folder_id = c.int("connectionFolderId")?;
        let folder = folder_chain(&conn_folders, folder_id);
        Ok(RawConnection {
            id: c.int("id")?.unwrap_or_default(),
            name,
            name_not_utf8,
            connection_type: c.lossy("connectionType")?,
            host: c.lossy("host")?,
            port: c.int("port")?,
            username: c.lossy("username")?,
            default_database: c.lossy("defaultDatabase")?,
            password: c.lossy("password")?.map(Ciphertext),
            ssh_enabled: c.flag("sshEnabled", false)?,
            ssl: c.flag("ssl", false)?,
            ssl_ca_file: c.lossy("sslCaFile")?,
            ssl_cert_file: c.lossy("sslCertFile")?,
            ssl_key_file: c.lossy("sslKeyFile")?,
            ssl_reject_unauthorized: c.flag("sslRejectUnauthorized", true)?,
            label_color: c.lossy("labelColor")?,
            workspace_id: c.int("workspaceId")?.unwrap_or(-1),
            socket_path_enabled: c.flag("socketPathEnabled", false)?,
            socket_path: c.lossy("socketPath")?,
            read_only: c.flag("readOnlyMode", false)?,
            iam_enabled: json_flag(c.lossy("iamAuthOptions")?, "iamAuthenticationEnabled"),
            azure_enabled: json_flag(c.lossy("azureAuthOptions")?, "azureAuthEnabled"),
            folder: clean_folder(folder.iter().map(String::as_str)),
        })
    })?;

    // Saved queries need at least an id and a title; otherwise there are none to import.
    let query_cols = reader.columns("favorite_query", QUERY_COLUMNS)?;
    let queries = if query_cols.contains(&"id") && query_cols.contains(&"title") {
        let query_folders = reader.folders("query_folder")?;
        reader.rows("favorite_query", &query_cols, |c| {
            let (title, title_not_utf8) = c.text("title")?.unwrap_or_default();
            Ok(RawQuery {
                id: c.int("id")?.unwrap_or_default(),
                title,
                title_not_utf8,
                text: c.bytes("text")?.map(<[u8]>::to_vec),
                folder: folder_chain(&query_folders, c.int("queryFolderId")?),
            })
        })?
    } else {
        Vec::new()
    };
    Ok(RawDb {
        connections,
        queries,
    })
}

/// Sorts a rusqlite error into the `BeekeeperError` the user can act on.
fn classify(path: &Path, e: rusqlite::Error) -> BeekeeperError {
    use rusqlite::ErrorCode::{CannotOpen, DatabaseBusy, DatabaseLocked, NotADatabase, ReadOnly};
    let path = path.to_path_buf();
    match e.sqlite_error_code() {
        Some(NotADatabase) => BeekeeperError::NotABeekeeperDb { path },
        Some(DatabaseBusy | DatabaseLocked | CannotOpen | ReadOnly) => BeekeeperError::Open {
            path,
            message: e.to_string(),
        },
        _ => BeekeeperError::Query {
            path,
            message: e.to_string(),
        },
    }
}

/// A folder table as `id -> (lossy name, parent id)`.
type Folders = HashMap<i64, (String, Option<i64>)>;

/// The names from `start` up through its parents, root first; a dangling or already seen
/// parent ends the walk.
fn folder_chain(folders: &Folders, start: Option<i64>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut chain = Vec::new();
    let mut next = start;
    while let Some(id) = next.filter(|id| seen.insert(*id)) {
        let Some((name, parent)) = folders.get(&id) else {
            break;
        };
        chain.push(name.clone());
        next = *parent;
    }
    chain.reverse();
    chain
}

/// Whether the JSON object in `value` sets `key` to true; non-JSON counts as false.
fn json_flag(value: Option<String>, key: &str) -> bool {
    value
        .and_then(|v| serde_json::from_str::<serde_json::Value>(&v).ok())
        .and_then(|v| v.get(key)?.as_bool())
        .unwrap_or(false)
}

struct Reader<'a> {
    conn: &'a Connection,
    path: &'a Path,
    tables: &'a HashSet<String>,
}

impl Reader<'_> {
    /// The `known` columns `table` has, in `known` order; none when the table is absent.
    fn columns(
        &self,
        table: &str,
        known: &[&'static str],
    ) -> Result<Vec<&'static str>, BeekeeperError> {
        if !self.tables.contains(table) {
            return Ok(Vec::new());
        }
        let present: HashSet<String> = self
            .conn
            .prepare(&format!("PRAGMA table_info(\"{table}\")"))
            .and_then(|mut s| s.query_map([], |r| r.get(1))?.collect())
            .map_err(|e| classify(self.path, e))?;
        Ok(known
            .iter()
            .copied()
            .filter(|c| present.contains(*c))
            .collect())
    }

    /// Every row of `table` in id order, with only `cols` selected, each mapped by `f`.
    fn rows<T>(
        &self,
        table: &'static str,
        cols: &[&'static str],
        mut f: impl FnMut(&Cells<'_>) -> Result<T, BeekeeperError>,
    ) -> Result<Vec<T>, BeekeeperError> {
        let list = cols
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let err = |e| classify(self.path, e);
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {list} FROM \"{table}\" ORDER BY id"))
            .map_err(err)?;
        let mut rows = stmt.query([]).map_err(err)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(err)? {
            out.push(f(&Cells {
                row,
                table,
                cols,
                path: self.path,
            })?);
        }
        Ok(out)
    }

    /// A folder table, or nothing when it or one of its columns is absent.
    fn folders(&self, table: &'static str) -> Result<Folders, BeekeeperError> {
        let cols = self.columns(table, FOLDER_COLUMNS)?;
        if cols.len() < FOLDER_COLUMNS.len() {
            return Ok(Folders::new());
        }
        let rows = self.rows(table, &cols, |c| {
            let name = c.lossy("name")?.unwrap_or_default();
            Ok((c.int("id")?.unwrap_or_default(), (name, c.int("parentId")?)))
        })?;
        Ok(rows.into_iter().collect())
    }
}

/// One row's cells by column name; an absent column or NULL reads as None.
struct Cells<'a> {
    row: &'a Row<'a>,
    table: &'static str,
    cols: &'a [&'static str],
    path: &'a Path,
}

impl Cells<'_> {
    fn get(&self, col: &str) -> Result<Option<ValueRef<'_>>, BeekeeperError> {
        let Some(i) = self.cols.iter().position(|c| *c == col) else {
            return Ok(None);
        };
        match self.row.get_ref(i).map_err(|e| classify(self.path, e))? {
            ValueRef::Null => Ok(None),
            v @ (ValueRef::Integer(_)
            | ValueRef::Real(_)
            | ValueRef::Text(_)
            | ValueRef::Blob(_)) => Ok(Some(v)),
        }
    }

    /// Names the column and storage class, never the value.
    fn mismatch(&self, col: &str, found: ValueRef<'_>, want: &str) -> BeekeeperError {
        let class = match found {
            ValueRef::Null => "null",
            ValueRef::Integer(_) => "an integer",
            ValueRef::Real(_) => "a real",
            ValueRef::Text(_) => "text",
            ValueRef::Blob(_) => "a blob",
        };
        BeekeeperError::Query {
            path: self.path.to_path_buf(),
            message: format!("{}.{col} holds {class} where {want} belongs", self.table),
        }
    }

    fn int(&self, col: &str) -> Result<Option<i64>, BeekeeperError> {
        match self.get(col)? {
            None => Ok(None),
            Some(ValueRef::Integer(n)) => Ok(Some(n)),
            Some(v) => Err(self.mismatch(col, v, "an integer")),
        }
    }

    fn flag(&self, col: &str, default: bool) -> Result<bool, BeekeeperError> {
        Ok(self.int(col)?.map_or(default, |n| n != 0))
    }

    fn bytes(&self, col: &str) -> Result<Option<&[u8]>, BeekeeperError> {
        match self.get(col)? {
            None => Ok(None),
            Some(ValueRef::Text(b) | ValueRef::Blob(b)) => Ok(Some(b)),
            Some(v) => Err(self.mismatch(col, v, "text")),
        }
    }

    /// The text lossily converted, and whether it was not valid UTF-8.
    fn text(&self, col: &str) -> Result<Option<(String, bool)>, BeekeeperError> {
        Ok(self.bytes(col)?.map(|b| match std::str::from_utf8(b) {
            Ok(s) => (s.to_owned(), false),
            Err(_) => (String::from_utf8_lossy(b).into_owned(), true),
        }))
    }

    fn lossy(&self, col: &str) -> Result<Option<String>, BeekeeperError> {
        Ok(self.text(col)?.map(|(s, _)| s))
    }
}

#[cfg(test)]
mod tests;
