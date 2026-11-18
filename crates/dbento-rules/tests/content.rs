use crate::common::{RULES, history, summary};
use potluck_kind::{Blob, Change, Code, Role};

fn conn(header: &str, extra: &str) -> String {
    format!("dbento = 1\n\n[connections.{header}]\nengine = \"sqlite\"\npath = \"a.db\"\n{extra}")
}

fn one(text: &str) -> (&'static str, Code, Option<String>) {
    let findings = history("dbento.toml", text);
    assert_eq!(findings.len(), 1, "{text}: {findings:?}");
    summary(&findings).remove(0)
}

fn secret(rule: &'static str, detail: Option<&str>) -> (&'static str, Code, Option<String>) {
    (rule, Code::SecretValue, detail.map(str::to_owned))
}

#[test]
fn a_path_reads_does_not_accept_is_never_judged() {
    for path in ["notes.toml", "dbento.toml.bak", "queries/dbento.sql"] {
        assert!(history(path, "password = \"x\"\n").is_empty(), "{path}");
    }
}

#[test]
fn a_deletion_gives_no_finding() {
    let old = Some(Blob::Bytes(b"dbento = 1\npassword = \"x\"\n"));
    assert!(
        RULES
            .check(&Change::tip("dbento.toml", old, None, Role::Write))
            .is_empty()
    );
}

#[test]
fn an_unread_file_fails_closed_as_too_large() {
    for change in [
        Change::history("dbento.toml", Blob::TooLarge, Role::Write),
        Change::tip("team/dbento.toml", None, Some(Blob::TooLarge), Role::Admin),
    ] {
        assert_eq!(
            summary(&RULES.check(&change)),
            vec![secret("too_large", None)]
        );
    }
}

#[test]
fn bytes_that_are_not_utf8_are_a_parse_finding() {
    let change = Change::history(
        "dbento.toml",
        Blob::Bytes(b"dbento = 1\n# \xff\n"),
        Role::Write,
    );
    assert_eq!(summary(&RULES.check(&change)), vec![secret("parse", None)]);
}

#[test]
fn a_file_dbento_opens_gives_no_finding_at_any_depth_or_case() {
    for path in ["dbento.toml", "team/DBENTO.TOML", "a/b/Dbento.Toml"] {
        assert!(
            history(path, &conn("a", "read_only = true\n")).is_empty(),
            "{path}"
        );
    }
}

#[test]
fn a_finding_names_the_changed_path() {
    let findings = history("team/dbento.toml", "dbento = 1\npassword = \"x\"\n");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].path(), "team/dbento.toml");
}

#[test]
fn each_error_code_maps_to_its_rule_code_and_detail() {
    assert_eq!(
        one(&conn("a", "password = \"hunter2\"\n")),
        secret("password", Some("connections.a.password"))
    );
    assert_eq!(
        one(
            "dbento = 1\n\n[connections.docs]\nengine = \"mongodb\"\nuri = \"mongodb://app:hunter2@localhost/app\"\n"
        ),
        secret("secret", Some("connections.docs.uri"))
    );
    assert_eq!(
        one(&conn("a", "color = \"pink\"\n")),
        secret("invalid", Some("connections.a"))
    );
    assert_eq!(
        one(&conn("\"bad name\"", "")),
        secret("invalid_name", Some("connections.\"bad name\""))
    );
    assert_eq!(
        one("dbento = 1\n\n[folders.Prod]\ncolor = \"pink\"\n"),
        secret("invalid_folder", Some("folders.Prod"))
    );
    assert_eq!(
        one("dbento = 2\n"),
        ("unsupported_version", Code::FormatUnsupported, None)
    );
    assert_eq!(one("dbento = 1\n[connections.a\n"), secret("parse", None));
    assert_eq!(one("dbento = \"1\"\n"), secret("parse", None));
    assert_eq!(one(&conn("a", "colour = \"red\"\n")), secret("parse", None));
}

#[test]
fn a_name_that_is_not_a_bare_key_is_quoted_in_the_detail() {
    for (header, want) in [
        ("\"a.b\"", "connections.\"a.b\""),
        ("\"a/b\"", "connections.\"a/b\""),
        ("\"a b\"", "connections.\"a b\""),
        ("\"a\\\"b\"", "connections.\"a\\\"b\""),
        ("'a\\b'", "connections.\"a\\\\b\""),
        ("\"a\\tb\"", "connections.\"a\\u0009b\""),
        ("\"café\"", "connections.\"café\""),
    ] {
        assert_eq!(
            one(&conn(header, "")),
            secret("invalid_name", Some(want)),
            "{header}"
        );
    }
    assert_eq!(
        one("dbento = 1\n\n[folders.\"Prod/EU\"]\ncolor = \"pink\"\n"),
        secret("invalid_folder", Some("folders.\"Prod/EU\""))
    );
}

#[test]
fn a_password_detail_is_the_parsers_key_unquoted() {
    assert_eq!(
        one(&conn("\"bad name\"", "password = \"x\"\n")),
        secret("password", Some("connections.bad name.password"))
    );
}

#[test]
fn a_detail_is_cut_to_200_bytes_on_a_char_boundary() {
    let name = "é".repeat(150);
    let (rule, _, detail) = one(&conn(&format!("\"{name}\""), ""));
    assert_eq!(rule, "invalid_name");
    let detail = detail.unwrap();
    // 13 bytes of `connections."`, then 93 two-byte chars; a 94th would cross 200.
    assert_eq!(detail.len(), 199, "{detail}");
    assert!(detail.starts_with("connections.\"éé"), "{detail}");
}

#[test]
fn nesting_past_the_parsers_limit_is_a_parse_finding_not_a_panic() {
    let text = format!("dbento = 1\nx = {}{}\n", "[".repeat(500), "]".repeat(500));
    assert_eq!(one(&text), secret("parse", None));
}
