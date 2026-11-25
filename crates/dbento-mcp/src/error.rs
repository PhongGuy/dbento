//! Tool failures as an AI client sees them: an `isError` result with a fixed code.

use dbento_core::{
    ConnectError, Engine, ExecuteError, HostKeyProblem, IntrospectError, OpenError, SecretError,
};
use rmcp::ErrorData;
use rmcp::handler::server::tool::IntoCallToolResult;
use rmcp::model::{CallToolResponse, CallToolResult, ContentBlock};
use serde_json::{Map, Value, json};

use crate::fence::fence;

/// A failed tool call: a stable `code` the client can branch on and a human `message`.
#[derive(Debug)]
pub(crate) struct ToolError {
    code: &'static str,
    message: String,
    sqlstate: Option<String>,
    error_number: Option<String>,
    class: Option<u8>,
    state: Option<u8>,
    request_id: Option<String>,
    reason: Option<String>,
}

impl ToolError {
    /// A failure with `code` and `message` and no SQLSTATE.
    pub(crate) fn new(code: &'static str, message: String) -> Self {
        ToolError {
            code,
            message,
            sqlstate: None,
            error_number: None,
            class: None,
            state: None,
            request_id: None,
            reason: None,
        }
    }

    /// The workspace stopped loading between calls.
    pub(crate) fn from_open(e: &OpenError) -> Self {
        ToolError::new("workspace", e.to_string())
    }

    /// No exposed connection is named `name`; an unexposed one reads exactly like an absent one.
    pub(crate) fn no_connection(name: &str) -> Self {
        ToolError::new(
            "no_connection",
            format!("no connection named {name:?}; call list_connections"),
        )
    }

    /// `name` is a connection but declares no databases of its own, as a child never does.
    pub(crate) fn not_a_parent(name: &str) -> Self {
        ToolError::new(
            "not_a_parent",
            format!("connection {name:?} declares no databases; call list_connections"),
        )
    }

    /// The connection's secret could not be resolved.
    pub(crate) fn from_secret(e: &SecretError) -> Self {
        ToolError::new("secret", e.to_string())
    }

    /// The session did not open; a missing SQLite file counts as no connection. No message
    /// carries a local path, so an SSH failure whose core text can name a known_hosts, identity
    /// or ssh config file is reworded to its host, port, line and fingerprint, or a fixed text.
    pub(crate) fn from_connect(e: &ConnectError) -> Self {
        match e {
            ConnectError::NotFound { .. } => ToolError::new(
                "no_connection",
                "the connection's database file does not exist".to_owned(),
            ),
            ConnectError::SshHostKeyUnknown { .. } => ToolError::new("ssh_host_key", e.to_string()),
            ConnectError::SshHostKeyChanged {
                host,
                port,
                fingerprint,
                line,
                problem,
                ..
            } => {
                let verb = match problem {
                    HostKeyProblem::Changed => "differs from",
                    HostKeyProblem::Revoked => "is revoked at",
                    HostKeyProblem::Unreadable => "cannot be read at",
                    _ => "is refused at",
                };
                ToolError::new(
                    "ssh_host_key",
                    format!(
                        "the SSH host key of {host}:{port} {verb} line {line} of a known_hosts \
                         file ({fingerprint}); the user must resolve it"
                    ),
                )
            }
            ConnectError::SshConnect { address, .. } => {
                ToolError::new("ssh", format!("cannot reach the SSH server {address}"))
            }
            ConnectError::SshAuth { .. } => ToolError::new(
                "ssh",
                "SSH login to this connection's server failed; run `dbento query` on it or open \
                 it in the app to see which methods were tried"
                    .to_owned(),
            ),
            ConnectError::SshUnsupported { .. } => ToolError::new(
                "ssh",
                "this connection's SSH settings use something Dbento cannot do yet; run \
                 `dbento query` on it to see what"
                    .to_owned(),
            ),
            ConnectError::SshTunnel { .. } => ToolError::new("ssh", e.to_string()),
            _ => ToolError::new("connect", e.to_string()),
        }
    }

    /// A statement or catalog query failed; only a database error carries a SQLSTATE. An
    /// engine with its own SQLSTATE (MySQL) also reports its native code as `error_number`,
    /// and SQL Server, which has none, reports its number there with `class` and `state`.
    pub(crate) fn from_execute(e: &ExecuteError) -> Self {
        let code = match e {
            ExecuteError::Database {
                code,
                message,
                sqlstate,
                class,
                state,
                ..
            } => {
                return ToolError {
                    code: "database",
                    message: message.clone(),
                    sqlstate: sqlstate
                        .clone()
                        .or_else(|| class.is_none().then(|| code.clone())),
                    error_number: (sqlstate.is_some() || class.is_some()).then(|| code.clone()),
                    class: *class,
                    state: *state,
                    request_id: None,
                    reason: None,
                };
            }
            ExecuteError::ReadOnlyViolation { .. } => {
                return ToolError::new(
                    "read_only",
                    format!("{e}; writes and DDL go through the `execute` tool"),
                );
            }
            ExecuteError::Timeout => "timeout",
            ExecuteError::Cancelled => "cancelled",
            ExecuteError::TransactionControl { .. } => "transaction_control",
            ExecuteError::Unsupported { .. } => "unsupported",
            ExecuteError::Syntax { .. } => "syntax",
            ExecuteError::SessionClosed
            | ExecuteError::SessionEnded { .. }
            | ExecuteError::StoppedAtDrainCap => "session_closed",
            _ => "internal",
        };
        ToolError::new(code, e.to_string())
    }

    /// As `from_execute`, with `engine`'s own words for a stop, which say what the stop kept.
    pub(crate) fn from_execute_on(e: &ExecuteError, engine: Engine) -> Self {
        let mut error = ToolError::from_execute(e);
        if matches!(
            e,
            ExecuteError::StoppedAtDrainCap | ExecuteError::Timeout | ExecuteError::Cancelled
        ) {
            error.message = e.message_for(engine);
        }
        error
    }

    /// An introspection failed; a catalog answer of the wrong shape is still the database's.
    pub(crate) fn from_introspect(e: &IntrospectError) -> Self {
        match e {
            IntrospectError::Execute(e) => ToolError::from_execute(e),
            _ => ToolError::new("database", e.to_string()),
        }
    }

    /// The SQLSTATE or SQLite extended result code of a `database` error.
    #[cfg(test)]
    pub(crate) fn sqlstate(&self) -> Option<&str> {
        self.sqlstate.as_deref()
    }

    /// The same failure naming the approval request it concerns.
    pub(crate) fn with_request_id(mut self, id: &str) -> Self {
        self.request_id = Some(id.to_owned());
        self
    }

    /// The same denial carrying the reason the user typed, when there is one.
    pub(crate) fn with_reason(mut self, reason: Option<String>) -> Self {
        self.reason = reason;
        self
    }

    /// The error code, such as `"workspace"` or `"internal"`.
    pub(crate) fn code(&self) -> &str {
        self.code
    }

    /// The human-readable message.
    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    /// The `{"error": {...}}` object, with `sqlstate`, `error_number`, `class`, `state`,
    /// `request_id` and `reason` only when set.
    fn to_value(&self) -> Value {
        let mut error = Map::new();
        error.insert("code".into(), json!(self.code));
        error.insert("message".into(), json!(self.message));
        if let Some(sqlstate) = &self.sqlstate {
            error.insert("sqlstate".into(), json!(sqlstate));
        }
        if let Some(number) = &self.error_number {
            error.insert("error_number".into(), json!(number));
        }
        if let Some(class) = self.class {
            error.insert("class".into(), json!(class));
        }
        if let Some(state) = self.state {
            error.insert("state".into(), json!(state));
        }
        if let Some(id) = &self.request_id {
            error.insert("request_id".into(), json!(id));
        }
        if let Some(reason) = &self.reason {
            error.insert("reason".into(), json!(reason));
        }
        json!({ "error": error })
    }
}

impl IntoCallToolResult for ToolError {
    fn into_call_tool_result(self) -> Result<CallToolResponse, ErrorData> {
        // A message can quote database text, so the readable copy is fenced like any result.
        let value = self.to_value();
        let mut result = CallToolResult::structured_error(value.clone());
        result.content = vec![ContentBlock::text(fence(&value.to_string()))];
        Ok(result.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn every_core_error_maps_to_its_fixed_code() {
        let cases = [
            (
                ToolError::from_connect(&ConnectError::NotFound {
                    path: "/x.db".into(),
                }),
                "no_connection",
            ),
            (
                ToolError::from_connect(&ConnectError::Open {
                    message: "refused".into(),
                }),
                "connect",
            ),
            (
                ToolError::from_connect(&ConnectError::Tls {
                    host: "h".into(),
                    message: "bad".into(),
                }),
                "connect",
            ),
            (
                ToolError::from_connect(&ConnectError::InvalidTimeout {
                    timeout: Duration::ZERO,
                }),
                "connect",
            ),
            (
                ToolError::from_connect(&ConnectError::Unsupported { reason: "x".into() }),
                "connect",
            ),
            (
                ToolError::from_secret(&SecretError::Missing {
                    variable: "V".into(),
                }),
                "secret",
            ),
            (
                ToolError::from_secret(&SecretError::Unavailable {
                    message: "locked".into(),
                }),
                "secret",
            ),
            (
                ToolError::from_execute(&ExecuteError::database("42P01", "no")),
                "database",
            ),
            (ToolError::from_execute(&ExecuteError::Timeout), "timeout"),
            (
                ToolError::from_execute(&ExecuteError::Cancelled),
                "cancelled",
            ),
            (
                ToolError::from_execute(&ExecuteError::ReadOnlyViolation {
                    message: "ro".into(),
                }),
                "read_only",
            ),
            (
                ToolError::from_execute(&ExecuteError::TransactionControl {
                    reason: "tc".into(),
                }),
                "transaction_control",
            ),
            (
                ToolError::from_execute(&ExecuteError::Unsupported {
                    reason: "copy".into(),
                }),
                "unsupported",
            ),
            (
                ToolError::from_execute(&ExecuteError::SessionClosed),
                "session_closed",
            ),
            (
                ToolError::from_execute(&ExecuteError::StoppedAtDrainCap),
                "session_closed",
            ),
            (
                ToolError::from_execute(&ExecuteError::SessionEnded {
                    reason: "the connection to the server was lost".into(),
                }),
                "session_closed",
            ),
            (
                ToolError::from_introspect(&IntrospectError::Execute(ExecuteError::Timeout)),
                "timeout",
            ),
            (
                ToolError::from_introspect(&IntrospectError::Unexpected {
                    message: "shape".into(),
                }),
                "database",
            ),
        ];
        for (e, code) in cases {
            assert_eq!(e.code(), code, "{}", e.message());
        }
    }

    #[test]
    fn ssh_errors_map_to_their_own_codes() {
        let key =
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEpl7XIrO05PhGnrhKwH5QLVhB9hgippRySbUyJFSjRd";
        let cases = [
            (
                ConnectError::SshHostKeyUnknown {
                    host: "h".into(),
                    port: 22,
                    key: dbento_core::HostKey::from_openssh("h", 22, key).unwrap(),
                },
                "ssh_host_key",
            ),
            (
                ConnectError::SshHostKeyChanged {
                    host: "h".into(),
                    port: 22,
                    fingerprint: "SHA256:x".into(),
                    file: "/k".into(),
                    line: 1,
                    problem: dbento_core::HostKeyProblem::Changed,
                },
                "ssh_host_key",
            ),
            (
                ConnectError::SshConnect {
                    address: "h:22".into(),
                    message: "m".into(),
                },
                "ssh",
            ),
            (
                ConnectError::SshAuth {
                    message: "m".into(),
                },
                "ssh",
            ),
            (
                ConnectError::SshUnsupported {
                    message: "m".into(),
                },
                "ssh",
            ),
            (
                ConnectError::SshTunnel {
                    message: "m".into(),
                },
                "ssh",
            ),
        ];
        for (e, code) in cases {
            let t = ToolError::from_connect(&e);
            assert_eq!(t.code(), code, "{e}");
        }
    }

    #[test]
    fn ssh_messages_leave_out_local_paths() {
        let changed = ToolError::from_connect(&ConnectError::SshHostKeyChanged {
            host: "bastion".into(),
            port: 2222,
            fingerprint: "SHA256:abc".into(),
            file: "/home/u/.ssh/known_hosts".into(),
            line: 7,
            problem: dbento_core::HostKeyProblem::Revoked,
        });
        for part in ["bastion:2222", "SHA256:abc", "line 7", "revoked"] {
            assert!(
                changed.message().contains(part),
                "{part}: {}",
                changed.message()
            );
        }
        let auth = ToolError::from_connect(&ConnectError::SshAuth {
            message: "key /home/u/.ssh/id_ed25519: rejected".into(),
        });
        let unsupported = ToolError::from_connect(&ConnectError::SshUnsupported {
            message: "Match in /home/u/.ssh/config is not supported yet".into(),
        });
        let connect = ToolError::from_connect(&ConnectError::SshConnect {
            address: "bastion:22".into(),
            message: "cannot read /home/u/.ssh/config: denied".into(),
        });
        assert!(
            connect.message().contains("bastion:22"),
            "{}",
            connect.message()
        );
        for t in [&changed, &auth, &unsupported, &connect] {
            assert!(!t.message().contains("/home"), "{}", t.message());
        }
    }

    #[test]
    fn every_unsupported_ssh_setting_reads_one_fixed_text() {
        for message in [
            "SSH tunnels for MySQL are not available yet",
            "no home directory to read SSH settings from",
            "Match in /home/u/.ssh/config is not supported yet",
        ] {
            let t = ToolError::from_connect(&ConnectError::SshUnsupported {
                message: message.into(),
            });
            assert_eq!(
                (t.code(), t.message()),
                (
                    "ssh",
                    "this connection's SSH settings use something Dbento cannot do yet; run `dbento query` on it to see what"
                )
            );
        }
    }

    #[test]
    fn a_database_error_keeps_its_code_and_a_read_only_one_points_at_execute() {
        let e = ToolError::from_execute(&ExecuteError::database(
            "42P01",
            "relation \"nope\" does not exist",
        ));
        assert_eq!(e.sqlstate(), Some("42P01"));
        assert!(e.message().contains("nope"), "{}", e.message());
        let e = ToolError::from_execute(&ExecuteError::ReadOnlyViolation {
            message: "cannot execute INSERT in a read-only transaction".into(),
        });
        assert_eq!(e.sqlstate(), None);
        assert!(e.message().contains("INSERT"), "{}", e.message());
        assert!(e.message().contains("execute"), "{}", e.message());
    }

    #[test]
    fn an_error_without_its_own_sqlstate_has_no_error_number() {
        let v = ToolError::from_execute(&ExecuteError::database("42P01", "no")).to_value();
        assert_eq!(v["error"]["sqlstate"], "42P01", "{v}");
        assert!(v["error"].get("error_number").is_none(), "{v}");
        let v = ToolError::from_execute(&ExecuteError::Timeout).to_value();
        assert!(v["error"].get("error_number").is_none(), "{v}");
    }

    #[test]
    fn a_stop_is_worded_for_its_engine_and_a_server_free_cap_pins_mysql() {
        let on = |e: &ExecuteError, engine| ToolError::from_execute_on(e, engine);
        let cap = ExecuteError::StoppedAtDrainCap;
        assert!(
            on(&cap, Engine::Mysql)
                .message()
                .contains("may have kept part of its changes")
        );
        assert_eq!(on(&cap, Engine::Postgres).message(), cap.to_string());
        assert_eq!(on(&cap, Engine::Mysql).code(), "session_closed");
        let cancelled = on(&ExecuteError::Cancelled, Engine::Mssql);
        assert_eq!(cancelled.code(), "cancelled");
        assert!(cancelled.message().contains("may have kept"));
    }
}
