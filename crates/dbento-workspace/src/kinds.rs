//! The closed vocabularies a connection's engine answers with: what it supports, the language
//! its queries are written in, and the schema objects it can define.

/// What the session's engine supports.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// The engine has schemas inside a database.
    pub schemas: bool,
    /// A cancel stops the statement inside the engine.
    pub server_cancel: bool,
    /// The connection authenticates with a secret from the keychain or the environment.
    pub password: bool,
    /// The connection is a database file path.
    pub file: bool,
    /// The TLS keys this engine's form shows apply.
    pub tls: bool,
    /// Staged row edits can be applied.
    pub edit_rows: bool,
    /// EXPLAIN plans can be requested.
    pub explain: bool,
    /// A table has its CREATE text in the schema tree.
    pub table_definition: bool,
    /// The language the engine's query text is written in.
    pub language: QueryLanguage,
    /// Keys can be scanned and their values read in pages.
    pub key_browser: bool,
}

/// The language a connection's query text is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryLanguage {
    /// SQL.
    Sql,
    /// The mongosh statement subset.
    MongoShell,
    /// SurrealQL.
    SurrealQl,
    /// One redis-cli command per line.
    RedisCommands,
    /// DynamoDB's PartiQL dialect.
    PartiQl,
    /// S3 commands (`ls`, `stat`, `cat`), one per line or `;`.
    S3Cmd,
}

impl QueryLanguage {
    /// The name shells show for the language: "SQL", "MongoDB shell", "SurrealQL", "Redis
    /// commands", "PartiQL" or "S3 commands".
    pub fn name(self) -> &'static str {
        match self {
            Self::Sql => "SQL",
            Self::MongoShell => "MongoDB shell",
            Self::SurrealQl => "SurrealQL",
            Self::RedisCommands => "Redis commands",
            Self::PartiQl => "PartiQL",
            Self::S3Cmd => "S3 commands",
        }
    }

    /// The saved-query file extension, without the dot: "sql", "js", "surql", "redis",
    /// "partiql" or "s3".
    pub fn file_extension(self) -> &'static str {
        match self {
            Self::Sql => "sql",
            Self::MongoShell => "js",
            Self::SurrealQl => "surql",
            Self::RedisCommands => "redis",
            Self::PartiQl => "partiql",
            Self::S3Cmd => "s3",
        }
    }

    /// The marker that comments out the rest of a line, as Dbento's own splitter reads it:
    /// "--" for SQL and PartiQL, "//" for the MongoDB shell and SurrealQL, "#" for Redis
    /// commands and S3 commands. None would mean the language has no line comment.
    pub fn line_comment(self) -> Option<&'static str> {
        // SurrealQL also takes `--` and `#`, but only `//` still comments inside a script body.
        Some(match self {
            Self::Sql | Self::PartiQl => "--",
            Self::MongoShell | Self::SurrealQl => "//",
            Self::RedisCommands | Self::S3Cmd => "#",
        })
    }
}

/// How far a read-only session's refusal of writes can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnlyEnforcement {
    /// The database refuses every write: Postgres, MySQL, SQLite, DuckDB, SQL Server with
    /// read_only_user, MongoDB with a user whose privileges only read, and SurrealDB's fence for
    /// a VIEWER login.
    Database,
    /// SQL Server without read_only_user: Dbento's fence and refusals, with known holes; MongoDB
    /// with any other user, where Dbento refuses every statement it cannot prove reads; and
    /// SurrealDB's fence for root OWNER, EDITOR or anonymous, where stored code can reach what
    /// the fence cannot undo; and DynamoDB, where only IAM, out of Dbento's sight, refuses writes.
    BestEffort,
}

/// What kind of table constraint a key or check is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConstraintKind {
    /// A primary key.
    PrimaryKey,
    /// A unique key.
    Unique,
    /// A foreign key.
    ForeignKey,
    /// A check constraint.
    Check,
    /// An exclusion constraint (Postgres).
    Exclusion,
    /// Any other kind the engine reports.
    Other,
}

/// The kind of schema object a definition is asked for, as the schema tree tells them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefinitionKind {
    /// A table.
    Table,
    /// A view or materialized view.
    View,
    /// An index.
    Index,
    /// A constraint of this kind.
    Constraint(ConstraintKind),
    /// A trigger.
    Trigger,
    /// A function or procedure.
    Routine,
    /// A user-defined type.
    Type,
}
