use dbento_core::{Finding, Refusal, parse_refusal, redact_token, rule_text};

/// A hook's stderr as git relays it: prefixed, padded, then git's own failure line.
fn remote(lines: &[&str]) -> String {
    let mut out: String = lines
        .iter()
        .map(|l| format!("remote: {l}        \n"))
        .collect();
    out.push_str("error: failed to push some refs to 'file:///tmp/origin.git'\n");
    out
}

fn parse(lines: &[&str]) -> Refusal {
    parse_refusal(&remote(lines)).expect("a refusal")
}

#[test]
fn a_golden_password_line_and_its_detail_make_one_finding() {
    let r = parse(&[
        "potluck-reject secret_value refs/heads/main dbento.toml password",
        "potluck-reject-detail connections.a.password",
    ]);
    let want = Finding::new("secret_value", "refs/heads/main", "dbento.toml", "password")
        .with_detail("connections.a.password");
    assert_eq!(r.findings, vec![want]);
    assert_eq!((r.more, r.other.len(), r.is_server_error()), (0, 0, false));
}

#[test]
fn findings_keep_the_received_order_without_details() {
    let r = parse(&[
        "potluck-reject secret_value refs/heads/main dbento.toml parse",
        "potluck-reject format_unsupported refs/heads/main dbento.toml format_unsupported",
    ]);
    let rules: Vec<&str> = r.findings.iter().map(|f| f.rule.as_str()).collect();
    assert_eq!(rules, ["parse", "format_unsupported"]);
    assert!(r.findings.iter().all(|f| f.detail.is_none()));
}

#[test]
fn ref_path_and_detail_are_percent_decoded_lossily() {
    let r = parse(&[
        "potluck-reject secret_value refs/heads/f%25x queries/my%20report.sql invalid_name",
        "potluck-reject-detail connections.\"bad%20name\"",
        "potluck-reject path_rule refs/heads/main queries/%FF.sql database_file",
    ]);
    assert_eq!(r.findings[0].git_ref, "refs/heads/f%x");
    assert_eq!(r.findings[0].path, "queries/my report.sql");
    assert_eq!(
        r.findings[0].detail.as_deref(),
        Some("connections.\"bad name\"")
    );
    assert_eq!(r.findings[1].path, "queries/\u{FFFD}.sql");
}

#[test]
fn rows_show_no_file_repeated_refs_and_the_more_count() {
    let r = parse(&[
        "potluck-reject secret_value refs/heads/main / secret",
        "potluck-reject safety_relaxation refs/heads/main dbento.toml relax_read_only",
        "potluck-reject safety_relaxation refs/heads/dev dbento.toml relax_read_only",
        "potluck-reject secret_value refs/heads/main dbento.toml password",
        "potluck-reject-detail connections.a.password",
        "potluck-reject-more 7",
    ]);
    assert_eq!(r.findings[0].path, "/");
    assert_eq!(r.more, 7);
    assert_eq!(
        r.rows(),
        [
            "no file on refs/heads/main: a secret value",
            "dbento.toml on refs/heads/main: relaxes read-only on a connection",
            "dbento.toml on refs/heads/dev: relaxes read-only on a connection",
            "dbento.toml on refs/heads/main: a password (connections.a.password)",
            "and 7 more",
        ]
    );
}

#[test]
fn a_potluck_error_is_a_server_error_not_a_finding() {
    let r = parse(&["potluck-error kind rules failed"]);
    assert!(r.findings.is_empty() && r.is_server_error());
    assert_eq!(r.other, ["potluck-error kind rules failed"]);
    assert_eq!(r.rows(), ["potluck-error kind rules failed"]);
}

#[test]
fn unknown_malformed_and_orphan_lines_go_to_other_as_text() {
    let lines = [
        "potluck-reject-detail orphan",
        "potluck-notice something new",
        "potluck-reject secret_value refs/heads/main dbento.toml",
        "potluck-reject secret_value refs/heads/main bad%zz.sql password",
    ];
    let r = parse(&lines);
    assert!(r.findings.is_empty());
    assert_eq!(r.other, lines);
}

#[test]
fn a_detail_after_the_more_line_does_not_attach_to_an_older_finding() {
    let r = parse(&[
        "potluck-reject secret_value refs/heads/main dbento.toml password",
        "potluck-reject-more 1",
        "potluck-reject-detail late",
    ]);
    assert_eq!(r.findings[0].detail, None);
    assert_eq!(r.other, ["potluck-reject-detail late"]);
}

#[test]
fn stderr_without_remote_potluck_lines_is_not_a_refusal() {
    let text = "remote: Counting objects\nTo file:///o.git\n\
                ! [remote rejected] main -> main (pre-receive hook declined)\n";
    assert_eq!(parse_refusal(text), None);
    assert_eq!(parse_refusal(""), None);
    let bare = "potluck-reject secret_value refs/heads/main dbento.toml password\n";
    assert_eq!(parse_refusal(bare), None);
}

#[test]
fn rule_text_and_token_redaction() {
    assert_eq!(rule_text("password"), "a password");
    assert_eq!(rule_text("database_file"), "a database file");
    assert_eq!(rule_text("brand_new_rule"), "brand_new_rule");
    assert_eq!(
        rule_text("default_branch_missing"),
        "the workspace's default branch is missing, so push it first"
    );
    assert_eq!(rule_text("branch_delete"), "deletes the default branch");
    assert_eq!(
        rule_text("force_push"),
        "rewrites the default branch's history"
    );
    assert_eq!(
        rule_text("ref_namespace"),
        "a ref outside branches and tags"
    );
    assert_eq!(
        redact_token("Bearer plk_AbC-123_x done"),
        "Bearer plk_[redacted] done"
    );
    assert_eq!(
        redact_token("a=plk_x;b=plk_y"),
        "a=plk_[redacted];b=plk_[redacted]"
    );
    assert_eq!(redact_token("plk_ alone, no token"), "plk_ alone, no token");
}
