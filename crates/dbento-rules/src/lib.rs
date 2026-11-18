//! Dbento's push rules for a Potluck team server, as a potluck-kind rule set: which files of a
//! Dbento workspace a push may not carry, the workspace format `dbento.toml` declares, and the
//! files a new workspace starts with.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod content;
mod detail;
mod paths;
mod relax;

use dbento_workspace::Workspace;
use potluck_kind::{Blob, Change, Finding, RuleSet, SeedFile};

/// The rule set Potluck registers for kind `dbento`.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct DbentoRules;

/// The value Potluck registers, as `&dbento_rules::RULE_SET`.
pub const RULE_SET: DbentoRules = DbentoRules;

impl RuleSet for DbentoRules {
    fn kind(&self) -> &'static str {
        "dbento"
    }

    // The literal, not `RULES_ABI`: built inside Potluck, that constant is Potluck's own, so only
    // a literal lets its runtime version check catch a mismatch.
    fn abi(&self) -> u32 {
        1
    }

    fn formats(&self) -> &'static [&'static str] {
        &["1"]
    }

    fn format_file(&self) -> &'static str {
        "dbento.toml"
    }

    fn detect_format(&self, format_file: &[u8]) -> Option<String> {
        content::detect_format(format_file)
    }

    fn path_rule(&self, path: &str) -> Option<Finding> {
        paths::path_rule(path)
    }

    fn reads(&self, path: &str) -> bool {
        paths::reads(path)
    }

    fn check(&self, change: &Change<'_>) -> Vec<Finding> {
        if !paths::reads(change.path()) {
            return Vec::new();
        }
        let (findings, new) = content::check_content(change.path(), change.new_blob());
        // Relaxation compares only a root dbento.toml the content rules accepted, against an old
        // side that opens too; an old side Dbento would refuse is no baseline, not a finding.
        if !findings.is_empty() || !change.has_baseline() || change.path() != "dbento.toml" {
            return findings;
        }
        match (new, change.old_blob().and_then(open_old)) {
            (Some(new), Some(old)) => relax::relaxations(&old, &new),
            _ => findings,
        }
    }

    // A new team workspace starts as `dbento init` would leave it, so both agree on the format.
    fn seed(&self) -> Vec<SeedFile> {
        vec![SeedFile::new(
            "dbento.toml",
            dbento_workspace::TEMPLATE.as_bytes(),
        )]
    }
}

// The old side of a tip change, opened exactly as the content rules open the new side.
fn open_old(blob: Blob<'_>) -> Option<Workspace> {
    let Blob::Bytes(bytes) = blob else {
        return None;
    };
    content::open(std::str::from_utf8(bytes).ok()?).ok()
}
