//! Dbento's push rules for a Potluck team server, as a potluck-kind rule set: which files of a
//! Dbento workspace a push may not carry, the workspace format `dbento.toml` declares, and the
//! files a new workspace starts with.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod content;

use potluck_kind::{Change, Finding, RuleSet, SeedFile};

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

    fn path_rule(&self, _path: &str) -> Option<Finding> {
        None
    }

    fn reads(&self, _path: &str) -> bool {
        false
    }

    fn check(&self, _change: &Change<'_>) -> Vec<Finding> {
        Vec::new()
    }

    // A new team workspace starts as `dbento init` would leave it, so both agree on the format.
    fn seed(&self) -> Vec<SeedFile> {
        vec![SeedFile::new(
            "dbento.toml",
            dbento_workspace::TEMPLATE.as_bytes(),
        )]
    }
}
