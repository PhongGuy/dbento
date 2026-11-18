use crate::common::RULES;
use dbento_rules::{DbentoRules, RULE_SET};
use potluck_kind::{RULES_ABI, RuleSet};

#[test]
fn abi_is_the_contract_version_it_was_built_against() {
    assert_eq!(RULES.abi(), RULES_ABI);
}

#[test]
fn kind_formats_and_format_file_name_dbento() {
    assert_eq!(RULES.kind(), "dbento");
    assert_eq!(RULES.formats(), &["1"]);
    assert_eq!(RULES.format_file(), "dbento.toml");
}

#[test]
fn rule_set_promotes_to_a_static_trait_object() {
    fn register(rules: &'static dyn RuleSet) -> &'static str {
        rules.kind()
    }
    assert_eq!(register(&RULE_SET), "dbento");
    assert_eq!(format!("{RULE_SET:?}"), "DbentoRules");
}

#[test]
fn rule_set_is_a_thread_safe_copy_value() {
    fn bounds<T: RuleSet + Copy + Send + Sync + 'static>() {}
    bounds::<DbentoRules>();
}

#[test]
fn seed_is_exactly_the_template_dbento_init_writes() {
    let seed = RULES.seed();
    assert_eq!(seed.len(), 1);
    assert_eq!(seed[0].path(), "dbento.toml");
    assert_eq!(seed[0].bytes(), dbento_workspace::TEMPLATE.as_bytes());
}
