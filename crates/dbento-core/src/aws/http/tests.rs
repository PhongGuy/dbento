use super::{endpoint_origin, https_client};

#[test]
fn an_endpoint_is_handed_over_as_its_normalised_origin() {
    for (written, origin) in [
        ("http://127.0.0.1:5000", "http://127.0.0.1:5000"),
        ("http://LOCALHOST:5000/", "http://localhost:5000"),
        (" http://localhost:8000", "http://localhost:8000"),
        ("http://%6cocalhost:8000", "http://localhost:8000"),
        ("http:localhost:8000", "http://localhost:8000"),
        (
            "https://dynamodb.eu-west-1.amazonaws.com:443/",
            "https://dynamodb.eu-west-1.amazonaws.com",
        ),
    ] {
        assert_eq!(
            endpoint_origin(written).as_deref(),
            Some(origin),
            "{written}"
        );
    }
    assert_eq!(endpoint_origin("not a url"), None);
    assert_eq!(
        endpoint_origin("file:///x"),
        None,
        "an opaque origin has no endpoint"
    );
}

#[test]
fn a_deeply_nested_blob_endpoint_has_no_origin_and_does_not_crash() {
    // A shared workspace can carry any endpoint text; a nested blob: chain once overflowed the stack.
    let endpoint = format!("{}http://h/", "blob:".repeat(10_000));
    assert_eq!(endpoint_origin(&endpoint), None);
}

#[test]
fn the_shared_https_client_builds_outside_a_runtime() {
    let a = https_client();
    let b = a.clone();
    drop((a, b));
}
