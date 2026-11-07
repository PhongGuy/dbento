use ::redis::{Value, parse_redis_value};

use super::{gate, verdict};
use crate::redis::classify::redis_classification;
use crate::redis::lex::RedisStatement;
use crate::redis::table::CommandTable;
use crate::session::{Classification, ExecuteError, ReadOnlyEnforcement::*};

fn table() -> CommandTable {
    let bytes = include_bytes!("../../../tests/fixtures/redis/valkey-9.0.7-command.resp3");
    CommandTable::from_value(&parse_redis_value(bytes).unwrap()).unwrap()
}

fn call(words: &[&str]) -> Vec<Vec<u8>> {
    words.iter().map(|w| w.as_bytes().to_vec()).collect()
}

fn reply(line: &str) -> Value {
    parse_redis_value(format!("{line}\r\n").as_bytes()).unwrap()
}

fn fenced(cmd: &str) -> Value {
    reply(&format!(
        "-NOPERM User dbento_ro has no permissions to run the '{cmd}' command"
    ))
}

#[test]
fn every_call_fenced_is_database_and_pre_acl_refusals_are_set_aside() {
    let calls = [
        call(&["set", "dbento-probe", "dbento-probe"]),
        call(&["save"]),
        call(&["debug", "dbento-probe"]),
        call(&["module", "load", "dbento-probe"]),
    ];
    let replies = [
        fenced("set"),
        reply("-ERR Command 'save' not allowed inside a transaction"),
        reply(
            "-ERR DEBUG command not allowed. If the enable-debug-command option is set to \"local\", you can run it from a local connection, otherwise you need to set this option in the configuration file, and then restart the server.",
        ),
        reply(
            "-ERR MODULE command not allowed. If the enable-module-command option is set to \"local\", you can run it from a local connection, otherwise you need to set this option in the configuration file, and then restart the server.",
        ),
    ];
    let p = verdict(&table(), &calls, &replies);
    assert_eq!(p.level, Database);
    let names: Vec<_> = p.unprobeable.iter().map(String::as_str).collect();
    assert_eq!(names, ["debug", "module|load", "save"]);
}

#[test]
fn anything_else_fails_closed() {
    let t = table();
    let calls = [
        call(&["set", "dbento-probe", "dbento-probe"]),
        call(&["del", "dbento-probe"]),
    ];
    for second in [
        reply("+QUEUED"),
        reply("-NOPERM No permissions to access a key"),
        reply("-ERR wrong number of arguments for 'del' command"),
    ] {
        assert_eq!(
            verdict(&t, &calls, &[fenced("set"), second.clone()]).level,
            BestEffort,
            "{second:?}"
        );
    }
    assert_eq!(verdict(&t, &calls, &[fenced("set")]).level, BestEffort);
}

fn classified(t: &CommandTable, words: &[&str]) -> Classification {
    redis_classification(
        t,
        &RedisStatement {
            args: call(words),
            range: 0..0,
        },
    )
    .unwrap()
}

#[test]
fn the_gate_follows_the_spec_table() {
    let t = table();
    let write = classified(&t, &["set", "a", "b"]);
    let read = classified(&t, &["get", "a"]);
    let unknown = classified(&t, &["nosuchcmd"]);
    let refuses = |c: &Classification, unprobeable, level| {
        matches!(
            gate(c, unprobeable, level),
            Some(ExecuteError::ReadOnlyViolation { .. })
        )
    };
    assert!(gate(&write, false, None).is_none());
    assert!(refuses(&write, false, Some(Database)));
    assert!(!refuses(&unknown, false, Some(Database)));
    assert!(refuses(&unknown, true, Some(Database)));
    assert!(refuses(&unknown, false, Some(BestEffort)));
    assert!(!refuses(&read, false, Some(BestEffort)));
}
