//! The server's error object, read into a form the session can match on.

use super::decode::Wire;
use crate::session::ExecuteError;

/// The wire code of a query the server stopped at its own timeout.
const TIMED_OUT: i64 = -32004;
/// The wire code of a reply the server could not encode, such as one holding a regex.
const UNENCODABLE: i64 = -32700;
/// The words of `end` after a `query_cancel` or the server's own send timeout.
const STOPPED: &str = "stopped before it completed";

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

    /// `query_cancel`, or the server's send timeout, ended the stream.
    pub(crate) fn stopped(&self) -> bool {
        self.message.contains(STOPPED)
    }

    fn timed_out(&self) -> bool {
        self.code == TIMED_OUT || self.has("TimedOut")
    }

    /// An IAM refusal of what the login tried; a refused change of database shares the kind.
    fn refused(&self) -> bool {
        self.kind == "NotAllowed" && self.message.starts_with("IAM error")
    }
}

/// A field of an object value.
pub(crate) fn field<'w>(value: &'w Wire, name: &str) -> Option<&'w Wire> {
    match value {
        Wire::Object(entries) => entries
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value),
        Wire::None
        | Wire::Null
        | Wire::Undefined
        | Wire::Bool(_)
        | Wire::Int(_)
        | Wire::Float(_)
        | Wire::Decimal(_)
        | Wire::Text(_)
        | Wire::Bytes(_)
        | Wire::Datetime { .. }
        | Wire::Duration { .. }
        | Wire::Uuid(_)
        | Wire::Table(_)
        | Wire::Record { .. }
        | Wire::Range(_)
        | Wire::Geometry { .. }
        | Wire::Set(_)
        | Wire::File { .. }
        | Wire::Array(_)
        | Wire::Unknown { .. } => None,
    }
}

fn text(value: &Wire, name: &str) -> String {
    match field(value, name) {
        Some(Wire::Text(text)) => text.clone(),
        _ => String::new(),
    }
}

/// How a session reads a statement's error.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Reading {
    /// The session is read-only, so an IAM refusal is a refused write.
    pub(crate) read_only: bool,
    /// A VIEWER login made it so, which the message names.
    pub(crate) viewer: bool,
}

/// A statement's `finished` or `end` error, or a call-level one, as `ExecuteError`.
pub(crate) fn statement_error(e: &WireError, reading: Reading) -> ExecuteError {
    if e.timed_out() {
        return ExecuteError::Timeout;
    }
    if e.expired() {
        return ExecuteError::SessionEnded {
            reason: "the login's session on the server expired".to_owned(),
        };
    }
    if reading.read_only && e.refused() {
        let message = if reading.viewer {
            format!(
                "{} (this login is a VIEWER, so the session is read-only)",
                e.message
            )
        } else {
            e.message.clone()
        };
        return ExecuteError::ReadOnlyViolation { message };
    }
    if e.code == UNENCODABLE {
        return ExecuteError::Database {
            code: e.code.to_string(),
            sqlstate: None,
            class: None,
            state: None,
            message: "a value in this result has no CBOR form, usually a regex; the statement \
                      may have run"
                .to_owned(),
            detail: Some(e.kind.clone()),
            hint: Some(STRING_CAST.to_owned()),
            position: None,
        };
    }
    ExecuteError::Database {
        code: e.code.to_string(),
        sqlstate: None,
        class: None,
        state: None,
        message: e.message.clone(),
        detail: Some(e.kind.clone()),
        hint: None,
        position: None,
    }
}

/// What to do about a value Dbento cannot read or the server cannot send.
pub(crate) const STRING_CAST: &str = "cast the field to a string with <string>";

/// A call-level error for `text`, the unit sent: a parse error points into it.
pub(crate) fn call_error(e: &WireError, text: &str, reading: Reading) -> ExecuteError {
    let mut error = statement_error(e, reading);
    if let ExecuteError::Database { position, .. } = &mut error {
        *position = parse_position(&e.message, text);
    }
    error
}

/// The byte offset in `text` of a parse error's `--> [line:col]`, both 1-based and the column
/// in chars; a column past the end of its line clamps to that end.
fn parse_position(message: &str, text: &str) -> Option<usize> {
    let (_, rest) = message.split_once("--> [")?;
    let (at, _) = rest.split_once(']')?;
    let (line, column) = at.split_once(':')?;
    let (line, column): (usize, usize) = (line.parse().ok()?, column.parse().ok()?);
    let start = match line.checked_sub(2) {
        None if line == 1 => 0,
        None => return None,
        Some(before) => text.match_indices('\n').nth(before)?.0 + 1,
    };
    let line = text[start..].split('\n').next().unwrap_or_default();
    let offset = line
        .char_indices()
        .nth(column.checked_sub(1)?)
        .map_or(line.len(), |(at, _)| at);
    Some(start + offset)
}

#[cfg(test)]
mod tests;
