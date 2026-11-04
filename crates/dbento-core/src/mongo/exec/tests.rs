use std::time::Duration;

use bson::{Bson, doc};
use mongodb::error::{CommandError, ErrorKind};

use super::*;
use crate::mongo::parse::parse_script;

fn op(src: &str) -> MongoOp {
    parse_script(src).remove(0).unwrap().op
}

fn ms(n: u64) -> Option<Duration> {
    Some(Duration::from_millis(n))
}

// `CommandError` is non-exhaustive, so the only way to one is the shape the server sends.
fn server_error(code: i32, name: &str) -> mongodb::error::Error {
    let reply = doc! { "code": code, "codeName": name, "errmsg": "refused" };
    let command: CommandError = bson::deserialize_from_document(reply).unwrap();
    ErrorKind::Command(command).into()
}

#[test]
fn an_insert_gets_ids_first_and_keeps_those_it_has() {
    let out = outgoing(&op("db.t.insertMany([{a: 1}, {_id: 7, a: 2}])"), None);
    let docs = out.get_array("documents").unwrap();
    let first = docs[0].as_document().unwrap();
    assert_eq!(first.keys().collect::<Vec<_>>(), ["_id", "a"]);
    assert!(first.get_object_id("_id").is_ok());
    assert_eq!(docs[1].as_document().unwrap(), &doc! { "_id": 7, "a": 2 });
}

#[test]
fn a_run_command_insert_goes_as_written() {
    let parsed = op("db.runCommand({insert: 't', documents: [{a: 1}]})");
    let MongoOp::RunCommand(written) = &parsed else {
        panic!()
    };
    assert_eq!(&outgoing(&parsed, None), written);
}

#[test]
fn the_timeout_becomes_max_time_ms_unless_the_command_has_a_smaller_one() {
    let at = |src: &str, t| outgoing(&op(src), t).get("maxTimeMS").cloned();
    assert_eq!(at("db.t.drop()", None), None);
    assert_eq!(at("db.t.drop()", ms(1500)), Some(Bson::Int64(1500)));
    let own = |n: i32| format!("db.runCommand({{find: 't', maxTimeMS: {n}}})");
    assert_eq!(at(&own(5), ms(1500)), Some(Bson::Int32(5)));
    assert_eq!(at(&own(9000), ms(1500)), Some(Bson::Int64(1500)));
    assert_eq!(at(&own(0), ms(1500)), Some(Bson::Int64(1500)));
    assert_eq!(at(&own(5), None), Some(Bson::Int32(5)));
}

#[test]
fn a_replaced_max_time_ms_keeps_its_place() {
    let src = "db.runCommand({find: 't', maxTimeMS: 9000, limit: 1})";
    let out = outgoing(&op(src), ms(1500));
    let keys: Vec<_> = out.keys().map(String::as_str).collect();
    assert_eq!(keys, ["find", "maxTimeMS", "limit"]);
}

// `maxTimeMS: 0` is no limit at all.
#[test]
fn a_sub_millisecond_timeout_is_still_a_limit() {
    let out = outgoing(&op("db.t.drop()"), Some(Duration::from_micros(500)));
    assert_eq!(out.get("maxTimeMS"), Some(&Bson::Int64(1)));
}

#[test]
fn rows_affected_reads_the_reply() {
    assert_eq!(rows_affected(&doc! { "n": 3, "ok": 1 }), 3);
    assert_eq!(rows_affected(&doc! { "n": 3, "nModified": 2, "ok": 1 }), 2);
    let upsert = doc! { "n": 1, "upserted": [{ "index": 0, "_id": 5 }], "nModified": 0, "ok": 1 };
    assert_eq!(rows_affected(&upsert), 1);
    assert_eq!(rows_affected(&doc! { "ok": 1 }), 0);
    assert_eq!(rows_affected(&doc! { "n": Bson::Int64(4), "ok": 1 }), 4);
    assert_eq!(rows_affected(&doc! { "n": 4.0, "ok": 1 }), 4);
}

#[test]
fn a_write_error_in_an_ok_reply_is_a_failure() {
    assert!(reply_failure(&doc! { "n": 1, "ok": 1 }).is_none());
    let dup =
        doc! { "n": 1, "ok": 1, "writeErrors": [{ "index": 1, "code": 11000, "errmsg": "dup" }] };
    match reply_failure(&dup) {
        Some(ExecuteError::Database {
            code,
            message,
            detail,
            ..
        }) => {
            assert_eq!((code.as_str(), message.as_str()), ("11000", "dup"));
            assert_eq!(
                detail.as_deref(),
                Some("the server reported 1 document written, which may be inexact")
            );
        }
        other => panic!("{other:?}"),
    }
    let late = doc! { "ok": 1, "writeErrors": [{ "code": 50, "errmsg": "late" }] };
    assert!(matches!(reply_failure(&late), Some(ExecuteError::Timeout)));
    let wc = doc! { "n": 1, "ok": 1, "writeConcernError": { "code": 64, "codeName": "WriteConcernFailed", "errmsg": "wc" } };
    match reply_failure(&wc) {
        Some(ExecuteError::Database { code, .. }) => assert_eq!(code, "WriteConcernFailed"),
        other => panic!("{other:?}"),
    }
}

// A write concern fails after the write landed, so its code 50 is no statement timeout.
#[test]
fn a_write_concern_timeout_keeps_what_was_written() {
    let concern = doc! { "code": 50, "codeName": "MaxTimeMSExpired", "errmsg": "late" };
    let lagging = doc! { "n": 2, "ok": 1, "writeConcernError": concern.clone() };
    match reply_failure(&lagging) {
        Some(ExecuteError::Database { code, detail, .. }) => {
            assert_eq!(code, "MaxTimeMSExpired");
            assert_eq!(
                detail.as_deref(),
                Some("the server reported 2 documents written, which may be inexact")
            );
        }
        other => panic!("{other:?}"),
    }
    match reply_failure(&doc! { "ok": 1, "writeConcernError": concern }) {
        Some(ExecuteError::Database { detail, .. }) => {
            assert_eq!(
                detail.as_deref(),
                Some("the write may have landed, but the server did not confirm its write concern")
            );
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_timed_out_write_error_is_a_timeout_only_with_nothing_written() {
    let entry = doc! { "code": 50, "errmsg": "late" };
    let some = doc! { "n": 1, "ok": 1, "writeErrors": [entry.clone()] };
    match reply_failure(&some) {
        Some(ExecuteError::Database { code, detail, .. }) => {
            assert_eq!(code, "50");
            assert_eq!(
                detail.as_deref(),
                Some("the server reported 1 document written, which may be inexact")
            );
        }
        other => panic!("{other:?}"),
    }
    let none = doc! { "n": 0, "ok": 1, "writeErrors": [entry] };
    assert!(matches!(reply_failure(&none), Some(ExecuteError::Timeout)));
}

// `n` on an update counts matched documents; only `nModified` were written.
#[test]
fn the_count_in_a_failure_is_the_one_a_success_reports() {
    let update = doc! { "n": 3, "nModified": 2, "ok": 1, "writeErrors": [{ "code": 11000, "errmsg": "dup" }] };
    match reply_failure(&update) {
        Some(ExecuteError::Database { detail, .. }) => assert_eq!(
            detail.as_deref(),
            Some("the server reported 2 documents written, which may be inexact")
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_failure_with_nothing_written_has_no_detail() {
    let first = doc! { "n": 0, "ok": 1, "writeErrors": [{ "code": 11000, "errmsg": "dup" }] };
    match reply_failure(&first) {
        Some(ExecuteError::Database { detail, .. }) => assert_eq!(detail, None),
        other => panic!("{other:?}"),
    }
}

// A multi update can land documents and still reply `n: 0`, and only `updateMany` is that kind.
#[test]
fn a_failed_update_many_may_have_changed_documents_whatever_it_reports() {
    let reply = doc! {
        "n": 0, "nModified": 0, "ok": 1,
        "writeErrors": [{ "index": 0, "code": 11000, "errmsg": "dup" }],
    };
    let detail = |src: &str| match write_failure(&op(src), &reply) {
        Some(ExecuteError::Database { detail, .. }) => detail,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        detail("db.t.updateMany({}, {$set: {a: 1}})").as_deref(),
        Some("the update may have changed some documents before it failed")
    );
    assert_eq!(detail("db.t.updateOne({}, {$set: {a: 1}})"), None);
}

#[test]
fn a_failed_update_many_keeps_a_count_and_a_timeout() {
    let many = op("db.t.updateMany({}, {$set: {a: 1}})");
    let counted = doc! {
        "n": 2, "nModified": 2, "ok": 1, "writeErrors": [{ "code": 11000, "errmsg": "dup" }],
    };
    match write_failure(&many, &counted) {
        Some(ExecuteError::Database { detail, .. }) => assert_eq!(
            detail.as_deref(),
            Some("the server reported 2 documents written, which may be inexact")
        ),
        other => panic!("{other:?}"),
    }
    let late = doc! { "ok": 1, "writeErrors": [{ "code": 50, "errmsg": "late" }] };
    assert!(matches!(
        write_failure(&many, &late),
        Some(ExecuteError::Timeout)
    ));
    assert!(write_failure(&many, &doc! { "n": 2, "ok": 1 }).is_none());
}

#[test]
fn a_network_failure_ends_the_session() {
    let e = mongodb::error::Error::from(std::io::ErrorKind::ConnectionReset);
    assert!(matches!(
        command_error(e, Writes::No, false),
        ExecuteError::SessionClosed
    ));
}

#[test]
fn a_timeout_is_a_timeout_on_any_statement() {
    let e = server_error(50, "MaxTimeMSExpired");
    assert!(matches!(
        command_error(e, Writes::Yes, false),
        ExecuteError::Timeout
    ));
}

#[test]
fn an_unauthorized_write_in_a_read_only_session_is_a_violation() {
    for writes in [Writes::Yes, Writes::Unknown] {
        match command_error(server_error(13, "Unauthorized"), writes, true) {
            ExecuteError::ReadOnlyViolation { message } => assert_eq!(message, "refused"),
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn an_unauthorized_read_or_a_session_that_may_write_stays_a_database_error() {
    for (writes, read_only) in [(Writes::No, true), (Writes::Yes, false)] {
        match command_error(server_error(13, "Unauthorized"), writes, read_only) {
            ExecuteError::Database { code, message, .. } => {
                assert_eq!(
                    (code.as_str(), message.as_str()),
                    ("Unauthorized", "refused")
                );
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn a_reply_with_no_code_name_uses_the_number() {
    match command_error(server_error(11000, ""), Writes::Yes, false) {
        ExecuteError::Database { code, .. } => assert_eq!(code, "11000"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_unreadable_reply_may_follow_a_write_but_not_a_read() {
    let unreadable =
        || -> mongodb::error::Error { ErrorKind::from(doc! {}.get_str("a").unwrap_err()).into() };
    let detail = |writes| match command_error(unreadable(), writes, false) {
        ExecuteError::Database { code, detail, .. } => {
            assert_eq!(code, "driver");
            detail
        }
        other => panic!("{other:?}"),
    };
    let landed = Some("the write may have landed, but its reply could not be read".to_owned());
    assert_eq!(detail(Writes::Yes), landed);
    assert_eq!(detail(Writes::Unknown), landed);
    assert_eq!(detail(Writes::No), None);
}

#[test]
fn any_other_driver_failure_names_the_driver() {
    let e = mongodb::error::Error::custom("odd");
    match command_error(e, Writes::No, false) {
        ExecuteError::Database { code, .. } => assert_eq!(code, "driver"),
        other => panic!("{other:?}"),
    }
}
