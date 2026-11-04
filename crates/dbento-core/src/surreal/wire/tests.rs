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
