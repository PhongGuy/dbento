use super::*;

#[test]
fn the_version_parser_takes_a_build_and_a_prerelease() {
    assert_eq!(server_version("surrealdb-3.3.5"), Some((3, 3, 5)));
    assert_eq!(
        server_version("surrealdb-3.2.6+20261018.c63e805"),
        Some((3, 2, 6))
    );
    assert_eq!(server_version("surrealdb-4.0.0-beta.1"), Some((4, 0, 0)));
    assert_eq!(server_version("3.3.5"), None);
    assert_eq!(server_version("surrealdb-3.3"), None);
    assert_eq!(server_version("surrealdb-3.3.5.1"), None);
}

#[test]
fn the_floor_is_three_three_zero_and_newer_majors_warn() {
    let check = |text: &str| version_check(text, "t");
    assert!(matches!(
        check("surrealdb-3.2.6+20261018.c63e805"),
        Err(ConnectError::Open { message }) if message.contains("3.2.6") && message.contains("3.3.0")
    ));
    assert_eq!(check("surrealdb-3.3.0").unwrap().1, None);
    assert_eq!(check("surrealdb-3.3.5+build").unwrap().1, None);
    assert!(
        check("surrealdb-3.4.0-beta.1")
            .unwrap()
            .1
            .unwrap()
            .contains("3.4.0-beta.1")
    );
    assert!(
        check("surrealdb-4.0.0")
            .unwrap()
            .1
            .unwrap()
            .contains("4.0.0")
    );
    assert!(matches!(
        check("something else"),
        Err(ConnectError::Open { .. })
    ));
}
