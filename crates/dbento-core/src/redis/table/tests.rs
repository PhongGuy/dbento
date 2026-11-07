use ::redis::{Value, parse_redis_value};

use super::{CommandTable, TableError};

const COMMAND: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/redis/valkey-9.0.7-command.resp3"
));
// One module command in the forms a RESP3 reply may also take: arrays for sets, bulk flags.
const MODULE: &str = "*1\r\n*10\r\n$9\r\nMYMOD.PUT\r\n:-3\r\n*1\r\n$6\r\nmodule\r\n:1\r\n:1\r\n:1\r\n\
                      *1\r\n+@write\r\n*0\r\n*1\r\n%1\r\n+flags\r\n*2\r\n+OW\r\n+update\r\n*0\r\n";

fn table() -> CommandTable {
    CommandTable::from_value(&parse_redis_value(COMMAND).unwrap()).unwrap()
}

fn parse(bytes: &[u8]) -> Result<CommandTable, TableError> {
    CommandTable::from_value(&parse_redis_value(bytes).unwrap())
}

fn name(t: &CommandTable, words: &[&str]) -> Option<String> {
    let args: Vec<Vec<u8>> = words.iter().map(|w| w.as_bytes().to_vec()).collect();
    t.entry(&args).map(|e| e.name.clone())
}

#[test]
fn the_valkey_table_holds_every_command_and_subcommand() {
    let t = table();
    assert_eq!(t.entries().count(), 396);
    assert_eq!(t.entries().filter(|e| !e.name.contains('|')).count(), 254);
    assert_eq!(t.entries().filter(|e| e.is_leaf()).count(), 380);
}

#[test]
fn an_entry_carries_its_arity_flags_categories_and_key_specs() {
    let t = table();
    let get = t.get("get").unwrap();
    assert_eq!(get.arity, 2);
    assert!(get.has_flag("readonly") && get.has_flag("fast") && !get.has_flag("write"));
    assert_eq!(get.categories, ["@read", "@string", "@fast"]);
    assert!(!get.key_spec_writes && get.is_leaf());
    let set = t.get("set").unwrap();
    assert_eq!(set.arity, -3);
    assert!(set.has_flag("write") && set.key_spec_writes);
    let eval = t.get("eval").unwrap();
    assert!(eval.key_spec_writes && !eval.has_flag("write"));
    let pfcount = t.get("pfcount").unwrap();
    assert!(pfcount.key_spec_writes && pfcount.has_flag("readonly"));
    assert!(t.get("save").unwrap().has_flag("no_multi"));
}

#[test]
fn a_container_lists_its_subcommands_by_full_name() {
    let t = table();
    let config = t.get("config").unwrap();
    assert!(!config.is_leaf());
    assert!(config.subcommands.iter().any(|s| s == "config|get"));
    assert!(config.subcommands.iter().all(|s| s.starts_with("config|")));
    let get = t.get("config|get").unwrap();
    assert_eq!((get.arity, get.is_leaf()), (-3, true));
    assert_eq!(get.categories, ["@admin", "@slow", "@dangerous"]);
}

#[test]
fn lookup_ignores_case_and_takes_the_subcommand_from_the_second_word() {
    let t = table();
    assert_eq!(name(&t, &["GET", "k"]).as_deref(), Some("get"));
    assert_eq!(name(&t, &["GET", "get"]).as_deref(), Some("get"));
    assert_eq!(
        name(&t, &["Config", "GET", "maxmemory"]).as_deref(),
        Some("config|get")
    );
    assert_eq!(name(&t, &["client"]).as_deref(), Some("client"));
    assert_eq!(name(&t, &["COMMAND"]).as_deref(), Some("command"));
    assert_eq!(
        name(&t, &["command", "docs", "get"]).as_deref(),
        Some("command|docs")
    );
    assert_eq!(name(&t, &["CONFIG", "NOSUCH"]), None);
    assert_eq!(name(&t, &["NOSUCH", "x"]), None);
    assert_eq!(name(&t, &[]), None);
    assert_eq!(t.entry(&[vec![0xff, 0xfe]]), None);
}

#[test]
fn arrays_stand_in_for_sets_and_a_key_spec_flag_marks_a_write() {
    let t = parse(MODULE.as_bytes()).unwrap();
    let e = t.get("mymod.put").unwrap();
    assert_eq!(e.flags, ["module"]);
    assert_eq!(e.categories, ["@write"]);
    assert!(e.key_spec_writes && e.is_leaf());
    let read_only = MODULE.replace("+OW\r\n+update", "+RO\r\n+access");
    assert!(
        !parse(read_only.as_bytes())
            .unwrap()
            .get("mymod.put")
            .unwrap()
            .key_spec_writes
    );
}

#[test]
fn a_reply_of_the_wrong_shape_is_an_error_not_a_panic() {
    assert!(CommandTable::from_value(&Value::Int(1)).is_err());
    // A pre-7.0 entry has seven fields: no tips, key specs or subcommands.
    assert!(
        parse(
            b"*1\r\n*7\r\n$3\r\nget\r\n:2\r\n~1\r\n+readonly\r\n:1\r\n:1\r\n:1\r\n~1\r\n+@read\r\n"
        )
        .is_err()
    );
    let text_arity = MODULE.replace(":-3", "$2\r\n-3");
    assert!(parse(text_arity.as_bytes()).is_err());
}
