use super::*;

fn object(entries: &[(&str, Wire)]) -> Wire {
    Wire::Object(
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect(),
    )
}

fn text_of(s: &str) -> Wire {
    Wire::Text(s.to_owned())
}

fn wire(code: i64, kind: &str, details: &[&str], message: &str) -> WireError {
    WireError {
        code,
        kind: kind.to_owned(),
        details: details.iter().map(|d| (*d).to_owned()).collect(),
        message: message.to_owned(),
    }
}

#[test]
fn an_error_object_reads_every_level_of_its_details() {
    let error = object(&[
        ("code", Wire::Int(-32000)),
        (
            "details",
            object(&[
                ("details", object(&[("kind", text_of("SessionExpired"))])),
                ("kind", text_of("Auth")),
            ]),
        ),
        ("kind", text_of("NotAllowed")),
        ("message", text_of("The session has expired")),
    ]);
    let e = WireError::of(&error);
    assert_eq!(
        e,
        wire(
            -32000,
            "NotAllowed",
            &["Auth", "SessionExpired"],
            "The session has expired"
        )
    );
    assert!(e.expired());
}

#[test]
fn details_that_are_not_a_chain_of_kinds_stop_the_walk() {
    let error = object(&[
        ("code", Wire::Int(-32000)),
        (
            "details",
            object(&[
                ("details", object(&[("id", text_of("w:1"))])),
                ("kind", text_of("Record")),
            ]),
        ),
        ("kind", text_of("AlreadyExists")),
        ("message", text_of("exists")),
    ]);
    assert_eq!(WireError::of(&error).details, ["Record"]);
    assert_eq!(WireError::of(&Wire::Null).code, 0);
}

#[test]
fn not_executed_and_cancelled_are_undone_and_nothing_else_is() {
    let undone = |details: &[&str]| wire(-32003, "Query", details, "x").undone();
    assert!(undone(&["NotExecuted"]));
    assert!(undone(&["Cancelled"]));
    assert!(!undone(&["TimedOut"]));
    assert!(!wire(-32000, "Internal", &["NotExecuted"], "x").undone());
}

const QUIET: Reading = Reading {
    read_only: false,
    viewer: false,
};

#[test]
fn an_iam_refusal_is_a_violation_only_in_a_read_only_session() {
    let iam = wire(
        -32002,
        "NotAllowed",
        &["Auth", "NotAllowed"],
        "IAM error: Not enough permissions to perform this action",
    );
    assert!(matches!(
        statement_error(&iam, QUIET),
        ExecuteError::Database { .. }
    ));
    let read_only = Reading {
        read_only: true,
        viewer: false,
    };
    assert!(matches!(
        statement_error(&iam, read_only),
        ExecuteError::ReadOnlyViolation { .. }
    ));
    let viewer = Reading {
        read_only: true,
        viewer: true,
    };
    match statement_error(&iam, viewer) {
        ExecuteError::ReadOnlyViolation { message } => {
            assert!(message.contains("VIEWER"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    // Refusing a change of database is not a refused write.
    let moved = wire(-32000, "NotAllowed", &[], "You don't have permission");
    assert!(matches!(
        statement_error(&moved, read_only),
        ExecuteError::Database { .. }
    ));
}

#[test]
fn the_error_table_maps_each_wire_error() {
    let timed = wire(-32004, "Query", &["TimedOut"], "exceeded the timeout");
    assert!(matches!(
        statement_error(&timed, QUIET),
        ExecuteError::Timeout
    ));
    let timed = wire(0, "Query", &["TimedOut"], "exceeded the timeout");
    assert!(matches!(
        statement_error(&timed, QUIET),
        ExecuteError::Timeout
    ));
    let expired = wire(-32000, "NotAllowed", &["Auth", "SessionExpired"], "x");
    let e = statement_error(&expired, QUIET);
    assert!(
        e.closes_session() && e.to_string().contains("expired"),
        "{e}"
    );
    let parse = wire(-32000, "Validation", &[], "Parse error");
    match statement_error(&parse, QUIET) {
        ExecuteError::Database {
            code,
            detail,
            message,
            ..
        } => {
            assert_eq!(code, "-32000");
            assert_eq!(detail.as_deref(), Some("Validation"));
            assert_eq!(message, "Parse error");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_encode_failure_says_to_cast_and_that_the_statement_may_have_run() {
    let e = wire(-32700, "Validation", &["Parse"], "Parse error");
    match statement_error(&e, QUIET) {
        ExecuteError::Database {
            code,
            message,
            hint,
            ..
        } => {
            assert_eq!(code, "-32700");
            assert!(message.contains("may have run"), "{message}");
            assert!(hint.unwrap().contains("<string>"));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_parse_position_is_a_byte_offset_counting_columns_in_chars() {
    let at = |line: usize, column: usize, text: &str| {
        parse_position(&format!("Parse error: x\n --> [{line}:{column}]\n"), text)
    };
    assert_eq!(at(1, 1, "SELEC 1"), Some(0));
    assert_eq!(at(3, 7, "SELECT * FROM t;\n\nSELEC 1;\n"), Some(24));
    assert_eq!(at(1, 3, "é x"), Some(3));
    assert_eq!(at(1, 99, "ab"), Some(2), "past the end clamps");
    assert_eq!(at(9, 1, "ab"), None, "no such line");
    assert_eq!(parse_position("no position here", "ab"), None);
}

#[test]
fn a_call_level_error_carries_its_position_and_others_none() {
    let parse = wire(
        -32000,
        "Validation",
        &[],
        "Parse error: Unexpected token\n --> [1:7]\n",
    );
    match call_error(&parse, "SELEC 1", QUIET) {
        ExecuteError::Database { position, .. } => assert_eq!(position, Some(6)),
        other => panic!("{other:?}"),
    }
    let timed = wire(-32004, "Query", &["TimedOut"], "x");
    assert!(matches!(
        call_error(&timed, "x", QUIET),
        ExecuteError::Timeout
    ));
}
