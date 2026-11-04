//! One statement's command as sent, and what its reply or failure means.

use std::time::Duration;

use bson::oid::ObjectId;
use bson::{Bson, Document, RawDocumentBuf, doc};
use mongodb::Database;
use mongodb::error::{Error, ErrorKind};
use mongodb::options::SelectionCriteria;

use super::parse::{MongoMethod, MongoOp};
use crate::session::{ExecuteError, Writes};

const MAX_TIME_EXPIRED: i32 = 50;
const UNAUTHORIZED: i32 = 13;

/// The command as sent: the statement timeout applied and, for `insertOne` and `insertMany`,
/// a fresh ObjectId at the front of each document without `_id`. `use` sends nothing, so it
/// has no command of its own.
pub(crate) fn outgoing(op: &MongoOp, timeout: Option<Duration>) -> Document {
    let mut command = match op {
        MongoOp::Use(_) => Document::new(),
        MongoOp::Method { method, command } => {
            let inserts = matches!(method, MongoMethod::InsertOne | MongoMethod::InsertMany);
            if inserts {
                with_ids(command)
            } else {
                command.clone()
            }
        }
        MongoOp::RunCommand(command) => command.clone(),
    };
    if let Some(timeout) = timeout {
        // The server reads 0 as no limit at all, so a sub-millisecond timeout rounds up; `connect`
        // has already refused one past `i32::MAX`.
        let limit = timeout.as_millis().max(1) as i64;
        let own = command.get("maxTimeMS").and_then(number);
        if !own.is_some_and(|n| n > 0.0 && n < limit as f64) {
            command.insert("maxTimeMS", limit);
        }
    }
    command
}

/// `outgoing`'s command, refused when it cannot be encoded. The driver would fail the same way
/// before sending, but its error is the one a reply that failed to decode gives after a write.
pub(crate) fn sendable(op: &MongoOp, timeout: Option<Duration>) -> Result<Document, ExecuteError> {
    let command = outgoing(op, timeout);
    match RawDocumentBuf::try_from(&command) {
        Ok(_) => Ok(command),
        Err(e) => Err(with_detail(
            ExecuteError::database("driver", e.to_string()),
            "the command could not be encoded, so nothing was sent",
        )),
    }
}

/// The URI's read preference, for a statement that cannot write. `run_command` must ignore a
/// client's default, so it reaches the driver only this way; anything else stays on the primary.
pub(crate) fn read_preference(database: &Database, writes: Writes) -> Option<SelectionCriteria> {
    let criteria = database
        .selection_criteria()
        .filter(|_| writes == Writes::No);
    criteria.cloned()
}

fn with_ids(command: &Document) -> Document {
    let mut command = command.clone();
    let documents = command.get_array_mut("documents").into_iter().flatten();
    for item in documents {
        if let Bson::Document(document) = item
            && !document.contains_key("_id")
        {
            // `insert` appends, and the id goes first as the shell puts it.
            let mut with = doc! { "_id": ObjectId::new() };
            with.extend(std::mem::take(document));
            *document = with;
        }
    }
    command
}

/// `n` for insert and delete, `nModified` plus `upserted`'s length for update, else 0.
pub(crate) fn rows_affected(reply: &Document) -> u64 {
    let count = |key| reply.get(key).and_then(number).map(|n| n as u64);
    match count("nModified") {
        Some(modified) => {
            let upserted = reply.get_array("upserted").map_or(0, Vec::len);
            modified.saturating_add(upserted as u64)
        }
        None => count("n").unwrap_or(0),
    }
}

/// The first `writeErrors` entry, then `writeConcernError`, as an error. These arrive in an
/// `ok: 1` reply, which the driver hands back as a success. Only a `writeErrors` code 50 with
/// nothing written is `Timeout`; anything else that may have landed stays `Database` and says so.
pub(crate) fn reply_failure(reply: &Document) -> Option<ExecuteError> {
    let write_error = reply
        .get_array("writeErrors")
        .ok()
        .and_then(|e| e.first())
        .and_then(Bson::as_document);
    let entry = write_error.or_else(|| reply.get_document("writeConcernError").ok())?;
    let code = entry.get("code").and_then(number).unwrap_or(0.0) as i32;
    let written = rows_affected(reply);
    if write_error.is_some() && code == MAX_TIME_EXPIRED && written == 0 {
        return Some(ExecuteError::Timeout);
    }
    let detail = match written {
        // A write concern fails after the write, and a reply like `create`'s has no count.
        0 if write_error.is_none() => Some(
            "the write may have landed, but the server did not confirm its write concern"
                .to_owned(),
        ),
        0 => None,
        1 => Some("the server reported 1 document written, which may be inexact".to_owned()),
        n => Some(format!(
            "the server reported {n} documents written, which may be inexact"
        )),
    };
    Some(ExecuteError::Database {
        code: code_text(entry.get_str("codeName").unwrap_or_default(), code),
        sqlstate: None,
        class: None,
        state: None,
        message: entry.get_str("errmsg").unwrap_or_default().to_owned(),
        detail,
        hint: None,
        position: None,
    })
}

/// `reply_failure` for a write method. A multi update can land documents and still reply with no
/// count, so a failed `updateMany` says so instead of reading as nothing written.
pub(crate) fn write_failure(op: &MongoOp, reply: &Document) -> Option<ExecuteError> {
    let failure = reply_failure(reply)?;
    let many = matches!(
        op,
        MongoOp::Method {
            method: MongoMethod::UpdateMany,
            ..
        }
    );
    Some(if many {
        with_detail(
            failure,
            "the update may have changed some documents before it failed",
        )
    } else {
        failure
    })
}

fn with_detail(mut failure: ExecuteError, text: &str) -> ExecuteError {
    if let ExecuteError::Database { detail, .. } = &mut failure {
        detail.get_or_insert_with(|| text.to_owned());
    }
    failure
}

/// Code 50 is `Timeout`; 13 on `writes != No` in a read-only session is `ReadOnlyViolation`;
/// a network or selection failure is `SessionClosed`; other commands are `Database`.
pub(crate) fn command_error(e: Error, writes: Writes, read_only: bool) -> ExecuteError {
    // Reads `e.kind`, not `e`, because `Error`'s `Display` dumps the server's reply as hex.
    match &*e.kind {
        ErrorKind::Command(c) if c.code == MAX_TIME_EXPIRED => ExecuteError::Timeout,
        ErrorKind::Command(c) if c.code == UNAUTHORIZED && read_only && writes != Writes::No => {
            ExecuteError::ReadOnlyViolation {
                message: c.message.clone(),
            }
        }
        ErrorKind::Command(c) => {
            ExecuteError::database(code_text(&c.code_name, c.code), c.message.clone())
        }
        ErrorKind::Io(_)
        | ErrorKind::ServerSelection { .. }
        | ErrorKind::ConnectionPoolCleared { .. }
        | ErrorKind::Shutdown => ExecuteError::SessionClosed,
        // The server has acted by the time its reply fails to decode.
        ErrorKind::InvalidResponse { .. } | ErrorKind::Bson(_) if writes != Writes::No => {
            with_detail(
                ExecuteError::database("driver", e.kind.to_string()),
                "the write may have landed, but its reply could not be read",
            )
        }
        other => ExecuteError::database("driver", other.to_string()),
    }
}

// `writeErrors` entries carry no `codeName`, so the decimal code stands in for it.
fn code_text(name: &str, code: i32) -> String {
    if name.is_empty() {
        code.to_string()
    } else {
        name.to_owned()
    }
}

// The server picks the numeric type of a count, and it may differ by version and size.
fn number(value: &Bson) -> Option<f64> {
    match value {
        Bson::Int32(n) => Some(f64::from(*n)),
        Bson::Int64(n) => Some(*n as f64),
        Bson::Double(n) => Some(*n),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
