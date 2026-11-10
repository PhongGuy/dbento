//! The server's `COMMAND` reply, read into a table of commands and subcommands.

use std::collections::HashMap;

use ::redis::Value;

/// The key-spec flags that mean a key may be changed.
const WRITE_KEY_FLAGS: [&str; 6] = ["rw", "ow", "rm", "insert", "update", "delete"];

/// One `COMMAND` entry: a command, or a subcommand named `parent|sub`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    /// Lowercase, as `get` or `config|get`.
    pub(crate) name: String,
    /// The server's arity: n means exactly n words, -n at least n, the name included.
    pub(crate) arity: i64,
    /// Lowercase flags, as `readonly`, `write` or `no_multi`.
    pub(crate) flags: Vec<String>,
    /// ACL categories as sent, as `@read`, in the server's order.
    pub(crate) categories: Vec<String>,
    /// A key spec carries `RW`, `OW`, `RM`, `insert`, `update` or `delete`.
    pub(crate) key_spec_writes: bool,
    /// Full lowercase names of the subcommand entries; empty for a leaf.
    pub(crate) subcommands: Vec<String>,
}

impl Entry {
    pub(crate) fn has_flag(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f == flag)
    }

    pub(crate) fn is_leaf(&self) -> bool {
        self.subcommands.is_empty()
    }
}

/// Every entry the server reported, in its order, each parent before its subcommands.
#[derive(Debug, Clone)]
pub(crate) struct CommandTable {
    entries: Vec<Entry>,
    by_name: HashMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the server's COMMAND reply is malformed: {0}")]
pub(crate) struct TableError(pub(crate) String);

impl CommandTable {
    pub(crate) fn from_value(reply: &Value) -> Result<CommandTable, TableError> {
        let mut table = CommandTable {
            entries: Vec::new(),
            by_name: HashMap::new(),
        };
        for item in list(reply, "the reply")? {
            table.push(item)?;
        }
        Ok(table)
    }

    /// The entry a statement's words name. A container followed by a word resolves
    /// `first|second`, and an unknown subcommand there is `None`; a leaf ignores later words.
    pub(crate) fn entry(&self, args: &[Vec<u8>]) -> Option<&Entry> {
        let word = |i: usize| {
            let w = std::str::from_utf8(args.get(i)?).ok()?;
            (!w.is_empty()).then(|| w.to_ascii_lowercase())
        };
        let first = self.get(&word(0)?)?;
        if first.is_leaf() || args.len() < 2 {
            return Some(first);
        }
        self.get(&format!("{}|{}", first.name, word(1)?))
    }

    pub(crate) fn get(&self, name: &str) -> Option<&Entry> {
        self.by_name.get(name).map(|&i| &self.entries[i])
    }

    pub(crate) fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter()
    }

    /// Read one entry and, depth first, its subcommands, keeping the parent ahead of them.
    fn push(&mut self, item: &Value) -> Result<(), TableError> {
        let fields = list(item, "an entry")?;
        if fields.len() < 10 {
            return Err(TableError(format!(
                "an entry has {} fields, not at least 10",
                fields.len()
            )));
        }
        let name = text(&fields[0], "a command name")?.to_ascii_lowercase();
        let Value::Int(arity) = fields[1] else {
            return Err(TableError(format!("{name} has a non-integer arity")));
        };
        let words = |v: &Value, what: &str| -> Result<Vec<String>, TableError> {
            list(v, what)?.iter().map(|w| text(w, what)).collect()
        };
        let flags = words(&fields[2], "a flag")?
            .into_iter()
            .map(|f| f.to_ascii_lowercase())
            .collect();
        let categories = words(&fields[6], "an ACL category")?;
        let mut key_spec_writes = false;
        for spec in list(&fields[8], "the key specs")? {
            let Value::Map(pairs) = spec else {
                return Err(TableError(format!(
                    "{name} has a key spec that is not a map"
                )));
            };
            for (key, value) in pairs {
                if text(key, "a key spec field")? == "flags" {
                    key_spec_writes |= words(value, "a key spec flag")?
                        .iter()
                        .any(|f| WRITE_KEY_FLAGS.contains(&f.to_ascii_lowercase().as_str()));
                }
            }
        }
        let index = self.entries.len();
        self.by_name.insert(name.clone(), index);
        self.entries.push(Entry {
            name,
            arity,
            flags,
            categories,
            key_spec_writes,
            subcommands: Vec::new(),
        });
        for sub in list(&fields[9], "the subcommands")? {
            let sub_index = self.entries.len();
            self.push(sub)?;
            let sub_name = self.entries[sub_index].name.clone();
            self.entries[index].subcommands.push(sub_name);
        }
        Ok(())
    }
}

/// An Array or a Set, either of which a server may send for a list.
fn list<'a>(value: &'a Value, what: &str) -> Result<&'a [Value], TableError> {
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "Value has too many variants to list; the rest share one arm"
    )]
    match value {
        Value::Array(items) | Value::Set(items) => Ok(items),
        _ => Err(TableError(format!("{what} is not a list"))),
    }
}

/// A simple or bulk string, as UTF-8.
fn text(value: &Value, what: &str) -> Result<String, TableError> {
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "Value has too many variants to list; the rest share one arm"
    )]
    match value {
        Value::SimpleString(s) => Ok(s.clone()),
        Value::BulkString(b) => {
            String::from_utf8(b.clone()).map_err(|_| TableError(format!("{what} is not UTF-8")))
        }
        _ => Err(TableError(format!("{what} is not a string"))),
    }
}

#[cfg(test)]
mod tests;
