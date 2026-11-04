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

fn token(claims: &[(&str, &str)]) -> Wire {
    Wire::Object(
        claims
            .iter()
            .map(|(k, v)| ((*k).to_owned(), Wire::Text((*v).to_owned())))
            .collect(),
    )
}

fn roles(names: &[&str]) -> Wire {
    let names = names.iter().map(|n| Wire::Text((*n).to_owned())).collect();
    Wire::Object(vec![("roles".to_owned(), Wire::Array(names))])
}

#[test]
fn claims_give_the_name_and_level() {
    let id = |claims: &[(&str, &str)]| identity_of(&token(claims)).unwrap();
    let user = |name: &str, level| Identity::User {
        name: name.to_owned(),
        level,
    };
    assert_eq!(id(&[("ID", "ro")]), user("ro", AuthLevel::Root));
    assert_eq!(
        id(&[("ID", "ne"), ("NS", "n")]),
        user("ne", AuthLevel::Namespace)
    );
    assert_eq!(
        id(&[("DB", "d"), ("ID", "dv"), ("NS", "n")]),
        user("dv", AuthLevel::Database)
    );
    assert_eq!(identity_of(&Wire::None).unwrap(), Identity::Anonymous);
}

#[test]
fn a_record_user_is_refused_by_its_ac_claim() {
    let e = identity_of(&token(&[
        ("AC", "acc"),
        ("ID", "acct:1"),
        ("NS", "n"),
        ("DB", "d"),
    ]));
    assert!(
        matches!(e, Err(ConnectError::Unsupported { reason }) if reason.contains("record users"))
    );
}

#[test]
fn a_token_with_no_id_cannot_name_a_user() {
    assert!(matches!(
        identity_of(&token(&[("NS", "n")])),
        Err(ConnectError::Open { .. })
    ));
}

#[test]
fn the_strongest_role_wins_in_any_case_and_unknown_names_do_not_count() {
    assert_eq!(role_of(&roles(&["owner"])), Some(Role::Owner));
    assert_eq!(role_of(&roles(&["VIEWER", "EDITOR"])), Some(Role::Editor));
    assert_eq!(role_of(&roles(&["Viewer"])), Some(Role::Viewer));
    assert_eq!(role_of(&roles(&["AUDITOR", "VIEWER"])), Some(Role::Viewer));
    assert_eq!(role_of(&roles(&["AUDITOR"])), None);
    assert_eq!(role_of(&roles(&[])), None);
    assert_eq!(role_of(&Wire::None), None);
}

#[test]
fn the_level_follows_the_role_and_an_unread_role_blocks_only_a_writable_session() {
    let level = |role, requested| read_only_level(role, requested, "t");
    assert_eq!(
        level(Ok(Role::Viewer), false).unwrap(),
        Some(ReadOnlyEnforcement::Database)
    );
    assert_eq!(level(Ok(Role::Owner), false).unwrap(), None);
    assert_eq!(
        level(Ok(Role::Editor), true).unwrap(),
        Some(ReadOnlyEnforcement::BestEffort)
    );
    assert_eq!(
        level(Err("no reply".to_owned()), true).unwrap(),
        Some(ReadOnlyEnforcement::BestEffort)
    );
    let refused = level(Err("no reply".to_owned()), false);
    assert!(matches!(
        refused,
        Err(ConnectError::Open { message }) if message.contains("no reply") && message.contains("read_only")
    ));
}

#[test]
fn a_statement_reads_ok_and_err_from_a_query_reply() {
    let reply = Wire::Array(vec![
        Wire::Object(vec![
            ("result".to_owned(), Wire::Int(1)),
            ("status".to_owned(), Wire::Text("OK".to_owned())),
        ]),
        Wire::Object(vec![
            ("result".to_owned(), Wire::Text("IAM error".to_owned())),
            ("status".to_owned(), Wire::Text("ERR".to_owned())),
        ]),
    ]);
    assert_eq!(statement(&reply, 0), Ok(Wire::Int(1)));
    assert_eq!(statement(&reply, 1), Err("IAM error".to_owned()));
    assert!(statement(&reply, 2).is_err());
    assert!(statement(&Wire::None, 0).is_err());
}

#[test]
fn holds_reads_a_key_of_an_info_map() {
    let names = Wire::Object(vec![("d1".to_owned(), Wire::Text("DEFINE".to_owned()))]);
    let info = Wire::Object(vec![("databases".to_owned(), names)]);
    assert!(holds(&info, "databases", "d1"));
    assert!(!holds(&info, "databases", "d2"));
    assert!(!holds(&info, "namespaces", "d1"));
}
