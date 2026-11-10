//! The importer-neutral model: what a reader found, before it is planned against a workspace.

use crate::{Color, Engine, FolderPath, TlsMode};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

/// Everything one importer read: connection drafts, queries, skipped items and warnings.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportSet {
    connections: Vec<ImportedConnection>,
    queries: Vec<ImportedQuery>,
    skipped: Vec<Skipped>,
    warnings: Vec<Warning>,
}

impl ImportSet {
    /// The connections to import, in source order.
    pub fn connections(&self) -> &[ImportedConnection] {
        &self.connections
    }

    /// The query files to import.
    pub fn queries(&self) -> &[ImportedQuery] {
        &self.queries
    }

    /// The items the reader could not import, with the reason.
    pub fn skipped(&self) -> &[Skipped] {
        &self.skipped
    }

    /// What the reader changed or dropped while mapping.
    pub fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    pub(crate) fn add_connection(&mut self, c: ImportedConnection) {
        self.connections.push(c);
    }

    pub(crate) fn add_query(&mut self, q: ImportedQuery) {
        self.queries.push(q);
    }

    pub(crate) fn skip(&mut self, kind: ItemKind, subject: String, reason: SkipReason) {
        self.skipped.push(Skipped::new(kind, subject, reason));
    }

    pub(crate) fn warn(&mut self, subject: String, kind: WarningKind) {
        self.warnings.push(Warning::new(subject, kind));
    }
}

/// One source connection, with the Dbento name it will take and its draft.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedConnection {
    source_id: String,
    source_name: String,
    name: String,
    draft: ConnectionDraft,
}

impl ImportedConnection {
    pub(crate) fn new(
        source_id: String,
        source_name: String,
        name: String,
        draft: ConnectionDraft,
    ) -> Self {
        ImportedConnection {
            source_id,
            source_name,
            name,
            draft,
        }
    }

    /// The source tool's identifier for the connection.
    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    /// The connection's display name in the source tool.
    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    /// The Dbento connection name it will take.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The connection settings to write.
    pub fn draft(&self) -> &ConnectionDraft {
        &self.draft
    }
}

/// The Postgres fields of a draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PgDraft {
    pub(crate) host: String,
    pub(crate) port: Option<u16>,
    pub(crate) database: String,
    pub(crate) user: String,
    pub(crate) secret: SecretSetting,
    pub(crate) tls: Option<TlsMode>,
    pub(crate) tls_ca: Option<String>,
    pub(crate) tls_cert: Option<String>,
    pub(crate) tls_key: Option<String>,
    pub(crate) settings: BTreeMap<String, String>,
}

/// The MySQL fields of a draft; MySQL has no session settings and may name no database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MysqlDraft {
    pub(crate) host: String,
    pub(crate) port: Option<u16>,
    pub(crate) database: Option<String>,
    pub(crate) user: String,
    pub(crate) secret: SecretSetting,
    pub(crate) tls: Option<TlsMode>,
    pub(crate) tls_ca: Option<String>,
    pub(crate) tls_cert: Option<String>,
    pub(crate) tls_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DraftKind {
    Sqlite { path: String },
    Postgres(PgDraft),
    Mysql(MysqlDraft),
}

/// The settings of one connection to write into `dbento.toml`. Holds no secret.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionDraft {
    kind: DraftKind,
    read_only: bool,
    color: Option<Color>,
    folder: Option<FolderPath>,
}

impl ConnectionDraft {
    pub(crate) fn sqlite(path: String, read_only: bool) -> Self {
        ConnectionDraft {
            kind: DraftKind::Sqlite { path },
            read_only,
            color: None,
            folder: None,
        }
    }

    // The default port is stored as None so identity and output treat both spellings alike.
    pub(crate) fn postgres(mut pg: PgDraft, read_only: bool) -> Self {
        if pg.port == Some(5432) {
            pg.port = None;
        }
        ConnectionDraft {
            kind: DraftKind::Postgres(pg),
            read_only,
            color: None,
            folder: None,
        }
    }

    // As for Postgres, the default port 3306 is stored as None.
    pub(crate) fn mysql(mut my: MysqlDraft, read_only: bool) -> Self {
        if my.port == Some(3306) {
            my.port = None;
        }
        ConnectionDraft {
            kind: DraftKind::Mysql(my),
            read_only,
            color: None,
            folder: None,
        }
    }

    pub(crate) fn with_color(mut self, color: Option<Color>) -> Self {
        self.color = color;
        self
    }

    pub(crate) fn with_folder(mut self, folder: Option<FolderPath>) -> Self {
        self.folder = folder;
        self
    }

    fn pg(&self) -> Option<&PgDraft> {
        match &self.kind {
            DraftKind::Postgres(pg) => Some(pg),
            DraftKind::Sqlite { .. } | DraftKind::Mysql(_) => None,
        }
    }

    // The fields Postgres and MySQL share, borrowed from whichever draft this is.
    fn server(&self) -> Option<Server<'_>> {
        match &self.kind {
            DraftKind::Postgres(pg) => Some(Server {
                host: &pg.host,
                port: pg.port,
                database: Some(&pg.database),
                user: &pg.user,
                secret: pg.secret,
                tls: pg.tls,
                tls_ca: pg.tls_ca.as_deref(),
                tls_cert: pg.tls_cert.as_deref(),
                tls_key: pg.tls_key.as_deref(),
            }),
            DraftKind::Mysql(my) => Some(Server {
                host: &my.host,
                port: my.port,
                database: my.database.as_deref(),
                user: &my.user,
                secret: my.secret,
                tls: my.tls,
                tls_ca: my.tls_ca.as_deref(),
                tls_cert: my.tls_cert.as_deref(),
                tls_key: my.tls_key.as_deref(),
            }),
            DraftKind::Sqlite { .. } => None,
        }
    }

    /// The engine.
    pub fn engine(&self) -> Engine {
        match self.kind {
            DraftKind::Sqlite { .. } => Engine::Sqlite,
            DraftKind::Postgres(_) => Engine::Postgres,
            DraftKind::Mysql(_) => Engine::Mysql,
        }
    }

    /// Whether the connection is read-only.
    pub fn read_only(&self) -> bool {
        self.read_only
    }

    /// The colour tag the source gave the connection, if any.
    pub fn color(&self) -> Option<Color> {
        self.color
    }

    /// The folder the connection sits in, if any.
    pub fn folder(&self) -> Option<&FolderPath> {
        self.folder.as_ref()
    }

    /// The SQLite file path, as the reader found it.
    pub fn path(&self) -> Option<&str> {
        match &self.kind {
            DraftKind::Sqlite { path } => Some(path),
            DraftKind::Postgres(_) | DraftKind::Mysql(_) => None,
        }
    }

    /// The server host.
    pub fn host(&self) -> Option<&str> {
        self.server().map(|s| s.host)
    }

    /// The server port; None means the engine's default (5432 or 3306).
    pub fn port(&self) -> Option<u16> {
        self.server().and_then(|s| s.port)
    }

    /// The database; always set for Postgres, optional for MySQL.
    pub fn database(&self) -> Option<&str> {
        self.server().and_then(|s| s.database)
    }

    /// The server user.
    pub fn user(&self) -> Option<&str> {
        self.server().map(|s| s.user)
    }

    /// Where the secret comes from; None for SQLite.
    pub fn secret(&self) -> Option<SecretSetting> {
        self.server().map(|s| s.secret)
    }

    /// The TLS mode, when not left to the default.
    pub fn tls(&self) -> Option<TlsMode> {
        self.server().and_then(|s| s.tls)
    }

    /// The CA certificate path.
    pub fn tls_ca(&self) -> Option<&str> {
        self.server().and_then(|s| s.tls_ca)
    }

    /// The client certificate path.
    pub fn tls_cert(&self) -> Option<&str> {
        self.server().and_then(|s| s.tls_cert)
    }

    /// The client key path.
    pub fn tls_key(&self) -> Option<&str> {
        self.server().and_then(|s| s.tls_key)
    }

    /// Postgres session settings; empty for MySQL and SQLite.
    pub fn settings(&self) -> &BTreeMap<String, String> {
        static EMPTY: BTreeMap<String, String> = BTreeMap::new();
        self.pg().map_or(&EMPTY, |pg| &pg.settings)
    }
}

/// A borrowed view of the fields Postgres and MySQL drafts share.
struct Server<'a> {
    host: &'a str,
    port: Option<u16>,
    database: Option<&'a str>,
    user: &'a str,
    secret: SecretSetting,
    tls: Option<TlsMode>,
    tls_ca: Option<&'a str>,
    tls_cert: Option<&'a str>,
    tls_key: Option<&'a str>,
}

/// Where an imported connection's secret will come from.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretSetting {
    /// The OS keychain, set later with `dbento secret set`.
    Keychain,
    /// The connection needs no secret.
    None,
}

/// One query file to import.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedQuery {
    connection: Option<String>,
    relative: PathBuf,
    origin: PathBuf,
    content: Vec<u8>,
}

impl ImportedQuery {
    pub(crate) fn new(
        connection: Option<String>,
        relative: PathBuf,
        origin: PathBuf,
        content: Vec<u8>,
    ) -> Self {
        ImportedQuery {
            connection,
            relative,
            origin,
            content,
        }
    }

    /// The Dbento connection the query binds to, if any.
    pub fn connection(&self) -> Option<&str> {
        self.connection.as_deref()
    }

    /// The path the query takes, relative to its folder under `queries/`.
    pub fn relative(&self) -> &Path {
        &self.relative
    }

    /// The file it was read from.
    pub fn origin(&self) -> &Path {
        &self.origin
    }

    /// The file's bytes, unchanged.
    pub fn content(&self) -> &[u8] {
        &self.content
    }
}

/// An item the reader did not import.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    kind: ItemKind,
    subject: String,
    reason: SkipReason,
}

impl Skipped {
    pub(crate) fn new(kind: ItemKind, subject: String, reason: SkipReason) -> Self {
        Skipped {
            kind,
            subject,
            reason,
        }
    }

    /// What kind of item was skipped.
    pub fn kind(&self) -> ItemKind {
        self.kind
    }

    /// The source name or path of the item.
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// Why it was skipped.
    pub fn reason(&self) -> &SkipReason {
        &self.reason
    }
}

/// Something changed or dropped while importing an item.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    subject: String,
    kind: WarningKind,
}

impl Warning {
    pub(crate) fn new(subject: String, kind: WarningKind) -> Self {
        Warning { subject, kind }
    }

    /// The item the warning is about.
    pub fn subject(&self) -> &str {
        &self.subject
    }

    /// What happened.
    pub fn kind(&self) -> &WarningKind {
        &self.kind
    }
}

/// The kind of a source item.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    /// A connection definition.
    DataSource,
    /// A query file or console.
    Query,
}

impl ItemKind {
    /// The snake_case code used in reports.
    pub fn code(self) -> &'static str {
        match self {
            ItemKind::DataSource => "data_source",
            ItemKind::Query => "query",
        }
    }
}

/// Why an item was skipped.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// The driver is not one Dbento supports; holds the driver name.
    UnsupportedDriver(String),
    /// The connection has no URL.
    MissingUrl,
    /// The URL could not be parsed.
    InvalidUrl,
    /// The connection goes through an SSH tunnel.
    SshTunnel,
    /// The Postgres or MySQL connection names no user.
    NoUser,
    /// The URL lists more than one host.
    MultiHost,
    /// The connection uses a `pg_service.conf` service.
    ServiceFile,
    /// A path uses a macro that cannot be resolved; holds the macro.
    UnknownMacro(String),
    /// The SQLite database is in memory.
    InMemory,
    /// The file or folder is a symlink.
    Symlink,
    /// The path leaves its root folder.
    OutsideRoot,
    /// The query belongs to a source that was skipped.
    SourceSkipped,
    /// The target name is taken by a different connection.
    ConnectionConflict,
    /// The connection goes through a network proxy.
    Proxy,
    /// The connection belongs to a cloud workspace.
    CloudWorkspace,
    /// The saved query has no text.
    EmptyQuery,
    /// The connection uses a Unix socket, which Dbento's MySQL driver cannot.
    UnixSocket,
}

impl SkipReason {
    /// The snake_case code used in reports.
    pub fn code(&self) -> &'static str {
        match self {
            SkipReason::UnsupportedDriver(_) => "unsupported_driver",
            SkipReason::MissingUrl => "missing_url",
            SkipReason::InvalidUrl => "invalid_url",
            SkipReason::SshTunnel => "ssh_tunnel",
            SkipReason::NoUser => "no_user",
            SkipReason::MultiHost => "multi_host",
            SkipReason::ServiceFile => "service_file",
            SkipReason::UnknownMacro(_) => "unknown_macro",
            SkipReason::InMemory => "in_memory",
            SkipReason::Symlink => "symlink",
            SkipReason::OutsideRoot => "outside_root",
            SkipReason::SourceSkipped => "source_skipped",
            SkipReason::ConnectionConflict => "connection_conflict",
            SkipReason::Proxy => "proxy",
            SkipReason::CloudWorkspace => "cloud_workspace",
            SkipReason::EmptyQuery => "empty_query",
            SkipReason::UnixSocket => "unix_socket",
        }
    }

    /// The reason's payload, if it has one.
    pub fn detail(&self) -> Option<&str> {
        match self {
            SkipReason::UnsupportedDriver(s) | SkipReason::UnknownMacro(s) => Some(s),
            SkipReason::MissingUrl
            | SkipReason::InvalidUrl
            | SkipReason::SshTunnel
            | SkipReason::NoUser
            | SkipReason::MultiHost
            | SkipReason::ServiceFile
            | SkipReason::InMemory
            | SkipReason::Symlink
            | SkipReason::OutsideRoot
            | SkipReason::SourceSkipped
            | SkipReason::ConnectionConflict
            | SkipReason::Proxy
            | SkipReason::CloudWorkspace
            | SkipReason::EmptyQuery
            | SkipReason::UnixSocket => None,
        }
    }
}

/// What a warning reports.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarningKind {
    /// The URL held a password, which was dropped.
    PasswordInUrl,
    /// A URL parameter or driver property was dropped; holds its name.
    DroppedParameter(String),
    /// The TLS mode became stricter than the source's.
    TlsStricter {
        /// The source's mode.
        from: SourceTls,
    },
    /// The TLS mode became weaker than the source's.
    TlsWeaker {
        /// The source's mode.
        from: SourceTls,
    },
    /// The CA certificate was dropped.
    CaCertMissing,
    /// The client certificate and key were dropped.
    ClientCertMissing,
    /// The path only makes sense on this machine.
    MachineSpecificPath,
    /// The SQLite file does not exist here.
    FileMissing,
    /// The file is not valid UTF-8.
    NotUtf8,
    /// DataGrip object colours, or an unreadable `databaseColors.xml`, were not imported.
    ColorsNotImported,
    /// The query is bound by its folder.
    BoundByFolder,
    /// The connection uses an authentication model Dbento does not import; holds its name.
    AuthModel(String),
    /// The key file is absent, so no saved secret is read.
    KeyMissing,
    /// The saved password did not decrypt.
    SecretUnreadable,
    /// The connection's colour maps to no tag; holds the raw source value.
    ColorUnknown(String),
}

impl WarningKind {
    /// The snake_case code used in reports.
    pub fn code(&self) -> &'static str {
        match self {
            WarningKind::PasswordInUrl => "password_in_url",
            WarningKind::DroppedParameter(_) => "dropped_parameter",
            WarningKind::TlsStricter { .. } => "tls_stricter",
            WarningKind::TlsWeaker { .. } => "tls_weaker",
            WarningKind::CaCertMissing => "ca_cert_missing",
            WarningKind::ClientCertMissing => "client_cert_missing",
            WarningKind::MachineSpecificPath => "machine_specific_path",
            WarningKind::FileMissing => "file_missing",
            WarningKind::NotUtf8 => "not_utf8",
            WarningKind::ColorsNotImported => "colors_not_imported",
            WarningKind::BoundByFolder => "bound_by_folder",
            WarningKind::AuthModel(_) => "auth_model",
            WarningKind::KeyMissing => "key_missing",
            WarningKind::SecretUnreadable => "secret_unreadable",
            WarningKind::ColorUnknown(_) => "color_unknown",
        }
    }

    /// The parameter or auth model name, the raw colour value, or the source TLS code for a TLS
    /// change.
    pub fn detail(&self) -> Option<&str> {
        match self {
            WarningKind::DroppedParameter(s)
            | WarningKind::AuthModel(s)
            | WarningKind::ColorUnknown(s) => Some(s),
            WarningKind::TlsStricter { from } | WarningKind::TlsWeaker { from } => {
                Some(from.code())
            }
            WarningKind::PasswordInUrl
            | WarningKind::CaCertMissing
            | WarningKind::ClientCertMissing
            | WarningKind::MachineSpecificPath
            | WarningKind::FileMissing
            | WarningKind::NotUtf8
            | WarningKind::ColorsNotImported
            | WarningKind::BoundByFolder
            | WarningKind::KeyMissing
            | WarningKind::SecretUnreadable => None,
        }
    }
}

/// A source connection's TLS mode, in libpq `sslmode` terms.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceTls {
    /// `disable`.
    Disable,
    /// `allow`.
    Allow,
    /// `prefer`.
    Prefer,
    /// `require`.
    Require,
    /// `verify-ca`.
    VerifyCa,
    /// `verify-full`.
    VerifyFull,
    /// A value no mode matches.
    Unrecognised,
}

impl SourceTls {
    /// The `sslmode` spelling.
    pub fn code(self) -> &'static str {
        match self {
            SourceTls::Disable => "disable",
            SourceTls::Allow => "allow",
            SourceTls::Prefer => "prefer",
            SourceTls::Require => "require",
            SourceTls::VerifyCa => "verify-ca",
            SourceTls::VerifyFull => "verify-full",
            SourceTls::Unrecognised => "unrecognised",
        }
    }
}
