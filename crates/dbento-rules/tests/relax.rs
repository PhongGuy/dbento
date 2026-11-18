use crate::common::{RULES, summary};
use potluck_kind::{Blob, Change, Code, Finding, Role};

fn tip_as(path: &str, old: &str, new: &str, role: Role) -> Vec<Finding> {
    let (old, new) = (Blob::Bytes(old.as_bytes()), Blob::Bytes(new.as_bytes()));
    RULES.check(&Change::tip(path, Some(old), Some(new), role))
}

fn tip(old: &str, new: &str) -> Vec<Finding> {
    tip_as("dbento.toml", old, new, Role::Write)
}

// A workspace of SQLite connections, each `(name, extra lines)`, after optional folder tables.
fn ws(folders: &str, conns: &[(&str, &str)]) -> String {
    let mut text = format!("dbento = 1\n{folders}\n");
    for (name, extra) in conns {
        text.push_str(&format!(
            "\n[connections.{name}]\nengine = \"sqlite\"\npath = \"{name}.db\"\n{extra}"
        ));
    }
    text
}

fn relaxed(rule: &'static str, detail: &str) -> (&'static str, Code, Option<String>) {
    (rule, Code::SafetyRelaxation, Some(detail.to_owned()))
}

#[test]
fn turning_read_only_off_relaxes() {
    let old = ws("", &[("prod", "read_only = true\n")]);
    for new in [
        ws("", &[("prod", "read_only = false\n")]),
        ws("", &[("prod", "")]),
    ] {
        assert_eq!(
            summary(&tip(&old, &new)),
            vec![relaxed("relax_read_only", "connections.prod.read_only")],
            "{new}"
        );
    }
}

#[test]
fn moving_a_connection_off_red_relaxes() {
    let old = ws("", &[("prod", "color = \"red\"\n")]);
    for new in [
        ws("", &[("prod", "color = \"blue\"\n")]),
        ws("", &[("prod", "")]),
    ] {
        assert_eq!(
            summary(&tip(&old, &new)),
            vec![relaxed("relax_color", "connections.prod.color")],
            "{new}"
        );
    }
}

#[test]
fn a_folder_recolour_removal_or_move_relaxes_its_connections() {
    let red = "\n[folders.Prod]\ncolor = \"red\"\n";
    let old = ws(red, &[("prod", "folder = \"Prod\"\n")]);
    for new in [
        ws(
            "\n[folders.Prod]\ncolor = \"orange\"\n",
            &[("prod", "folder = \"Prod\"\n")],
        ),
        ws("", &[("prod", "folder = \"Prod\"\n")]),
        ws(red, &[("prod", "folder = \"Dev\"\n")]),
    ] {
        assert_eq!(
            summary(&tip(&old, &new)),
            vec![relaxed("relax_color", "connections.prod.color")],
            "{new}"
        );
    }
}

#[test]
fn tightening_gives_no_finding() {
    let old = ws("", &[("a", "color = \"blue\"\n"), ("b", "")]);
    let new = ws(
        "",
        &[("a", "color = \"red\"\n"), ("b", "read_only = true\n")],
    );
    assert!(tip(&old, &new).is_empty());
}

#[test]
fn a_deleted_renamed_or_added_connection_is_not_a_relaxation() {
    let strict = "read_only = true\ncolor = \"red\"\n";
    let old = ws("", &[("prod", strict)]);
    for new in [
        ws("", &[]),
        ws("", &[("prod2", "")]),
        ws("", &[("prod", strict), ("dev", "")]),
    ] {
        assert!(tip(&old, &new).is_empty(), "{new}");
    }
}

#[test]
fn without_a_usable_old_side_nothing_is_compared() {
    let new = ws("", &[("prod", "")]);
    let unparsable = "dbento = 1\n[connections.prod\n";
    let refused = ws("", &[("prod", "read_only = true\npassword = \"x\"\n")]);
    for old in [
        None,
        Some(Blob::TooLarge),
        Some(Blob::Bytes(b"dbento = 1\n# \xff\n")),
        Some(Blob::Bytes(unparsable.as_bytes())),
        Some(Blob::Bytes(refused.as_bytes())),
    ] {
        let change = Change::tip(
            "dbento.toml",
            old,
            Some(Blob::Bytes(new.as_bytes())),
            Role::Write,
        );
        assert!(RULES.check(&change).is_empty(), "{old:?}");
    }
}

#[test]
fn a_history_change_is_never_compared() {
    let new = ws("", &[("prod", "")]);
    let change = Change::history("dbento.toml", Blob::Bytes(new.as_bytes()), Role::Write);
    assert!(RULES.check(&change).is_empty());
}

#[test]
fn only_the_root_dbento_toml_in_exact_case_is_compared() {
    let old = ws("", &[("prod", "read_only = true\n")]);
    let new = ws("", &[("prod", "")]);
    for path in ["team/dbento.toml", "DBENTO.TOML", "Dbento.toml"] {
        assert!(tip_as(path, &old, &new, Role::Write).is_empty(), "{path}");
    }
}

#[test]
fn an_admin_push_is_still_reported() {
    let old = ws("", &[("prod", "read_only = true\n")]);
    let new = ws("", &[("prod", "")]);
    assert_eq!(
        summary(&tip_as("dbento.toml", &old, &new, Role::Admin)),
        vec![relaxed("relax_read_only", "connections.prod.read_only")]
    );
}

#[test]
fn a_new_side_the_content_rules_refuse_is_not_also_compared() {
    let old = ws("", &[("prod", "read_only = true\n")]);
    let new = ws("", &[("prod", "password = \"x\"\n")]);
    assert_eq!(
        summary(&tip(&old, &new)),
        vec![(
            "password",
            Code::SecretValue,
            Some("connections.prod.password".to_owned())
        )]
    );
}

#[test]
fn several_relaxations_give_one_finding_per_rule_in_name_order() {
    let strict = "read_only = true\ncolor = \"red\"\n";
    let old = ws("", &[("c", strict), ("a", strict), ("b", strict)]);
    let new = ws("", &[("c", ""), ("a", ""), ("b", "read_only = true\n")]);
    assert_eq!(
        summary(&tip(&old, &new)),
        vec![
            relaxed(
                "relax_read_only",
                "connections.a.read_only connections.c.read_only"
            ),
            relaxed(
                "relax_color",
                "connections.a.color connections.b.color connections.c.color"
            ),
        ]
    );
}

#[test]
fn a_long_list_is_cut_after_the_last_whole_entry_within_200_bytes() {
    let names: Vec<String> = (0..30).map(|i| format!("conn-{i:02}")).collect();
    let old = ws(
        "",
        &names
            .iter()
            .map(|n| (n.as_str(), "read_only = true\n"))
            .collect::<Vec<_>>(),
    );
    let new = ws(
        "",
        &names.iter().map(|n| (n.as_str(), "")).collect::<Vec<_>>(),
    );
    let want = names[..6]
        .iter()
        .map(|n| format!("connections.{n}.read_only"))
        .collect::<Vec<_>>()
        .join(" ");
    // Six 29-byte entries and five spaces; a seventh would reach 209.
    assert_eq!(want.len(), 179);
    assert_eq!(
        summary(&tip(&old, &new)),
        vec![relaxed("relax_read_only", &want)]
    );
}

#[test]
fn a_first_entry_over_200_bytes_is_cut_at_200() {
    let name = "p".repeat(250);
    let old = ws("", &[(name.as_str(), "read_only = true\n")]);
    let new = ws("", &[(name.as_str(), "")]);
    let findings = tip(&old, &new);
    assert_eq!(findings.len(), 1);
    let detail = findings[0].detail().unwrap();
    assert_eq!(detail.len(), 200);
    assert!(detail.starts_with("connections.ppp"), "{detail}");
}
