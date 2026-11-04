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
