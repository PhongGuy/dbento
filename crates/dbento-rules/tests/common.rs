//! Handles shared by the suites.

use potluck_kind::{Blob, Change, Code, Finding, Role, RuleSet};

/// The rule set under test, held the way Potluck holds it.
pub const RULES: &dyn RuleSet = &dbento_rules::RULE_SET;

/// Checks `text` as a history change at `path`, the way Potluck checks a pushed blob.
pub fn history(path: &str, text: &str) -> Vec<Finding> {
    RULES.check(&Change::history(
        path,
        Blob::Bytes(text.as_bytes()),
        Role::Write,
    ))
}

/// Each finding as (rule, code, detail), for one-line comparisons.
pub fn summary(findings: &[Finding]) -> Vec<(&'static str, Code, Option<String>)> {
    findings
        .iter()
        .map(|f| (f.rule(), f.code(), f.detail().map(str::to_owned)))
        .collect()
}
