use ::redis::{Value, parse_redis_value};

use super::{command_docs_of, fold_counts};

fn docs() -> Value {
    let bytes = include_bytes!("../../../tests/fixtures/redis/valkey-9.0.7-command-docs.resp3");
    parse_redis_value(bytes).unwrap()
}

#[test]
fn command_docs_carry_names_tokens_and_subcommands() {
    let all = command_docs_of(&docs()).unwrap();
    assert_eq!(all.len(), 254);
    assert!(all.windows(2).all(|w| w[0].name < w[1].name));
    let get = all.iter().find(|d| d.name == "get").unwrap();
    assert_eq!(get.summary, "Returns the string value of a key.");
    assert_eq!(
        (
            get.arguments[0].name.as_str(),
            get.arguments[0].kind.as_str()
        ),
        ("key", "key")
    );
    let pexpireat = all.iter().find(|d| d.name == "pexpireat").unwrap();
    let condition = &pexpireat.arguments[2];
    assert!(condition.optional && condition.token.is_none());
    let tokens: Vec<_> = condition
        .arguments
        .iter()
        .map(|a| a.token.as_deref().unwrap())
        .collect();
    assert_eq!(tokens, ["NX", "XX", "GT", "LT"]);
    let config = all.iter().find(|d| d.name == "config").unwrap();
    assert!(config.subcommands.iter().any(|d| d.name == "config|get"));
}

#[test]
fn counts_stop_at_the_first_db_that_does_not_exist() {
    let r = |s: &str| parse_redis_value(format!("{s}\r\n").as_bytes()).unwrap();
    let replies = [
        r("+OK"),
        r(":5"),
        r("+OK"),
        r(":0"),
        r("-ERR DB index is out of range"),
        r(":0"),
        r("+OK"),
        r(":9"),
    ];
    assert_eq!(fold_counts(&replies), vec![(0, 5), (1, 0)]);
}
