use ::redis::{Value, parse_redis_value};

use super::{entry_writes, redis_classification};
use crate::redis::lex::{RedisStatement, parse_script};
use crate::redis::table::CommandTable;
use crate::session::{Unguarded, Writes};

const COMMAND: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/redis/valkey-9.0.7-command.resp3"
));
const MODULE: &str = "*1\r\n*10\r\n$9\r\nMYMOD.PUT\r\n:-3\r\n*1\r\n$6\r\nmodule\r\n:1\r\n:1\r\n:1\r\n\
                      *1\r\n+@write\r\n*0\r\n*1\r\n%1\r\n+flags\r\n*2\r\n+OW\r\n+update\r\n*0\r\n";

fn table() -> CommandTable {
    CommandTable::from_value(&parse_redis_value(COMMAND).unwrap()).unwrap()
}
fn stmt(line: &str) -> RedisStatement {
    parse_script(line).remove(0).unwrap()
}
fn writes(t: &CommandTable, line: &str) -> Writes {
    redis_classification(t, &stmt(line)).unwrap().writes
}
fn words(call: &[Vec<u8>]) -> Vec<String> {
    call.iter()
        .map(|w| String::from_utf8(w.clone()).unwrap())
        .collect()
}

#[test]
fn key_reads_are_no() {
    let t = table();
    for line in [
        "GET k",
        "HGETALL user:1",
        "SCAN 0 MATCH * COUNT 10",
        "DBSIZE",
        "EVAL_RO \"return 1\" 0",
        "FCALL_RO f 0",
        "KEYS *",
        "PFCOUNT h",
        "MEMORY USAGE k",
        "OBJECT ENCODING k",
        "XINFO STREAM s",
    ] {
        assert_eq!(writes(&t, line), Writes::No, "{line}");
    }
}

#[test]
fn session_control_and_the_fixed_reads_are_no() {
    let t = table();
    for line in [
        "SELECT 1",
        "PING",
        "ECHO x",
        "TIME",
        "COMMAND",
        "COMMAND DOCS get",
        "CLIENT ID",
        "CLIENT GETNAME",
        "CLIENT INFO",
        "CLIENT LIST",
        "CLIENT SETNAME me",
        "CLIENT SETINFO LIB-NAME x",
        "INFO",
        "LASTSAVE",
        "ROLE",
        "CONFIG GET maxmemory",
        "SLOWLOG GET",
        "SLOWLOG LEN",
        "LATENCY LATEST",
        "LATENCY HISTORY x",
        "MEMORY STATS",
        "MEMORY DOCTOR",
        "PUBSUB CHANNELS",
        "PUBSUB NUMSUB",
        "PUBSUB NUMPAT",
        "SCRIPT EXISTS a",
        "FUNCTION LIST",
        "FUNCTION STATS",
        "ACL WHOAMI",
        "ACL CAT",
        "CLUSTER INFO",
        "CLUSTER NODES",
        "CLUSTER SLOTS",
        "CLUSTER SHARDS",
        "CONFIG HELP",
        "XINFO HELP",
        "WATCH k",
        "UNWATCH",
        "MULTI",
        "EXEC",
        "DISCARD",
    ] {
        assert_eq!(writes(&t, line), Writes::No, "{line}");
    }
}

#[test]
fn flagged_writes_key_spec_writes_and_publish_are_yes() {
    let t = table();
    for line in [
        "SET k v",
        "DEL k",
        "HSET h f v",
        "LPUSH l a",
        "EXPIRE k 10",
        "BLPOP k 0",
        "EVAL \"return 1\" 0",
        "EVALSHA abc 0",
        "FCALL f 0",
        "PUBLISH ch m",
        "SPUBLISH ch m",
        "FUNCTION LOAD x",
        "XGROUP CREATE s g $",
        "FLUSHDB",
        "flushall async",
    ] {
        assert_eq!(writes(&t, line), Writes::Yes, "{line}");
    }
}

#[test]
fn everything_else_is_unknown_with_its_categories() {
    let t = table();
    for line in [
        "CONFIG SET maxmemory 1",
        "CONFIG REWRITE",
        "CLIENT KILL ID 1",
        "CLIENT PAUSE 10",
        "ACL SETUSER x",
        "SCRIPT LOAD x",
        "SCRIPT FLUSH",
        "WAIT 0 0",
        "READONLY",
        "SAVE",
        "BGSAVE",
        "SHUTDOWN",
        "DEBUG SLEEP 0",
        "MODULE LOAD x",
        "CLIENT",
        "CONFIG NOSUCH",
        "NOSUCH x",
        "\"\"",
    ] {
        assert_eq!(writes(&t, line), Writes::Unknown, "{line}");
    }
    let nosuch = redis_classification(&t, &stmt("NOSUCH x")).unwrap();
    assert!(nosuch.categories.is_empty() && nosuch.unguarded.is_empty());
}

#[test]
fn flushes_are_unguarded_and_transactions_are_control() {
    let t = table();
    for line in ["FLUSHDB", "flushall async"] {
        let c = redis_classification(&t, &stmt(line)).unwrap();
        assert_eq!(c.unguarded, [Unguarded::FlushKeys], "{line}");
    }
    assert!(
        redis_classification(&t, &stmt("DEL k"))
            .unwrap()
            .unguarded
            .is_empty()
    );
    for (line, control) in [
        ("MULTI", true),
        ("exec", true),
        ("DISCARD", true),
        ("WATCH k", false),
    ] {
        let c = redis_classification(&t, &stmt(line)).unwrap();
        assert_eq!(c.transaction_control, control, "{line}");
    }
}

#[test]
fn categories_come_from_the_resolved_entry() {
    let t = table();
    let cats = |line: &str| redis_classification(&t, &stmt(line)).unwrap().categories;
    assert_eq!(cats("GET k"), ["@read", "@string", "@fast"]);
    assert_eq!(
        cats("config get maxmemory"),
        ["@admin", "@slow", "@dangerous"]
    );
    assert!(cats("KEYS *").iter().any(|c| c == "@dangerous"));
    assert!(cats("INFO").iter().any(|c| c == "@dangerous"));
    assert!(
        cats("EVAL_RO \"return 1\" 0")
            .iter()
            .any(|c| c == "@scripting")
    );
    assert!(cats("BLPOP k 0").iter().any(|c| c == "@blocking"));
}

#[test]
fn some_commands_are_refused_in_every_session_whatever_the_table_holds() {
    let empty = CommandTable::from_value(&Value::Array(vec![])).unwrap();
    for t in [table(), empty] {
        for line in [
            "SUBSCRIBE ch",
            "psubscribe p*",
            "SSUBSCRIBE c",
            "UNSUBSCRIBE",
            "PUNSUBSCRIBE",
            "SUNSUBSCRIBE",
            "MONITOR",
            "SYNC",
            "PSYNC ? -1",
            "CLIENT REPLY OFF",
            "client tracking on",
            "RESET",
            "AUTH hunter2",
            "AUTH user hunter2",
            "HELLO 3",
            "QUIT",
        ] {
            let refusal = redis_classification(&t, &stmt(line)).unwrap_err();
            assert!(!refusal.reason.is_empty(), "{line}");
            assert!(
                !refusal.reason.contains("hunter2"),
                "{line}: {}",
                refusal.reason
            );
        }
    }
    let auth = redis_classification(&table(), &stmt("AUTH x")).unwrap_err();
    assert!(auth.reason.contains("credentials"), "{}", auth.reason);
}

#[test]
fn the_probe_sends_one_arity_valid_call_per_write_or_unknown_leaf() {
    let t = table();
    let calls = t.probe_calls();
    assert_eq!(calls.len(), 210);
    let listed: Vec<Vec<String>> = calls.iter().map(|c| words(c)).collect();
    let p = "dbento-probe";
    for want in [
        vec!["set", p, p],
        vec!["flushall"],
        vec!["config", "set", p, p],
        vec!["client", "kill", p],
        vec!["eval", p, p],
        vec!["debug", p],
        vec!["module", "load", p],
    ] {
        assert!(listed.iter().any(|c| *c == want), "{want:?}");
    }
    for call in &calls {
        let e = t.entry(call).unwrap();
        assert!(e.is_leaf() && !e.has_flag("no_multi"), "{}", e.name);
        assert_eq!(call.len() as i64, e.arity.abs(), "{}", e.name);
        assert!(
            matches!(entry_writes(e), Ok(Writes::Yes | Writes::Unknown)),
            "{}",
            e.name
        );
    }
    for absent in [
        "get",
        "ping",
        "select",
        "multi",
        "discard",
        "subscribe",
        "auth",
        "hello",
        "save",
        "shutdown",
    ] {
        assert!(!listed.iter().any(|c| c[0] == absent), "{absent}");
    }
}

#[test]
fn the_probe_sends_nothing_the_recommended_reader_acl_grants() {
    let t = table();
    let granted = [
        "ping",
        "echo",
        "time",
        "hello",
        "select",
        "command",
        "multi",
        "discard",
        "client|setname",
        "client|setinfo",
        "client|id",
        "client|info",
    ];
    for call in t.probe_calls() {
        let e = t.entry(&call).unwrap();
        assert!(!e.categories.iter().any(|c| c == "@read"), "{}", e.name);
        assert!(
            !granted.contains(&e.name.as_str()) && !e.name.starts_with("command|"),
            "{}",
            e.name
        );
    }
}

#[test]
fn a_module_command_follows_the_key_spec_it_declares() {
    let t = CommandTable::from_value(&parse_redis_value(MODULE.as_bytes()).unwrap()).unwrap();
    assert_eq!(writes(&t, "MYMOD.PUT k v"), Writes::Yes);
    assert_eq!(
        words(&t.probe_calls()[0]),
        ["mymod.put", "dbento-probe", "dbento-probe"]
    );
}
