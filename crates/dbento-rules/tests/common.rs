//! Handles shared by the suites.

use potluck_kind::RuleSet;

/// The rule set under test, held the way Potluck holds it.
pub const RULES: &dyn RuleSet = &dbento_rules::RULE_SET;
