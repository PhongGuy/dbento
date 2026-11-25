//! A connection's keys as `dbento.toml` writes them, for building or editing one entry.

use crate::folder::FolderPath;
use crate::workspace::{AuthLevel, Color, Databases, Engine, SecretSource, SshAuth, TlsMode};

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
    /// `databases` (Postgres and SQL Server parents).
    pub databases: Option<Databases>,
    /// `secret` (Postgres and MySQL).
    pub secret: Option<SecretSource>,
    /// `read_only`.
    pub read_only: Option<bool>,
    /// `color`.
    pub color: Option<Color>,
    /// `folder`.
    pub folder: Option<FolderPath>,
    /// `statement_timeout`, as a duration such as `30s` or `1m 30s`.
    pub statement_timeout: Option<String>,
    /// `uri` (MongoDB).
    pub uri: Option<String>,
    /// `namespace` (SurrealDB).
    pub namespace: Option<String>,
    /// `auth_level` (SurrealDB).
    pub auth_level: Option<AuthLevel>,
    /// `region` (DynamoDB, S3).
    pub region: Option<String>,
    /// `endpoint` (DynamoDB, S3).
    pub endpoint: Option<String>,
    /// `access_key_id` (DynamoDB, S3).
    pub access_key_id: Option<String>,
    /// `consistent_read` (DynamoDB).
    pub consistent_read: Option<bool>,
    /// The `[connections.<name>.ssh]` sub-table.
    pub ssh: Option<SshSpec>,
    /// `virtual_hosted` (S3).
    pub virtual_hosted: Option<bool>,
    /// `buckets` (S3).
    pub buckets: Option<Vec<String>>,
}

/// A connection's `[connections.<name>.ssh]` keys as written; `None` means the key is absent.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshSpec {
    /// `host`: the SSH host or `~/.ssh/config` alias.
    pub host: String,
    /// `port`.
    pub port: Option<u16>,
    /// `user`.
    pub user: Option<String>,
    /// `auth`.
    pub auth: Option<SshAuth>,
    /// `identity_file`, as written.
    pub identity_file: Option<String>,
    /// `keepalive`.
    pub keepalive: Option<bool>,
}

impl SshSpec {
    /// A spec with only `host` set.
    pub fn new(host: impl Into<String>) -> SshSpec {
        SshSpec {
            host: host.into(),
            port: None,
            user: None,
            auth: None,
            identity_file: None,
            keepalive: None,
        }
    }
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
            databases: None,
            secret: None,
            read_only: None,
            color: None,
            folder: None,
            statement_timeout: None,
            uri: None,
            namespace: None,
            auth_level: None,
            region: None,
            endpoint: None,
            access_key_id: None,
            consistent_read: None,
            ssh: None,
            virtual_hosted: None,
            buckets: None,
        }
    }
}
