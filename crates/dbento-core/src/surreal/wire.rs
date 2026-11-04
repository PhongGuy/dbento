//! The server's error object, read into a form the session can match on.

use super::decode::Wire;
use crate::session::ExecuteError;

/// One `error` object: its code, kind, nested detail kinds outermost first, and message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WireError {
    pub(crate) code: i64,
    pub(crate) kind: String,
    pub(crate) details: Vec<String>,
    pub(crate) message: String,
}

impl WireError {
    /// Reads an `error` object; a missing field reads as empty, as v3 omits some of them.
    pub(crate) fn of(error: &Wire) -> Self {
        // The details nest as `{ kind, details: { kind, .. } }`; walk the chain of kinds and
        // stop at the first level that is not one, such as a record id.
        let mut details = Vec::new();
        let mut at = field(error, "details");
        while let Some(detail) = at {
            match field(detail, "kind") {
                Some(Wire::Text(kind)) => details.push(kind.clone()),
                _ => break,
            }
            at = field(detail, "details");
        }
        WireError {
            code: match field(error, "code") {
                Some(Wire::Int(code)) => i64::try_from(*code).unwrap_or_default(),
                _ => 0,
            },
            kind: text(error, "kind"),
            details,
            message: text(error, "message"),
        }
    }

    fn has(&self, detail: &str) -> bool {
        self.details.iter().any(|d| d == detail)
    }

    /// `NotExecuted` or `Cancelled`: a statement a transaction undid, not an error.
    pub(crate) fn undone(&self) -> bool {
        self.kind == "Query" && (self.has("NotExecuted") || self.has("Cancelled"))
    }

    /// The server's session for this login is over; the socket answers nothing but `ping`.
    pub(crate) fn expired(&self) -> bool {
        self.has("SessionExpired")
    }
}

/// A field of an object value.
pub(crate) fn field<'w>(value: &'w Wire, name: &str) -> Option<&'w Wire> {
    match value {
        Wire::Object(entries) => entries
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value),
        _ => None,
    }
}

fn text(value: &Wire, name: &str) -> String {
    match field(value, name) {
        Some(Wire::Text(text)) => text.clone(),
        _ => String::new(),
    }
}

/// A statement's `finished` or `end` error, or a call-level one, as `ExecuteError`.
pub(crate) fn statement_error(e: &WireError) -> ExecuteError {
    ExecuteError::Database {
        code: e.code.to_string(),
        sqlstate: None,
        message: e.message.clone(),
        detail: Some(e.kind.clone()),
        hint: None,
        position: None,
    }
}

#[cfg(test)]
mod tests;
