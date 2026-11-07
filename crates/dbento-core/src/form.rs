//! The connection form descriptor: which `dbento.toml` keys each engine's form shows.

use crate::workspace::Engine;

/// The engine-specific part of a connection form, in display order.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionForm {
    /// The titled groups of fields.
    pub sections: Vec<FormSection>,
    /// Help shown beside the read-only checkbox, when the engine's read-only has gaps.
    pub read_only_help: Option<&'static str>,
}

/// One titled group of fields.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormSection {
    /// The section heading.
    pub title: &'static str,
    /// The fields in display order.
    pub fields: Vec<FormField>,
}

/// One field, named by its `dbento.toml` key.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormField {
    /// The `dbento.toml` key the field writes.
    pub key: &'static str,
    /// The row label.
    pub label: &'static str,
    /// What kind of input the field takes.
    pub kind: FieldKind,
    /// Placeholder text for an empty input; empty when there is none.
    pub placeholder: &'static str,
    /// Whether the parser refuses the connection without this key.
    pub required: bool,
    /// Help shown under the field.
    pub help: Option<&'static str>,
}

/// The input a field takes.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    /// Free text.
    Text,
    /// A port number; `default` applies when the key is left out.
    Port {
        /// The port the parser uses when the key is absent.
        default: u16,
    },
    /// A file path, relative to the workspace.
    Path,
    /// The password, kept in the keychain rather than the file.
    Secret,
    /// The TLS mode.
    Tls,
    /// A connection string; it never holds the password.
    Uri,
}

const MYSQL_READ_ONLY_HELP: &str = "Read-only blocks writes in each statement's transaction, \
     but not GET_LOCK or writes to non-transactional tables. For production, use an account \
     with only SELECT.";

const DUCKDB_READ_ONLY_HELP: &str = "Read-only opens the file read-only with file access off, \
     so read_csv and parquet of local files do not work. While the app holds a writable DuckDB \
     connection, the CLI and MCP cannot open that file.";

/// The form for `engine`; it mirrors the parser's per-engine rules in `ConnectionConfig`.
pub fn connection_form(engine: Engine) -> ConnectionForm {
    match engine {
        Engine::Sqlite => ConnectionForm {
            sections: vec![section(
                "Connection",
                vec![field(
                    "path",
                    "File",
                    FieldKind::Path,
                    "path/to/database.sqlite",
                    true,
                )],
            )],
            read_only_help: None,
        },
        Engine::Postgres => {
            server_form((5432, "5432"), "e.g. postgres", true, "e.g. postgres", None)
        }
        // A MySQL connection may leave the database out and browse every schema it can see.
        Engine::Mysql => {
            let mut form = server_form(
                (3306, "3306"),
                "optional, the default schema",
                false,
                "e.g. root",
                Some(MYSQL_READ_ONLY_HELP),
            );
            form.sections[0].fields[2].help =
                Some("Leave empty to browse every database the account can see.");
            form
        }
        Engine::Duckdb => ConnectionForm {
            sections: vec![section(
                "Connection",
                vec![field(
                    "path",
                    "File",
                    FieldKind::Path,
                    "path/to/database.duckdb",
                    true,
                )],
            )],
            read_only_help: Some(DUCKDB_READ_ONLY_HELP),
        },
        Engine::Mssql => mssql_form(),
        Engine::Mongodb => mongodb_form(),
        Engine::Surrealdb => surrealdb_form(),
        Engine::Redis => redis_form(),
    }
}

const MSSQL_READ_ONLY_HELP: &str = "SQL Server has no read-only transaction. Without a reader \
     user, read-only is best effort; set one for production.";

/// SQL Server's own layout: its TLS `off` still encrypts the login, and read-only work can
/// impersonate a reader user, so both get a section with help of their own.
fn mssql_form() -> ConnectionForm {
    let help = |mut f: FormField, text: &'static str| {
        f.help = Some(text);
        f
    };
    ConnectionForm {
        sections: vec![
            section(
                "Connection",
                vec![
                    field("host", "Host", FieldKind::Text, "e.g. localhost", true),
                    field(
                        "port",
                        "Port",
                        FieldKind::Port { default: 1433 },
                        "1433",
                        false,
                    ),
                    help(
                        field("database", "Database", FieldKind::Text, "optional", false),
                        "Leave empty to use the login's default database.",
                    ),
                    field("user", "User", FieldKind::Text, "e.g. sa", true),
                ],
            ),
            section(
                "Secret",
                vec![field("secret", "Password", FieldKind::Secret, "", false)],
            ),
            section(
                "TLS",
                vec![help(
                    field("tls", "TLS", FieldKind::Tls, "", false),
                    "`off` encrypts the login against passive capture only; data is not \
                     encrypted.",
                )],
            ),
            section(
                "Read-only",
                vec![help(
                    field(
                        "read_only_user",
                        "Reader user",
                        FieldKind::Text,
                        "e.g. dbento_reader",
                        false,
                    ),
                    "A user without login in db_datareader and db_denydatawriter, with VIEW \
                     DEFINITION and DENY ALTER ANY USER; the login needs IMPERSONATE on it. A \
                     user that can write is refused at connect.",
                )],
            ),
        ],
        read_only_help: Some(MSSQL_READ_ONLY_HELP),
    }
}

const MONGODB_READ_ONLY_HELP: &str = "MongoDB has no read-only session. Dbento reads the user's \
     privileges at connect: a user with only read actions is enforced by the database, and any \
     other user is best effort, with its writes and unknown commands refused by Dbento.";

/// MongoDB's layout: the URI carries everything but the password.
fn mongodb_form() -> ConnectionForm {
    let mut uri = field(
        "uri",
        "URI",
        FieldKind::Uri,
        "mongodb://user@host:27017/database",
        true,
    );
    uri.help = Some("No password here; it goes in the keychain.");
    ConnectionForm {
        sections: vec![
            section("Connection", vec![uri]),
            section(
                "Secret",
                vec![field("secret", "Password", FieldKind::Secret, "", false)],
            ),
        ],
        read_only_help: Some(MONGODB_READ_ONLY_HELP),
    }
}

const SURREALDB_READ_ONLY_HELP: &str = "SurrealDB has no read-only session. Dbento runs each \
     statement in a transaction it cancels, so changes are undone. Stored code can still \
     advance sequences and, on servers that allow them, reach http:: and files; a root OWNER \
     or an anonymous session can also reach ALTER SYSTEM.";

/// SurrealDB's own layout: `namespace` sits beside `database`, and `user` is optional because
/// no user means an anonymous session.
fn surrealdb_form() -> ConnectionForm {
    let help = |mut f: FormField, text: &'static str| {
        f.help = Some(text);
        f
    };
    ConnectionForm {
        sections: vec![
            section(
                "Connection",
                vec![
                    field("host", "Host", FieldKind::Text, "e.g. localhost", true),
                    field(
                        "port",
                        "Port",
                        FieldKind::Port { default: 8000 },
                        "8000",
                        false,
                    ),
                    field("namespace", "Namespace", FieldKind::Text, "e.g. app", true),
                    field("database", "Database", FieldKind::Text, "e.g. main", true),
                    help(
                        field("user", "User", FieldKind::Text, "optional", false),
                        "Leave empty for anonymous access.",
                    ),
                    help(
                        field(
                            "auth_level",
                            "Auth level",
                            FieldKind::Text,
                            "root, namespace or database",
                            false,
                        ),
                        "root, namespace or database, as the user was defined.",
                    ),
                ],
            ),
            section(
                "Secret",
                vec![field("secret", "Password", FieldKind::Secret, "", false)],
            ),
            section("TLS", vec![field("tls", "TLS", FieldKind::Tls, "", false)]),
        ],
        read_only_help: Some(SURREALDB_READ_ONLY_HELP),
    }
}

const REDIS_READ_ONLY_HELP: &str = "Redis has no read-only session. At connect Dbento checks that \
     the user's ACL refuses every write, and otherwise refuses writes itself from the server's \
     command table. For a server-enforced reader: ACL SETUSER dbento_ro on >… ~* &* -@all +@read \
     +ping +echo +time +hello +select +command +multi +discard +client|setname +client|setinfo \
     +client|id +client|info";

/// Redis's layout: the server shape with nothing required, since the host defaults to the
/// local machine, a blank user is the `default` user and the database is a numbered index.
fn redis_form() -> ConnectionForm {
    ConnectionForm {
        sections: vec![
            section(
                "Connection",
                vec![
                    field("host", "Host", FieldKind::Text, "127.0.0.1", false),
                    field(
                        "port",
                        "Port",
                        FieldKind::Port { default: 6379 },
                        "6379",
                        false,
                    ),
                    field("database", "DB index", FieldKind::Text, "0", false),
                ],
            ),
            section(
                "Authentication",
                vec![
                    field("user", "User", FieldKind::Text, "default", false),
                    field("secret", "Password", FieldKind::Secret, "", false),
                ],
            ),
            section(
                "Options",
                vec![field("tls", "TLS", FieldKind::Tls, "", false)],
            ),
        ],
        read_only_help: Some(REDIS_READ_ONLY_HELP),
    }
}

/// The server engines' shared shape, varying in the port, database and user details.
fn server_form(
    (port, port_placeholder): (u16, &'static str),
    database_placeholder: &'static str,
    database_required: bool,
    user_placeholder: &'static str,
    read_only_help: Option<&'static str>,
) -> ConnectionForm {
    ConnectionForm {
        sections: vec![
            section(
                "Connection",
                vec![
                    field("host", "Host", FieldKind::Text, "e.g. localhost", true),
                    field(
                        "port",
                        "Port",
                        FieldKind::Port { default: port },
                        port_placeholder,
                        false,
                    ),
                    field(
                        "database",
                        "Database",
                        FieldKind::Text,
                        database_placeholder,
                        database_required,
                    ),
                ],
            ),
            section(
                "Authentication",
                vec![
                    field("user", "User", FieldKind::Text, user_placeholder, true),
                    field("secret", "Password", FieldKind::Secret, "", false),
                ],
            ),
            section(
                "Options",
                vec![field("tls", "TLS", FieldKind::Tls, "", false)],
            ),
        ],
        read_only_help,
    }
}

fn section(title: &'static str, fields: Vec<FormField>) -> FormSection {
    FormSection { title, fields }
}

fn field(
    key: &'static str,
    label: &'static str,
    kind: FieldKind,
    placeholder: &'static str,
    required: bool,
) -> FormField {
    FormField {
        key,
        label,
        kind,
        placeholder,
        required,
        help: None,
    }
}
