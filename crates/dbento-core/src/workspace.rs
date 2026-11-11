//! The workspace folder: `dbento.toml` parsing and validation.
//!
//! Parsing is synchronous and engine-free. No secret is ever read from or kept in the file.

use crate::edit::ConnectionSpec;
use crate::folder::{FolderPath, parse_written};
use crate::session::{Capabilities, QueryLanguage, ReadOnlyEnforcement};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The database engine a connection talks to.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// PostgreSQL.
    Postgres,
    /// SQLite.
    Sqlite,
    /// MySQL or MariaDB.
    Mysql,
    /// SQL Server.
    Mssql,
    /// DuckDB.
    Duckdb,
    /// MongoDB.
    Mongodb,
    /// SurrealDB.
    Surrealdb,
    /// Redis or Valkey.
    Redis,
    /// Amazon DynamoDB.
    Dynamodb,
}

impl Engine {
    /// The engine as `dbento.toml` names it: `postgres`, `sqlite`, `mysql`, `mssql`, `duckdb`,
    /// `mongodb`, `surrealdb`, `redis` or `dynamodb`.
    pub fn name(self) -> &'static str {
        match self {
            Engine::Postgres => "postgres",
            Engine::Sqlite => "sqlite",
            Engine::Mysql => "mysql",
            Engine::Mssql => "mssql",
            Engine::Duckdb => "duckdb",
            Engine::Mongodb => "mongodb",
            Engine::Surrealdb => "surrealdb",
            Engine::Redis => "redis",
            Engine::Dynamodb => "dynamodb",
        }
    }

    /// What the engine supports in this build; the same for every session of it.
    pub fn capabilities(self) -> Capabilities {
        let none = Capabilities {
            schemas: false,
            server_cancel: false,
            password: false,
            file: false,
            tls: false,
            edit_rows: false,
            explain: false,
            table_definition: false,
            language: QueryLanguage::Sql,
            key_browser: false,
        };
        match self {
            Engine::Postgres => Capabilities {
                schemas: true,
                server_cancel: true,
                password: true,
                tls: true,
                edit_rows: true,
                explain: true,
                ..none
            },
            Engine::Sqlite => Capabilities {
                server_cancel: true,
                file: true,
                edit_rows: true,
                explain: true,
                table_definition: true,
                ..none
            },
            // Only what works today: editing and EXPLAIN come in later plans.
            Engine::Mysql => Capabilities {
                schemas: true,
                server_cancel: true,
                password: true,
                tls: true,
                table_definition: true,
                ..none
            },
            Engine::Mssql => Capabilities {
                schemas: true,
                server_cancel: true,
                password: true,
                tls: true,
                ..none
            },
            Engine::Duckdb => Capabilities {
                schemas: true,
                file: true,
                server_cancel: true,
                table_definition: true,
                ..none
            },
            Engine::Mongodb => Capabilities {
                schemas: true,
                password: true,
                tls: true,
                language: QueryLanguage::MongoShell,
                ..none
            },
            Engine::Surrealdb => Capabilities {
                schemas: true,
                server_cancel: true,
                password: true,
                tls: true,
                language: QueryLanguage::SurrealQl,
                ..none
            },
            Engine::Redis => Capabilities {
                schemas: true,
                password: true,
                tls: true,
                key_browser: true,
                language: QueryLanguage::RedisCommands,
                ..none
            },
            Engine::Dynamodb => Capabilities {
                password: true,
                language: QueryLanguage::PartiQl,
                ..none
            },
        }
    }

    /// The engine a `dbento.toml` `engine` value names; `mariadb` is an alias of `mysql`,
    /// `sqlserver` one of `mssql`, `mongo` one of `mongodb`, `surreal` one of `surrealdb`,
    /// `valkey` one of `redis` and `dynamo` one of `dynamodb`.
    fn parse(name: &str) -> Option<Engine> {
        match name {
            "postgres" => Some(Engine::Postgres),
            "mysql" | "mariadb" => Some(Engine::Mysql),
            "sqlite" => Some(Engine::Sqlite),
            "mssql" | "sqlserver" => Some(Engine::Mssql),
            "duckdb" => Some(Engine::Duckdb),
            "mongodb" | "mongo" => Some(Engine::Mongodb),
            "surrealdb" | "surreal" => Some(Engine::Surrealdb),
            "redis" | "valkey" => Some(Engine::Redis),
            "dynamodb" | "dynamo" => Some(Engine::Dynamodb),
            _ => None,
        }
    }
}

/// The color tag of a connection; red means production.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// Production.
    Red,
    /// Orange.
    Orange,
    /// Yellow.
    Yellow,
    /// Green.
    Green,
    /// Teal.
    Teal,
    /// Blue.
    Blue,
    /// Purple.
    Purple,
    /// Gray.
    Gray,
}

/// How a Postgres connection secures its transport.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsMode {
    /// TLS with certificate verification.
    Verify,
    /// Plain TCP.
    Off,
}

/// The level a SurrealDB user signs in at.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthLevel {
    /// Root.
    Root,
    /// Namespace.
    Namespace,
    /// Database.
    Database,
}

impl AuthLevel {
    /// The word `dbento.toml` uses: `root`, `namespace` or `database`.
    pub fn name(self) -> &'static str {
        match self {
            AuthLevel::Root => "root",
            AuthLevel::Namespace => "namespace",
            AuthLevel::Database => "database",
        }
    }

    fn parse(word: &str) -> Option<AuthLevel> {
        [AuthLevel::Root, AuthLevel::Namespace, AuthLevel::Database]
            .into_iter()
            .find(|level| level.name() == word)
    }
}

/// Where a connection's secret comes from.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretSource {
    /// The OS keychain.
    Keychain,
    /// The connection needs no secret.
    None,
    /// The named environment variable.
    Env(String),
}

/// Why a workspace could not be opened.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// There is no `dbento.toml` in the directory.
    #[error("no dbento.toml in {}{}", start.display(), if *parents { " or any parent" } else { "" })]
    NotFound {
        /// The directory that was searched.
        start: PathBuf,
        /// Whether the parent directories were searched too.
        parents: bool,
    },
    /// The file exists but could not be read.
    #[error("cannot read {}: {message}", path.display())]
    Io {
        /// The file that failed to read.
        path: PathBuf,
        /// The underlying error text.
        message: String,
    },
    /// The file is not valid TOML or does not match the format.
    #[error("{}: {message}", path.display())]
    Parse {
        /// The file that failed to parse.
        path: PathBuf,
        /// The parser's message, which names the offending key.
        message: String,
    },
    /// A key named `password` was found.
    #[error(
        "`{key}`: passwords never go in dbento.toml; store it with `dbento secret set <connection>`"
    )]
    Password {
        /// The dotted path of the key.
        key: String,
    },
    /// The `dbento` format version is not one this build reads.
    #[error("unsupported workspace format version {found}; this dbento reads version 1")]
    UnsupportedVersion {
        /// The version found in the file.
        found: i64,
    },
    /// A connection name has characters outside `[A-Za-z0-9_-]`.
    #[error("connection name `{name}` must match [A-Za-z0-9_-]+")]
    InvalidName {
        /// The rejected name.
        name: String,
    },
    /// A connection's keys are inconsistent for its engine.
    #[error("connection `{connection}`: {message}")]
    Invalid {
        /// The connection name.
        connection: String,
        /// What is wrong.
        message: String,
    },
    /// A `[folders]` table has a bad path or colour.
    #[error("folder `{folder}`: {message}")]
    InvalidFolder {
        /// The folder key as written.
        folder: String,
        /// What is wrong.
        message: String,
    },
}

/// Why a workspace could not be initialised.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum InitError {
    /// `dbento.toml` is already there; nothing was written.
    #[error("{} already exists", path.display())]
    Exists {
        /// The existing file.
        path: PathBuf,
    },
    /// A file or folder could not be written.
    #[error("cannot write {}: {message}", path.display())]
    Io {
        /// The path that failed.
        path: PathBuf,
        /// The underlying error text.
        message: String,
    },
}

/// The largest saved query `Workspace::read_query` loads, in bytes.
pub const MAX_QUERY_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Why a saved query could not be read or written.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum QueryFileError {
    /// The file or a folder could not be accessed.
    #[error("cannot access {}: {message}", path.display())]
    Io {
        /// The path that failed, relative to the workspace directory.
        path: PathBuf,
        /// The underlying error text.
        message: String,
    },
    /// The path leaves `queries/`, lexically or through a symlink.
    #[error("{} is not a file under the workspace's queries/ folder", path.display())]
    OutsideQueries {
        /// The path as given.
        path: PathBuf,
    },
    /// The file is not valid UTF-8.
    #[error("{} is not UTF-8 text", path.display())]
    NotUtf8 {
        /// The path as given.
        path: PathBuf,
    },
}

/// A saved query file under `queries/`, bound to a connection by its folder.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedQuery {
    path: PathBuf,
    connection: Option<String>,
}

impl SavedQuery {
    /// The file path, relative to the workspace directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The connection whose name is the first folder under `queries/`, if configured.
    pub fn connection(&self) -> Option<&str> {
        self.connection.as_deref()
    }

    /// The language its extension implies, in any case: `.js` is the shell, `.surql` is
    /// SurrealQL, `.redis` is Redis commands, `.partiql` is PartiQL, anything else SQL.
    pub fn language(&self) -> QueryLanguage {
        let ext = self.path.extension();
        [
            QueryLanguage::MongoShell,
            QueryLanguage::SurrealQl,
            QueryLanguage::RedisCommands,
            QueryLanguage::PartiQl,
        ]
        .into_iter()
        .find(|l| ext.is_some_and(|e| e.eq_ignore_ascii_case(l.file_extension())))
        .unwrap_or(QueryLanguage::Sql)
    }
}

pub(crate) const TEMPLATE: &str = "\
# Dbento workspace. Commit this file; passwords never go here.
dbento = 1

# [connections.local]
# engine = \"sqlite\"
# path = \"data/app.db\"
# read_only = true
#
# [connections.prod]
# engine = \"postgres\"
# host = \"db.example.com\"
# database = \"app\"
# user = \"app\"
# secret = { env = \"PROD_DB_PASSWORD\" }
# color = \"red\"
";

/// An opened workspace folder and its connections.
#[non_exhaustive]
#[derive(Debug)]
pub struct Workspace {
    dir: PathBuf,
    source: String,
    connections: Vec<ConnectionConfig>,
    folders: Vec<Folder>,
}

/// A folder in the workspace tree: declared under `[folders]` or implied by a path.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    path: FolderPath,
    declared: bool,
    color: Option<Color>,
    effective_color: Option<Color>,
}

impl Folder {
    /// The full folder path.
    pub fn path(&self) -> &FolderPath {
        &self.path
    }

    /// Whether `dbento.toml` has a `[folders]` table for this path.
    pub fn declared(&self) -> bool {
        self.declared
    }

    /// The folder's own colour, if declared with one.
    pub fn color(&self) -> Option<Color> {
        self.color
    }

    /// The own colour, else the nearest ancestor's.
    pub fn effective_color(&self) -> Option<Color> {
        self.effective_color
    }
}

/// The databases a Postgres or SQL Server parent exposes as child connections.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Databases {
    /// Every database the server lists, written `databases = "all"`.
    All,
    /// Only these, in the order written.
    Listed(Vec<String>),
}

/// One configured connection. Holds no secret.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    name: String,
    engine: Engine,
    read_only: bool,
    color: Option<Color>,
    folder: Option<FolderPath>,
    effective_color: Option<Color>,
    statement_timeout: Option<Duration>,
    secret_source: SecretSource,
    path: Option<String>,
    file_path: Option<PathBuf>,
    host: Option<String>,
    port: Option<u16>,
    database: Option<String>,
    user: Option<String>,
    tls: Option<TlsMode>,
    tls_ca: Option<PathBuf>,
    tls_cert: Option<PathBuf>,
    tls_key: Option<PathBuf>,
    settings: BTreeMap<String, String>,
    read_only_user: Option<String>,
    databases: Option<Databases>,
    uri: Option<String>,
    namespace: Option<String>,
    auth_level: Option<AuthLevel>,
    region: Option<String>,
    endpoint: Option<String>,
    access_key_id: Option<String>,
    consistent_read: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRoot {
    #[allow(dead_code)]
    dbento: i64,
    #[serde(default)]
    connections: BTreeMap<String, RawConnection>,
    #[serde(default)]
    folders: BTreeMap<String, RawFolder>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFolder {
    color: Option<String>,
}

// One flat struct with every key optional; per-engine rules are checked by hand,
// which keeps `deny_unknown_fields` usable (it does not work on a tagged enum).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConnection {
    engine: String,
    path: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    database: Option<String>,
    user: Option<String>,
    secret: Option<RawSecret>,
    tls: Option<String>,
    tls_ca: Option<String>,
    tls_cert: Option<String>,
    tls_key: Option<String>,
    settings: Option<BTreeMap<String, String>>,
    read_only_user: Option<String>,
    databases: Option<RawDatabases>,
    read_only: Option<bool>,
    color: Option<String>,
    folder: Option<String>,
    statement_timeout: Option<String>,
    uri: Option<String>,
    namespace: Option<String>,
    auth_level: Option<String>,
    region: Option<String>,
    endpoint: Option<String>,
    access_key_id: Option<String>,
    consistent_read: Option<bool>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawSecret {
    Word(String),
    Env(RawEnv),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawDatabases {
    Word(String),
    List(Vec<String>),
}

// A parent's `databases`: the word "all" or a non-empty list of distinct database names.
// SQL Server compares names case-insensitively, so it also refuses a pair differing in case.
fn databases(raw: RawDatabases, engine: Engine) -> Result<Databases, String> {
    let names = match raw {
        RawDatabases::Word(w) if w == "all" => return Ok(Databases::All),
        RawDatabases::Word(_) => {
            return Err("`databases` must be \"all\" or a list of database names".to_owned());
        }
        RawDatabases::List(names) => names,
    };
    if names.is_empty() {
        return Err("`databases` lists no database; use \"all\" or name at least one".to_owned());
    }
    for (i, name) in names.iter().enumerate() {
        if !valid_name(name) {
            return Err(format!(
                "`databases` entry `{name}` must match [A-Za-z0-9_-]+"
            ));
        }
        for earlier in &names[..i] {
            if earlier == name {
                return Err(format!("`databases` lists `{name}` twice"));
            }
            if engine == Engine::Mssql && earlier.to_lowercase() == name.to_lowercase() {
                return Err(format!(
                    "`databases` lists `{earlier}` and `{name}`, which SQL Server treats as one database"
                ));
            }
        }
    }
    Ok(Databases::Listed(names))
}

// Its own struct because `deny_unknown_fields` is ignored on an untagged struct variant.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEnv {
    env: String,
}

fn tls_mode(word: &str) -> Result<TlsMode, String> {
    match word {
        "verify" => Ok(TlsMode::Verify),
        "off" => Ok(TlsMode::Off),
        other => Err(format!("tls `{other}` must be \"verify\" or \"off\"")),
    }
}

// A server engine's `secret`: the keychain unless written otherwise.
fn secret_source(raw: Option<RawSecret>) -> Result<SecretSource, String> {
    match raw {
        None => Ok(SecretSource::Keychain),
        Some(RawSecret::Env(e)) if e.env.is_empty() => {
            Err("secret `env` needs a variable name".to_owned())
        }
        Some(RawSecret::Env(e)) => Ok(SecretSource::Env(e.env)),
        Some(RawSecret::Word(w)) => match w.as_str() {
            "keychain" => Ok(SecretSource::Keychain),
            "none" => Ok(SecretSource::None),
            // The word may be the password itself, so the message never quotes it.
            _ => Err("secret must be \"keychain\", \"none\" or { env = \"NAME\" }".to_owned()),
        },
    }
}

// toml's own Display prints the offending line, which can hold a password no later check has
// seen, so only the line number and the message leave this file. The message quotes a wrong-typed
// string value in double quotes, which go too; key names sit in backticks and stay.
fn toml_reason(text: &str, error: &toml::de::Error) -> String {
    let message = error.message();
    let message = match (message.find('"'), message.rfind('"')) {
        (Some(open), Some(close)) if open < close => {
            format!("{}\"...\"{}", &message[..open], &message[close + 1..])
        }
        _ => message.to_owned(),
    };
    match error.span() {
        Some(span) => {
            let line = text
                .bytes()
                .take(span.start)
                .filter(|b| *b == b'\n')
                .count()
                + 1;
            format!("line {line}: {message}")
        }
        None => message,
    }
}

impl Workspace {
    /// Reads `<dir>/dbento.toml`.
    ///
    /// Rejects unknown keys, any `password` key, bad names and engine-inconsistent
    /// connections.
    pub fn open(dir: impl AsRef<Path>) -> Result<Workspace, OpenError> {
        let dir = dir.as_ref();
        let file = dir.join("dbento.toml");
        #[allow(
            clippy::wildcard_enum_match_arm,
            reason = "ErrorKind has too many variants to list; the rest share one arm"
        )]
        let text = std::fs::read_to_string(&file).map_err(|e| match e.kind() {
            ErrorKind::NotFound => OpenError::NotFound {
                start: dir.to_path_buf(),
                parents: false,
            },
            _ => OpenError::Io {
                path: file.clone(),
                message: e.to_string(),
            },
        })?;
        let canonical = std::fs::canonicalize(dir).map_err(|e| OpenError::Io {
            path: dir.to_path_buf(),
            message: e.to_string(),
        })?;
        Workspace::from_text(canonical, &file, &text)
    }

    /// Parses and validates `text` as the `dbento.toml` at `file`, for the workspace `dir`.
    ///
    /// `dir` is taken as given (callers pass it canonical); relative SQLite paths resolve
    /// against it.
    pub(crate) fn from_text(dir: PathBuf, file: &Path, text: &str) -> Result<Workspace, OpenError> {
        let canonical = dir;
        let parse = |message: String| OpenError::Parse {
            path: file.to_path_buf(),
            message,
        };

        // The untyped table comes first so the version and password checks win over key errors.
        let table: toml::Table = text
            .parse()
            .map_err(|e: toml::de::Error| parse(toml_reason(text, &e)))?;
        match table.get("dbento") {
            Some(toml::Value::Integer(1)) => {}
            Some(toml::Value::Integer(found)) => {
                return Err(OpenError::UnsupportedVersion { found: *found });
            }
            _ => {
                return Err(parse(
                    "missing integer key `dbento` (the format version)".to_owned(),
                ));
            }
        }
        if let Some(key) = find_password(&toml::Value::Table(table), "") {
            return Err(OpenError::Password { key });
        }

        let raw: RawRoot = toml::from_str(text).map_err(|e| parse(toml_reason(text, &e)))?;
        let mut declared = BTreeMap::new();
        for (key, folder) in raw.folders {
            let invalid = |message: String| OpenError::InvalidFolder {
                folder: key.clone(),
                message,
            };
            let path = parse_written(&key).map_err(|e| invalid(format!("{e}")))?;
            let color = match folder.color.as_deref() {
                None => None,
                Some(c) => {
                    Some(parse_color(c).ok_or_else(|| invalid(format!("unknown color `{c}`")))?)
                }
            };
            declared.insert(path, color);
        }
        let mut connections = Vec::new();
        for (name, conn) in raw.connections {
            connections.push(ConnectionConfig::build(name, conn, &canonical)?);
        }

        // Every declared folder, every connection's folder and all their ancestors form
        // the tree; a colour flows down from the nearest declared ancestor that has one.
        let inherited = |path: &FolderPath| {
            std::iter::successors(Some(path.clone()), FolderPath::parent)
                .find_map(|p| declared.get(&p).copied().flatten())
        };
        let mut paths: BTreeSet<FolderPath> = declared.keys().cloned().collect();
        for conn in &mut connections {
            if let Some(folder) = &conn.folder {
                paths.extend(std::iter::successors(
                    Some(folder.clone()),
                    FolderPath::parent,
                ));
            }
            conn.effective_color = conn
                .color
                .or_else(|| conn.folder.as_ref().and_then(inherited));
        }
        let folders = paths
            .into_iter()
            .map(|path| Folder {
                declared: declared.contains_key(&path),
                color: declared.get(&path).copied().flatten(),
                effective_color: inherited(&path),
                path,
            })
            .collect();
        Ok(Workspace {
            dir: canonical,
            source: text.to_owned(),
            connections,
            folders,
        })
    }

    /// Opens the nearest workspace at or above `start`.
    pub fn discover(start: impl AsRef<Path>) -> Result<Workspace, OpenError> {
        let start = start.as_ref();
        for dir in start.ancestors() {
            match Workspace::open(dir) {
                Err(OpenError::NotFound { .. }) => {}
                found => return found,
            }
        }
        Err(OpenError::NotFound {
            start: start.to_path_buf(),
            parents: true,
        })
    }

    /// Writes a commented `dbento.toml` and an empty `queries/` into `dir`.
    ///
    /// Refuses, before writing anything, if `dbento.toml` already exists.
    pub fn init(dir: impl AsRef<Path>) -> Result<(), InitError> {
        let dir = dir.as_ref();
        let file = dir.join("dbento.toml");
        let io = |path: &Path, e: std::io::Error| InitError::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        };
        // create_new makes the existence check and the write one atomic step.
        let mut out = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file)
        {
            Ok(out) => out,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                return Err(InitError::Exists { path: file });
            }
            Err(e) => return Err(io(&file, e)),
        };
        std::io::Write::write_all(&mut out, TEMPLATE.as_bytes()).map_err(|e| io(&file, e))?;
        // Only after the file is ours: a refused init must not touch the directory.
        let queries = dir.join("queries");
        std::fs::create_dir_all(&queries).map_err(|e| io(&queries, e))
    }

    /// Every `.sql`, `.js`, `.surql`, `.redis` and `.partiql` file under `queries/`, sorted by
    /// path.
    pub fn saved_queries(&self) -> Result<Vec<SavedQuery>, OpenError> {
        let root = self.dir.join("queries");
        let mut found = Vec::new();
        let walk =
            std::fs::canonicalize(&root).and_then(|real| collect_queries(&root, &real, &mut found));
        #[allow(
            clippy::wildcard_enum_match_arm,
            reason = "ErrorKind has too many variants to list; the rest share one arm"
        )]
        walk.or_else(|e| match e.kind() {
            ErrorKind::NotFound => Ok(()),
            _ => Err(OpenError::Io {
                path: root.clone(),
                message: e.to_string(),
            }),
        })?;
        let mut queries: Vec<SavedQuery> = found
            .into_iter()
            .map(|file| self.bind(file.strip_prefix(&self.dir).unwrap_or(&file).to_path_buf()))
            .collect();
        queries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(queries)
    }

    /// The text of `q`.
    pub fn read_query(&self, q: &SavedQuery) -> Result<String, QueryFileError> {
        use std::io::Read as _;
        let rel = q.path();
        let file = self.confine(rel, false)?;
        let io = |message: String| QueryFileError::Io {
            path: self.shown(&file),
            message,
        };
        // Check the kind and size before opening, so a FIFO cannot block the open and
        // a device such as /dev/zero is never read; the capped read covers a growing file.
        let meta = std::fs::metadata(&file).map_err(|e| io(e.to_string()))?;
        if !meta.is_file() {
            return Err(io("not a regular file".into()));
        }
        let too_big = || io(format!("larger than {MAX_QUERY_FILE_BYTES} bytes"));
        if meta.len() > MAX_QUERY_FILE_BYTES {
            return Err(too_big());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&file)
            .and_then(|f| f.take(MAX_QUERY_FILE_BYTES + 1).read_to_end(&mut bytes))
            .map_err(|e| io(e.to_string()))?;
        if bytes.len() as u64 > MAX_QUERY_FILE_BYTES {
            return Err(too_big());
        }
        String::from_utf8(bytes).map_err(|_| QueryFileError::NotUtf8 {
            path: rel.to_path_buf(),
        })
    }

    /// The text at `rel` (relative to the workspace directory, below `queries/`), or None when
    /// nothing is there; anything else that cannot be read is an error, as in `read_query`.
    pub fn read_query_at(&self, rel: &Path) -> Result<Option<String>, QueryFileError> {
        if !self.query_exists(rel)? {
            return Ok(None);
        }
        self.read_query(&self.bind(rel.to_path_buf())).map(Some)
    }

    /// Whether something already sits at `rel` (relative to the workspace directory, below
    /// `queries/`), resolved and confined exactly as `read_query` and `write_query` are, so a
    /// path they refuse is refused here too. No `queries/` folder means nothing exists.
    pub fn query_exists(&self, rel: &Path) -> Result<bool, QueryFileError> {
        let file = match self.confine(rel, false) {
            Ok(file) => file,
            // `confine` validated `rel` before it found `queries/` missing.
            Err(QueryFileError::Io { .. })
                if std::fs::symlink_metadata(self.dir.join("queries"))
                    .is_err_and(|e| e.kind() == ErrorKind::NotFound) =>
            {
                return Ok(false);
            }
            Err(e) => return Err(e),
        };
        match std::fs::symlink_metadata(&file) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
            Err(e) => Err(QueryFileError::Io {
                path: self.shown(&file),
                message: e.to_string(),
            }),
        }
    }

    /// Writes `text` to `rel` (relative to the workspace directory, below `queries/`),
    /// creating folders, by renaming a temporary file over it; returns it bound the way
    /// `saved_queries` binds it.
    pub fn write_query(&self, rel: &Path, text: &str) -> Result<SavedQuery, QueryFileError> {
        use std::io::Write as _;
        let file = self.confine(rel, true)?;
        #[allow(clippy::expect_used)] // confine joins `rel` below the workspace directory
        let parent = file.parent().expect("a confined path has a parent");
        let io = |path: &Path, e: std::io::Error| QueryFileError::Io {
            path: self.shown(path),
            message: e.to_string(),
        };
        std::fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
        // Re-check after creating: a folder swapped for a symlink meanwhile is refused.
        self.confine(rel, false)?;

        // A temporary file in the target's folder, synced and renamed over it, so a reader
        // sees the old text or the new one and never a torn file. A new query is created
        // 0666 through the umask, like any editor's file; a rewrite keeps the file's mode.
        // Only the unix mode below mutates the builder.
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut builder = tempfile::Builder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            builder.permissions(std::fs::Permissions::from_mode(0o666));
        }
        let mut tmp = builder.tempfile_in(parent).map_err(|e| io(parent, e))?;
        // The temporary file's random name means nothing to the user; errors name the target.
        tmp.write_all(text.as_bytes()).map_err(|e| io(&file, e))?;
        #[cfg(unix)]
        if let Ok(meta) = std::fs::metadata(&file) {
            tmp.as_file()
                .set_permissions(meta.permissions())
                .map_err(|e| io(&file, e))?;
        }
        tmp.as_file().sync_all().map_err(|e| io(&file, e))?;
        tmp.persist(&file).map_err(|e| io(&file, e.error))?;
        #[cfg(unix)]
        std::fs::File::open(parent)
            .and_then(|d| d.sync_all())
            .map_err(|e| io(parent, e))?;
        Ok(self.bind(rel.components().collect()))
    }

    /// Resolves `rel` to an absolute path inside the resolved `queries/` folder.
    /// `rel` must be relative, made only of plain names, start with `queries` and name
    /// something below it. The deepest existing ancestor and an existing file are
    /// canonicalized (a dangling link is refused) before anything is created, so no
    /// read, write or new folder ever goes through a link that leaves `queries/`.
    fn confine(&self, rel: &Path, create_root: bool) -> Result<PathBuf, QueryFileError> {
        use std::path::Component;
        let outside = || QueryFileError::OutsideQueries {
            path: rel.to_path_buf(),
        };
        let mut parts = rel.components();
        let plain = rel.components().all(|c| matches!(c, Component::Normal(_)));
        if !plain || parts.next() != Some(Component::Normal("queries".as_ref())) {
            return Err(outside());
        }
        if parts.next().is_none() {
            return Err(outside());
        }
        let literal = self.dir.join("queries");
        if create_root {
            match std::fs::create_dir(&literal) {
                Err(e) if e.kind() != ErrorKind::AlreadyExists => {
                    return Err(QueryFileError::Io {
                        path: self.shown(&literal),
                        message: e.to_string(),
                    });
                }
                _ => {}
            }
        }
        let file = self.dir.join(rel);
        #[allow(
            clippy::wildcard_enum_match_arm,
            reason = "ErrorKind has too many variants to list; the rest share one arm"
        )]
        let resolve = |p: &Path| {
            std::fs::canonicalize(p).map_err(|e| match e.kind() {
                ErrorKind::NotFound => outside(),
                _ => QueryFileError::Io {
                    path: self.shown(p),
                    message: e.to_string(),
                },
            })
        };
        let root = match std::fs::symlink_metadata(&literal) {
            Ok(_) => resolve(&literal)?,
            // No `queries/` yet (only on read), so the file cannot exist.
            Err(e) => {
                return Err(QueryFileError::Io {
                    path: self.shown(&file),
                    message: e.to_string(),
                });
            }
        };
        // Below `literal`, which exists, so the walk stops at `literal` at the latest.
        let mut below = file.as_path();
        let ancestor = loop {
            let Some(parent) = below.parent() else {
                return Err(outside());
            };
            if std::fs::symlink_metadata(parent).is_ok() {
                break parent;
            }
            below = parent;
        };
        if !resolve(ancestor)?.starts_with(&root) {
            return Err(outside());
        }
        // The file itself may be a link: an escaping or dangling one is refused for
        // reading and for writing (a rename would replace the link, not follow it).
        if std::fs::symlink_metadata(&file).is_ok() && !resolve(&file)?.starts_with(&root) {
            return Err(outside());
        }
        Ok(file)
    }

    /// `path` as messages show it: relative to the workspace directory when below it, so no
    /// local absolute path reaches the user.
    fn shown(&self, path: &Path) -> PathBuf {
        path.strip_prefix(&self.dir).unwrap_or(path).to_path_buf()
    }

    /// Binds a path relative to the workspace: the first folder under `queries/` names
    /// the connection when one is configured by that name.
    fn bind(&self, path: PathBuf) -> SavedQuery {
        // A file directly in queries/ has no folder, so it binds to nothing.
        let mut parts = path.components().skip(1);
        let connection = match (parts.next(), parts.next()) {
            (Some(folder), Some(_)) => folder.as_os_str().to_str(),
            _ => None,
        }
        .filter(|name| self.connection(name).is_some())
        .map(str::to_owned);
        SavedQuery { path, connection }
    }

    /// The canonical workspace directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// All connections, sorted by name.
    pub fn connections(&self) -> &[ConnectionConfig] {
        &self.connections
    }

    /// Declared and implied folders, in tree order.
    pub fn folders(&self) -> &[Folder] {
        &self.folders
    }

    /// The connection with this name, if configured.
    pub fn connection(&self, name: &str) -> Option<&ConnectionConfig> {
        self.connections.iter().find(|c| c.name == name)
    }

    /// A connection written in dbento.toml; what edits, keychain writes and `queries/` moves act on.
    pub fn declared(&self, name: &str) -> Option<&ConnectionConfig> {
        self.connections.iter().find(|c| c.name == name)
    }

    /// The text of `dbento.toml` this workspace was loaded from.
    pub fn source_text(&self) -> &str {
        &self.source
    }

    /// The connection's keys exactly as written, before defaults are resolved.
    ///
    /// `None` when no connection has this name.
    pub fn connection_spec(&self, name: &str) -> Option<ConnectionSpec> {
        // Re-read from the source text, so the spec always matches what an edit rewrites.
        let mut raw: RawRoot = toml::from_str(&self.source).ok()?;
        let raw = raw.connections.remove(name)?;
        let mut spec = ConnectionSpec::new(Engine::parse(&raw.engine)?);
        spec.path = raw.path;
        spec.host = raw.host;
        spec.port = raw.port;
        spec.database = raw.database;
        spec.user = raw.user;
        spec.tls = raw.tls.map(|t| match t.as_str() {
            "verify" => TlsMode::Verify,
            _ => TlsMode::Off,
        });
        spec.tls_ca = raw.tls_ca;
        spec.tls_cert = raw.tls_cert;
        spec.tls_key = raw.tls_key;
        spec.read_only_user = raw.read_only_user;
        // The source text already loaded, so any word here is "all".
        spec.databases = raw.databases.map(|d| match d {
            RawDatabases::Word(_) => Databases::All,
            RawDatabases::List(names) => Databases::Listed(names),
        });
        spec.secret = raw.secret.map(|s| match s {
            RawSecret::Env(e) => SecretSource::Env(e.env),
            RawSecret::Word(w) if w == "keychain" => SecretSource::Keychain,
            RawSecret::Word(_) => SecretSource::None,
        });
        spec.read_only = raw.read_only;
        spec.color = raw.color.as_deref().and_then(parse_color);
        spec.folder = raw.folder.and_then(|f| parse_written(&f).ok());
        spec.statement_timeout = raw.statement_timeout;
        spec.uri = raw.uri;
        spec.namespace = raw.namespace;
        spec.auth_level = raw.auth_level.as_deref().and_then(AuthLevel::parse);
        spec.region = raw.region;
        spec.endpoint = raw.endpoint;
        spec.access_key_id = raw.access_key_id;
        spec.consistent_read = raw.consistent_read;
        Some(spec)
    }
}

/// Recursively gathers `.sql`, `.js`, `.surql`, `.redis` and `.partiql` files below `dir`,
/// skipping any whose real path leaves `root` (the canonical `queries/`) and dangling links.
fn collect_queries(dir: &Path, root: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect_queries(&path, root, out)?;
        } else if path.extension().is_some_and(|e| {
            [
                QueryLanguage::Sql,
                QueryLanguage::MongoShell,
                QueryLanguage::SurrealQl,
                QueryLanguage::RedisCommands,
                QueryLanguage::PartiQl,
            ]
            .iter()
            .any(|l| e.eq_ignore_ascii_case(l.file_extension()))
        }) && std::fs::canonicalize(&path).is_ok_and(|real| real.starts_with(root))
        {
            out.push(path);
        }
    }
    Ok(())
}

/// Dotted path of the first key named `password` at any depth.
fn find_password(value: &toml::Value, prefix: &str) -> Option<String> {
    match value {
        toml::Value::Table(t) => t.iter().find_map(|(k, v)| {
            let path = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            if k.eq_ignore_ascii_case("password") {
                Some(path)
            } else {
                find_password(v, &path)
            }
        }),
        toml::Value::Array(a) => a.iter().find_map(|v| find_password(v, prefix)),
        toml::Value::String(_)
        | toml::Value::Integer(_)
        | toml::Value::Float(_)
        | toml::Value::Boolean(_)
        | toml::Value::Datetime(_) => None,
    }
}

pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub(crate) fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A setting name is an identifier, or two identifiers joined by one dot (custom GUCs).
fn valid_setting_name(name: &str) -> bool {
    match name.split_once('.') {
        Some((a, b)) => is_identifier(a) && is_identifier(b),
        None => is_identifier(name),
    }
}

const RESERVED_SETTINGS: [&str; 4] = [
    "client_encoding",
    "bytea_output",
    "statement_timeout",
    "default_transaction_read_only",
];

/// The lowercase word `dbento.toml` uses for `c`; the inverse of `parse_color`.
pub(crate) fn color_word(c: Color) -> &'static str {
    match c {
        Color::Red => "red",
        Color::Orange => "orange",
        Color::Yellow => "yellow",
        Color::Green => "green",
        Color::Teal => "teal",
        Color::Blue => "blue",
        Color::Purple => "purple",
        Color::Gray => "gray",
    }
}

fn parse_color(s: &str) -> Option<Color> {
    Some(match s {
        "red" => Color::Red,
        "orange" => Color::Orange,
        "yellow" => Color::Yellow,
        "green" => Color::Green,
        "teal" => Color::Teal,
        "blue" => Color::Blue,
        "purple" => Color::Purple,
        "gray" => Color::Gray,
        _ => return None,
    })
}

/// Loopback addresses, `localhost` and unix socket directories default to no TLS.
pub(crate) fn is_local_host(host: &str) -> bool {
    host.starts_with('/')
        || host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Ok when `text` is a usable endpoint; the error never holds `text`, which may hold credentials.
fn endpoint_rule(text: &str) -> Result<(), &'static str> {
    let url = url::Url::parse(text).map_err(|_| "`endpoint` must be an http or https URL")?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("`endpoint` must be an http or https URL");
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("`endpoint` must not hold a user, a path, a query or a fragment");
    }
    // Plain http is fine only when the request never leaves this machine.
    let loopback = match url.host() {
        Some(url::Host::Domain(d)) => d == "localhost",
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    if url.scheme() == "http" && !loopback {
        return Err(
            "`endpoint` uses http on a host that is not this machine: use https; signed requests over http expose the data",
        );
    }
    Ok(())
}

/// Postgres takes whole milliseconds in an i32, and 0 there means no timeout.
pub(crate) fn timeout_in_range(d: Duration) -> bool {
    !d.is_zero() && d.as_micros().div_ceil(1000) <= i32::MAX as u128
}

impl ConnectionConfig {
    fn build(name: String, raw: RawConnection, dir: &Path) -> Result<Self, OpenError> {
        if !valid_name(&name) {
            return Err(OpenError::InvalidName { name });
        }
        let invalid = |message: String| OpenError::Invalid {
            connection: name.clone(),
            message,
        };

        let engine = Engine::parse(&raw.engine).ok_or_else(|| {
            invalid(format!(
                "unknown engine `{}`; use postgres, mysql, mssql, sqlite, duckdb, mongodb, surrealdb, redis or dynamodb",
                raw.engine
            ))
        })?;
        let color = match raw.color.as_deref() {
            None => None,
            Some(c) => Some(parse_color(c).ok_or_else(|| invalid(format!("unknown color `{c}`")))?),
        };
        let folder = match raw.folder.as_deref() {
            None => None,
            Some(f) => Some(parse_written(f).map_err(|e| invalid(format!("{e}")))?),
        };
        let statement_timeout = match raw.statement_timeout.as_deref() {
            None => None,
            Some(t) => {
                let d = humantime::parse_duration(t)
                    .map_err(|e| invalid(format!("statement_timeout `{t}`: {e}")))?;
                if !timeout_in_range(d) {
                    return Err(invalid(format!(
                        "statement_timeout `{t}` must be above zero and at most 2147483647ms"
                    )));
                }
                Some(d)
            }
        };

        let mut config = ConnectionConfig {
            name: name.clone(),
            engine,
            read_only: raw.read_only.unwrap_or(false),
            color,
            folder,
            // Resolved by `Workspace::from_text` once every folder is known.
            effective_color: color,
            statement_timeout,
            secret_source: SecretSource::None,
            path: None,
            file_path: None,
            host: None,
            port: None,
            database: None,
            user: None,
            tls: None,
            tls_ca: None,
            tls_cert: None,
            tls_key: None,
            settings: BTreeMap::new(),
            read_only_user: None,
            databases: None,
            uri: None,
            namespace: None,
            auth_level: None,
            region: None,
            endpoint: None,
            access_key_id: None,
            consistent_read: false,
        };

        // Checked ahead of the per-engine rules, whose messages would call it a postgres key.
        if engine != Engine::Mssql && raw.read_only_user.is_some() {
            return Err(invalid(format!(
                "`read_only_user` is a mssql key, not valid for {}",
                engine.name()
            )));
        }
        if engine != Engine::Mongodb && raw.uri.is_some() {
            return Err(invalid(format!(
                "`uri` is a mongodb key, not valid for {}",
                engine.name()
            )));
        }
        if engine != Engine::Surrealdb {
            for (key, present) in [
                ("namespace", raw.namespace.is_some()),
                ("auth_level", raw.auth_level.is_some()),
            ] {
                if present {
                    return Err(invalid(format!(
                        "`{key}` is a surrealdb key, not valid for {}",
                        engine.name()
                    )));
                }
            }
        }
        if engine != Engine::Dynamodb {
            for (key, present) in [
                ("region", raw.region.is_some()),
                ("endpoint", raw.endpoint.is_some()),
                ("access_key_id", raw.access_key_id.is_some()),
                ("consistent_read", raw.consistent_read.is_some()),
            ] {
                if present {
                    return Err(invalid(format!(
                        "`{key}` is a dynamodb key, not valid for {}",
                        engine.name()
                    )));
                }
            }
        }

        // Ahead of the per-engine rules, so its message wins over a "postgres key" one.
        if let Some(raw_databases) = raw.databases {
            match engine {
                Engine::Postgres | Engine::Mssql => {
                    config.databases = Some(databases(raw_databases, engine).map_err(invalid)?);
                }
                Engine::Mysql => {
                    return Err(invalid(
                        "`databases` is not valid for mysql: leave `database` empty to browse every database as a schema"
                            .to_owned(),
                    ));
                }
                Engine::Sqlite
                | Engine::Duckdb
                | Engine::Mongodb
                | Engine::Surrealdb
                | Engine::Redis
                | Engine::Dynamodb => {
                    return Err(invalid(format!(
                        "`databases` is a postgres and mssql key, not valid for {}",
                        engine.name()
                    )));
                }
            }
        }

        match engine {
            // The file engines share one shape: a path resolved against the workspace.
            Engine::Sqlite | Engine::Duckdb => {
                let ename = engine.name();
                for (key, present) in [
                    ("host", raw.host.is_some()),
                    ("port", raw.port.is_some()),
                    ("database", raw.database.is_some()),
                    ("user", raw.user.is_some()),
                    ("secret", raw.secret.is_some()),
                    ("tls", raw.tls.is_some()),
                    ("tls_ca", raw.tls_ca.is_some()),
                    ("tls_cert", raw.tls_cert.is_some()),
                    ("tls_key", raw.tls_key.is_some()),
                    ("settings", raw.settings.is_some()),
                ] {
                    if present {
                        return Err(invalid(format!(
                            "`{key}` is a postgres key, not valid for {ename}"
                        )));
                    }
                }
                let path = raw
                    .path
                    .ok_or_else(|| invalid(format!("{ename} needs `path`")))?;
                config.file_path = Some(dir.join(&path));
                config.path = Some(path);
            }
            // MySQL and SQL Server share the server keys; only Postgres needs a database or
            // takes settings.
            Engine::Postgres | Engine::Mysql | Engine::Mssql => {
                let ename = engine.name();
                if raw.path.is_some() {
                    return Err(invalid(format!(
                        "`path` is a sqlite key, not valid for {ename}"
                    )));
                }
                if engine != Engine::Postgres && raw.settings.is_some() {
                    return Err(invalid(format!(
                        "`settings` is a postgres key, not valid for {ename}"
                    )));
                }
                // No client certificate on SQL Server; refused before the pair rules so
                // either key alone is named as not valid rather than as half a pair.
                if engine == Engine::Mssql {
                    for (key, present) in [
                        ("tls_cert", raw.tls_cert.is_some()),
                        ("tls_key", raw.tls_key.is_some()),
                    ] {
                        if present {
                            return Err(invalid(format!("`{key}` is not valid for mssql")));
                        }
                    }
                    if raw
                        .read_only_user
                        .as_deref()
                        .is_some_and(|u| u.trim().is_empty())
                    {
                        return Err(invalid("`read_only_user` must not be empty".to_owned()));
                    }
                    config.read_only_user = raw.read_only_user;
                }
                let host = raw
                    .host
                    .ok_or_else(|| invalid(format!("{ename} needs `host`")))?;
                config.database = match (engine, raw.database) {
                    (Engine::Postgres, None) => {
                        return Err(invalid("postgres needs `database`".to_owned()));
                    }
                    (_, database) => database,
                };
                config.user = Some(
                    raw.user
                        .ok_or_else(|| invalid(format!("{ename} needs `user`")))?,
                );
                config.port = Some(raw.port.unwrap_or(match engine {
                    Engine::Mysql => 3306,
                    Engine::Mssql => 1433,
                    Engine::Postgres
                    | Engine::Sqlite
                    | Engine::Duckdb
                    | Engine::Mongodb
                    | Engine::Surrealdb
                    | Engine::Redis
                    | Engine::Dynamodb => 5432,
                }));
                config.secret_source = secret_source(raw.secret).map_err(invalid)?;
                // SQL Server verifies on every host: its `off` is not plaintext, so the
                // local-host exception of Postgres and MySQL does not carry over.
                let local_off = engine != Engine::Mssql && is_local_host(&host);
                config
                    .set_tls(
                        local_off,
                        raw.tls,
                        raw.tls_ca,
                        raw.tls_cert,
                        raw.tls_key,
                        dir,
                    )
                    .map_err(invalid)?;
                for (key, value) in raw.settings.unwrap_or_default() {
                    if !valid_setting_name(&key) {
                        return Err(invalid(format!(
                            "settings name `{key}` is not an identifier"
                        )));
                    }
                    if RESERVED_SETTINGS
                        .iter()
                        .any(|r| r.eq_ignore_ascii_case(&key))
                    {
                        return Err(invalid(format!("settings name `{key}` is reserved")));
                    }
                    config.settings.insert(key, value);
                }
                config.host = Some(host);
            }
            // Every server setting lives in the URI, so each key names the form that replaces it.
            Engine::Mongodb => {
                for (key, present, hint) in [
                    ("host", raw.host.is_some(), "put it in `uri`"),
                    ("port", raw.port.is_some(), "put it in `uri` as host:port"),
                    (
                        "database",
                        raw.database.is_some(),
                        "put it in `uri` as /<database>",
                    ),
                    ("user", raw.user.is_some(), "put it in `uri` as user@"),
                    ("path", raw.path.is_some(), "use `uri`"),
                    (
                        "settings",
                        raw.settings.is_some(),
                        "put options in `uri` after ?",
                    ),
                    ("tls", raw.tls.is_some(), "put tls=true in `uri`"),
                    ("tls_ca", raw.tls_ca.is_some(), "put tlsCAFile in `uri`"),
                    (
                        "tls_cert",
                        raw.tls_cert.is_some(),
                        "put tlsCertificateKeyFile in `uri`",
                    ),
                    (
                        "tls_key",
                        raw.tls_key.is_some(),
                        "put tlsCertificateKeyFile in `uri`",
                    ),
                ] {
                    if present {
                        return Err(invalid(format!("`{key}` is not valid for mongodb; {hint}")));
                    }
                }
                let uri = raw
                    .uri
                    .ok_or_else(|| invalid("mongodb needs `uri`".to_owned()))?;
                let info = crate::mongo::uri::check(&uri).map_err(invalid)?;
                config.secret_source = match (&info.user, raw.secret) {
                    (Some(_), secret) => secret_source(secret).map_err(invalid)?,
                    (None, None) => SecretSource::None,
                    (None, Some(RawSecret::Word(w))) if w == "none" => SecretSource::None,
                    (None, Some(_)) => {
                        return Err(invalid("`secret` needs a user in `uri`".to_owned()));
                    }
                };
                config.uri = Some(uri);
                config.user = info.user;
                config.database = Some(info.database);
            }
            Engine::Surrealdb => {
                for (key, present) in [
                    ("path", raw.path.is_some()),
                    ("settings", raw.settings.is_some()),
                    ("tls_cert", raw.tls_cert.is_some()),
                    ("tls_key", raw.tls_key.is_some()),
                ] {
                    if present {
                        return Err(invalid(format!("`{key}` is not valid for surrealdb")));
                    }
                }
                let host = raw
                    .host
                    .ok_or_else(|| invalid("surrealdb needs `host`".to_owned()))?;
                let name = |key: &str, value: Option<String>| match value {
                    None => Err(invalid(format!("surrealdb needs `{key}`"))),
                    Some(v) if v.trim().is_empty() => {
                        Err(invalid(format!("`{key}` must not be empty")))
                    }
                    Some(v) => Ok(v),
                };
                config.namespace = Some(name("namespace", raw.namespace)?);
                config.database = Some(name("database", raw.database)?);
                // No user is an anonymous session, which signs in with nothing, so a level or
                // a secret there could only be a mistake. A blank user is no user.
                let user = raw.user.filter(|u| !u.trim().is_empty());
                (config.auth_level, config.secret_source) = match (&user, raw.auth_level) {
                    (Some(_), None) => {
                        return Err(invalid(
                            "surrealdb needs `auth_level` with `user`".to_owned(),
                        ));
                    }
                    (Some(_), Some(word)) => {
                        let level = AuthLevel::parse(&word).ok_or_else(|| {
                            invalid(format!(
                                "auth_level `{word}` must be \"root\", \"namespace\" or \"database\""
                            ))
                        })?;
                        (Some(level), secret_source(raw.secret).map_err(invalid)?)
                    }
                    (None, Some(_)) => {
                        return Err(invalid("`auth_level` needs `user`".to_owned()));
                    }
                    (None, None) => match raw.secret {
                        None => (None, SecretSource::None),
                        Some(RawSecret::Word(w)) if w == "none" => (None, SecretSource::None),
                        Some(_) => return Err(invalid("`secret` needs `user`".to_owned())),
                    },
                };
                config.user = user;
                config.port = Some(raw.port.unwrap_or(8000));
                config.tls = Some(
                    raw.tls
                        .as_deref()
                        .map_or(Ok(TlsMode::Verify), tls_mode)
                        .map_err(invalid)?,
                );
                config.tls_ca = raw.tls_ca.map(|p| dir.join(p));
                config.host = Some(host);
            }
            Engine::Redis => {
                for (key, present) in [
                    ("path", raw.path.is_some()),
                    ("settings", raw.settings.is_some()),
                ] {
                    if present {
                        return Err(invalid(format!("`{key}` is not valid for redis")));
                    }
                }
                let host = raw.host.unwrap_or_else(|| "127.0.0.1".to_owned());
                // The DB index is stored canonical so `007` and `7` are the same session.
                let database = raw.database.unwrap_or_else(|| "0".to_owned());
                let index = database.parse::<u32>().map_err(|_| {
                    invalid(format!(
                        "redis `database` must be a DB index (0, 1, ...), not `{database}`"
                    ))
                })?;
                config.database = Some(index.to_string());
                // A blank user is the server's `default` user.
                config.user = raw.user.filter(|u| !u.trim().is_empty());
                config.port = Some(raw.port.unwrap_or(6379));
                config.secret_source = secret_source(raw.secret).map_err(invalid)?;
                config
                    .set_tls(
                        is_local_host(&host),
                        raw.tls,
                        raw.tls_ca,
                        raw.tls_cert,
                        raw.tls_key,
                        dir,
                    )
                    .map_err(invalid)?;
                config.host = Some(host);
            }
            // The SDK finds the server from the region or `endpoint` and signs with an access
            // key, so the server keys of other engines each name the key that replaces them.
            Engine::Dynamodb => {
                for (key, present, hint) in [
                    ("host", raw.host.is_some(), "; use `endpoint`"),
                    ("port", raw.port.is_some(), "; use `endpoint`"),
                    ("database", raw.database.is_some(), ""),
                    ("user", raw.user.is_some(), "; use `access_key_id`"),
                    ("path", raw.path.is_some(), ""),
                    ("settings", raw.settings.is_some(), ""),
                    ("tls", raw.tls.is_some(), ""),
                    ("tls_ca", raw.tls_ca.is_some(), ""),
                    ("tls_cert", raw.tls_cert.is_some(), ""),
                    ("tls_key", raw.tls_key.is_some(), ""),
                ] {
                    if present {
                        return Err(invalid(format!("`{key}` is not valid for dynamodb{hint}")));
                    }
                }
                // Without an access key every credential comes from the AWS_* variables, so a
                // secret would have nothing to pair with; with one, an unsigned request is useless.
                config.secret_source = match (&raw.access_key_id, raw.secret) {
                    (Some(_), Some(RawSecret::Word(w))) if w == "none" => {
                        return Err(invalid(
                            "secret \"none\" is not valid for dynamodb: an unsigned request always fails"
                                .to_owned(),
                        ));
                    }
                    (Some(_), secret) => secret_source(secret).map_err(invalid)?,
                    (None, None) => SecretSource::None,
                    (None, Some(_)) => {
                        return Err(invalid(
                            "`secret` needs `access_key_id`; without it every credential comes from the AWS_* variables"
                                .to_owned(),
                        ));
                    }
                };
                for (key, value) in [
                    ("region", &raw.region),
                    ("access_key_id", &raw.access_key_id),
                ] {
                    if value.as_deref().is_some_and(|v| v.trim().is_empty()) {
                        return Err(invalid(format!("`{key}` must not be empty")));
                    }
                }
                if let Some(endpoint) = &raw.endpoint {
                    endpoint_rule(endpoint).map_err(|m| invalid(m.to_owned()))?;
                }
                config.region = raw.region;
                config.endpoint = raw.endpoint;
                config.access_key_id = raw.access_key_id;
                config.consistent_read = raw.consistent_read.unwrap_or(false);
            }
        }
        Ok(config)
    }

    // Postgres's TLS keys: `verify` unless written or `local_off`, a CA path, and a client
    // certificate only as a whole pair inside verified TLS, since it is useless otherwise.
    fn set_tls(
        &mut self,
        local_off: bool,
        tls: Option<String>,
        ca: Option<String>,
        cert: Option<String>,
        key: Option<String>,
        dir: &Path,
    ) -> Result<(), String> {
        let mode = match tls.as_deref() {
            Some(word) => tls_mode(word)?,
            None if local_off => TlsMode::Off,
            None => TlsMode::Verify,
        };
        self.tls = Some(mode);
        self.tls_ca = ca.map(|p| dir.join(p));
        match (cert, key) {
            (None, None) => {}
            (Some(_), None) => return Err("`tls_cert` needs `tls_key`".to_owned()),
            (None, Some(_)) => return Err("`tls_key` needs `tls_cert`".to_owned()),
            (Some(_), Some(_)) if mode != TlsMode::Verify => {
                return Err("`tls_cert` and `tls_key` need tls = \"verify\"".to_owned());
            }
            (Some(cert), Some(key)) => {
                self.tls_cert = Some(dir.join(cert));
                self.tls_key = Some(dir.join(key));
            }
        }
        Ok(())
    }

    /// The connection name from `dbento.toml`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The database engine.
    pub fn engine(&self) -> Engine {
        self.engine
    }

    /// Whether every statement runs read-only.
    pub fn read_only(&self) -> bool {
        self.read_only
    }

    /// The color tag, if set.
    pub fn color(&self) -> Option<Color> {
        self.color
    }

    /// The folder this connection sits in, if any.
    pub fn folder(&self) -> Option<&FolderPath> {
        self.folder.as_ref()
    }

    /// The own colour, else the colour inherited from the nearest folder that has one.
    pub fn effective_color(&self) -> Option<Color> {
        self.effective_color
    }

    /// Whether the effective colour is red, which marks production.
    pub fn is_production(&self) -> bool {
        self.effective_color == Some(Color::Red)
    }

    /// The configured statement timeout, if any.
    pub fn statement_timeout(&self) -> Option<Duration> {
        self.statement_timeout
    }

    /// Whether a session opened with either would be the same: everything but colour and folder.
    pub fn same_session(&self, other: &ConnectionConfig) -> bool {
        // Resolved values, so an absent key and its default compare equal; the name is the
        // caller's key and the file path as written resolves into `file_path`.
        self.engine == other.engine
            && self.read_only == other.read_only
            && self.statement_timeout == other.statement_timeout
            && self.secret_source == other.secret_source
            && self.file_path == other.file_path
            && self.host == other.host
            && self.port == other.port
            && self.database == other.database
            && self.user == other.user
            && self.tls == other.tls
            && self.tls_ca == other.tls_ca
            && self.tls_cert == other.tls_cert
            && self.tls_key == other.tls_key
            && self.settings == other.settings
            && self.read_only_user == other.read_only_user
            && self.uri == other.uri
            && self.namespace == other.namespace
            && self.auth_level == other.auth_level
            && self.region == other.region
            && self.endpoint == other.endpoint
            && self.access_key_id == other.access_key_id
            && self.consistent_read == other.consistent_read
    }

    /// `user@host:port/database` for Postgres, `user@host:port` plus any `/database` for
    /// MySQL and SQL Server, the path as written for SQLite and DuckDB, the URI for MongoDB,
    /// `host:port/namespace/database` after any `user@` for SurrealDB, and
    /// `host:port/<db index>` after any `user@` for Redis, and `<region> at <endpoint>` for
    /// DynamoDB, with `(region from environment)` for a missing region.
    pub fn display_target(&self) -> String {
        match self.engine {
            Engine::Sqlite | Engine::Duckdb => self.path.clone().unwrap_or_default(),
            Engine::Mysql | Engine::Mssql => {
                let default_port = if self.engine == Engine::Mysql {
                    3306
                } else {
                    1433
                };
                let mut target = format!(
                    "{}@{}:{}",
                    self.user.as_deref().unwrap_or_default(),
                    self.host.as_deref().unwrap_or_default(),
                    self.port.unwrap_or(default_port),
                );
                if let Some(db) = &self.database {
                    target.push('/');
                    target.push_str(db);
                }
                target
            }
            Engine::Postgres => format!(
                "{}@{}:{}/{}",
                self.user.as_deref().unwrap_or_default(),
                self.host.as_deref().unwrap_or_default(),
                self.port.unwrap_or(5432),
                self.database.as_deref().unwrap_or_default(),
            ),
            Engine::Mongodb => self.uri.clone().unwrap_or_default(),
            Engine::Surrealdb => format!(
                "{}{}:{}/{}/{}",
                self.user
                    .as_deref()
                    .map(|u| format!("{u}@"))
                    .unwrap_or_default(),
                self.host.as_deref().unwrap_or_default(),
                self.port.unwrap_or(8000),
                self.namespace.as_deref().unwrap_or_default(),
                self.database.as_deref().unwrap_or_default(),
            ),
            Engine::Redis => format!(
                "{}{}:{}/{}",
                self.user
                    .as_deref()
                    .map(|u| format!("{u}@"))
                    .unwrap_or_default(),
                self.host.as_deref().unwrap_or_default(),
                self.port.unwrap_or(6379),
                self.database.as_deref().unwrap_or("0"),
            ),
            Engine::Dynamodb => {
                let region = self
                    .region
                    .as_deref()
                    .unwrap_or("(region from environment)");
                match &self.endpoint {
                    Some(endpoint) => format!("{region} at {endpoint}"),
                    None => region.to_owned(),
                }
            }
        }
    }

    /// Where the secret comes from; SQLite connections have none.
    pub fn secret_source(&self) -> &SecretSource {
        &self.secret_source
    }

    /// The SQLite database file: the workspace directory joined with `path`.
    pub fn sqlite_path(&self) -> Option<&Path> {
        self.file_path().filter(|_| self.engine == Engine::Sqlite)
    }

    /// The database file of a file engine (SQLite or DuckDB), resolved against the workspace.
    pub fn file_path(&self) -> Option<&Path> {
        self.file_path.as_deref()
    }

    /// Postgres host, or a socket directory. `None` for SQLite and DuckDB.
    pub fn host(&self) -> Option<&str> {
        self.host.as_deref()
    }

    /// Postgres port, defaulting to 5432. `None` for SQLite and DuckDB.
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// Postgres database name. `None` for SQLite.
    pub fn database(&self) -> Option<&str> {
        self.database.as_deref()
    }

    /// The SurrealDB namespace.
    pub fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
    }

    /// The level a SurrealDB `user` signs in at; `None` when anonymous.
    pub fn auth_level(&self) -> Option<AuthLevel> {
        self.auth_level
    }

    /// Postgres user name. `None` for SQLite.
    pub fn user(&self) -> Option<&str> {
        self.user.as_deref()
    }

    /// Postgres TLS mode. `None` for SQLite.
    pub fn tls(&self) -> Option<TlsMode> {
        self.tls
    }

    /// Extra root certificate: the workspace directory joined with `tls_ca`.
    pub fn tls_ca(&self) -> Option<&Path> {
        self.tls_ca.as_deref()
    }

    /// Client certificate chain: the workspace directory joined with `tls_cert`.
    pub fn tls_cert(&self) -> Option<&Path> {
        self.tls_cert.as_deref()
    }

    /// Client private key: the workspace directory joined with `tls_key`.
    pub fn tls_key(&self) -> Option<&Path> {
        self.tls_key.as_deref()
    }

    /// The databases this parent exposes as children; `None` when it declares none.
    pub fn databases(&self) -> Option<&Databases> {
        self.databases.as_ref()
    }

    /// The SQL Server reader user that read-only work impersonates; `None` on other engines.
    pub fn read_only_user(&self) -> Option<&str> {
        self.read_only_user.as_deref()
    }

    /// How a read-only session on this connection is enforced, known without connecting.
    pub fn read_only_enforcement(&self) -> ReadOnlyEnforcement {
        // DynamoDB has no read-only session; only IAM, which Dbento cannot see, enforces it.
        if self.engine == Engine::Dynamodb
            || (self.engine == Engine::Mssql && self.read_only_user.is_none())
        {
            ReadOnlyEnforcement::BestEffort
        } else {
            ReadOnlyEnforcement::Database
        }
    }

    /// Postgres startup settings; empty for SQLite.
    pub fn settings(&self) -> &BTreeMap<String, String> {
        &self.settings
    }

    /// The MongoDB connection string as written (never holds a password).
    pub fn uri(&self) -> Option<&str> {
        self.uri.as_deref()
    }

    /// The DynamoDB region; `None` when it comes from the environment.
    pub fn region(&self) -> Option<&str> {
        self.region.as_deref()
    }

    /// The DynamoDB endpoint as written; `None` for the region's AWS endpoint.
    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }

    /// The DynamoDB access key id; `None` when credentials come from the environment.
    pub fn access_key_id(&self) -> Option<&str> {
        self.access_key_id.as_deref()
    }

    /// Whether DynamoDB reads are strongly consistent; `false` when absent.
    pub fn consistent_read(&self) -> bool {
        self.consistent_read
    }
}
