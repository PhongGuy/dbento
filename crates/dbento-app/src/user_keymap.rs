//! The user's `keymap.json`: parsing it into overrides and problems, applying the overrides to
//! the binding table, and finding the conflicts an override creates. Pure apart from `load`,
//! which reads the file and may move a broken one aside.

use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::Path;

use gpui_kit::{Action, Keystroke, SharedString};
use serde_json::Value;

use crate::keymap;

/// One thing wrong with the keymap file, shown in the Keyboard section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeymapProblem {
    /// The entry names no command.
    UnknownId(String),
    /// A key in the entry is empty or does not parse.
    BadKeys {
        /// The entry's command id.
        id: String,
        /// The key as written.
        keys: String,
        /// Why it was rejected.
        reason: String,
    },
    /// The entry's value is not a list of strings.
    NotAList(String),
    /// Several commands answer the same keys where their contexts overlap.
    Conflict {
        /// The keys, as written in the overriding entry.
        keys: String,
        /// The commands involved, sorted.
        ids: Vec<String>,
    },
    /// The parser's message (line and column) or the io error; never file content.
    Unparseable {
        /// The reason, without any of the file's text.
        reason: String,
    },
}

impl KeymapProblem {
    /// One line for the problem list, naming the id and keys.
    pub fn message(&self) -> String {
        match self {
            Self::UnknownId(id) => format!("{id}: no such command"),
            Self::BadKeys { id, keys, reason } => {
                format!("{id}: \"{keys}\" is not a key ({reason})")
            }
            Self::NotAList(id) => format!("{id}: the keys must be a list of strings"),
            Self::Conflict { keys, ids } => format!("{keys}: bound to {}", ids.join(", ")),
            Self::Unparseable { reason } => format!("keymap.json could not be read: {reason}"),
        }
    }
}

/// Usable entries, in serde_json's object iteration order (not promised to be the file's;
/// tests never depend on it): id to its deduplicated keys (empty unbinds). A JSON key repeated
/// in the file collapses to its last value, which is accepted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides(pub Vec<(String, Vec<String>)>);

impl Overrides {
    /// The keys an entry gives `id`, or None when the file leaves it alone.
    pub fn get(&self, id: &str) -> Option<&[String]> {
        self.0
            .iter()
            .find(|(i, _)| i == id)
            .map(|(_, keys)| keys.as_slice())
    }
}

/// The result of parsing a well-formed file.
#[derive(Debug)]
pub struct Parsed {
    /// The entries that apply.
    pub overrides: Overrides,
    /// One problem per skipped entry.
    pub problems: Vec<KeymapProblem>,
}

/// Parses one key (a space-separated chord of strokes) the way GPUI will bind it. A stroke
/// GPUI accepts with an empty key (`"ctrl-shift-"`) is rejected here, since it never fires.
fn parse_keys(keys: &str) -> Result<Vec<Keystroke>, String> {
    let mut strokes = Vec::new();
    for stroke in keys.split_whitespace() {
        let parsed =
            Keystroke::parse(stroke).map_err(|_| format!("\"{stroke}\" does not parse"))?;
        if parsed.key.is_empty() {
            return Err(format!("\"{stroke}\" has no key"));
        }
        strokes.push(parsed);
    }
    if strokes.is_empty() {
        return Err("empty".into());
    }
    Ok(strokes)
}

/// Checks one entry's value: a list of parseable keys, deduplicated by their parsed form.
fn entry_keys(id: &str, value: &Value) -> Result<Vec<String>, KeymapProblem> {
    let not_a_list = || KeymapProblem::NotAList(id.to_string());
    let items = value.as_array().ok_or_else(not_a_list)?;
    let mut seen = Vec::new();
    let mut keys = Vec::new();
    for item in items {
        let text = item.as_str().ok_or_else(not_a_list)?;
        let parsed = parse_keys(text).map_err(|reason| KeymapProblem::BadKeys {
            id: id.to_string(),
            keys: text.to_string(),
            reason,
        })?;
        if !seen.contains(&parsed) {
            seen.push(parsed);
            keys.push(text.to_string());
        }
    }
    Ok(keys)
}

/// Parses the file's text; Err(reason) when it is not JSON or not an object.
pub fn parse(text: &str) -> Result<Parsed, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let Value::Object(map) = value else {
        return Err("the file is not a JSON object".into());
    };
    let known = keymap::command_ids();
    let mut overrides = Vec::new();
    let mut problems = Vec::new();
    for (id, value) in &map {
        if !known.contains(&id.as_str()) {
            problems.push(KeymapProblem::UnknownId(id.clone()));
            continue;
        }
        match entry_keys(id, value) {
            Ok(keys) => overrides.push((id.clone(), keys)),
            Err(problem) => problems.push(problem),
        }
    }
    Ok(Parsed {
        overrides: Overrides(overrides),
        problems,
    })
}

/// One effective binding row.
#[derive(Clone)]
pub struct Row {
    /// The command id.
    pub id: &'static str,
    /// The keystrokes, as GPUI parses them.
    pub keys: SharedString,
    /// Builds the action the keys dispatch.
    pub action: fn() -> Box<dyn Action>,
    /// The key context the binding is limited to; None binds it everywhere.
    pub context: Option<&'static str>,
    /// The shortcut list's name for the binding.
    pub label: Option<&'static str>,
}

/// `keymap::bindings()` with `overrides` applied (see the plan's Decisions for placement).
pub fn effective(overrides: &Overrides) -> Vec<Row> {
    let table = keymap::bindings();
    let mut rows = Vec::with_capacity(table.len());
    let mut replaced: Vec<&str> = Vec::new();
    for b in &table {
        let Some(keys) = overrides.get(b.id) else {
            rows.push(Row {
                id: b.id,
                keys: b.keys.into(),
                action: b.action,
                context: b.context,
                label: b.label,
            });
            continue;
        };
        if replaced.contains(&b.id) {
            continue;
        }
        replaced.push(b.id);
        // The id's rows collapse into its first slot: each distinct context once, every key in it.
        let mut contexts: Vec<&keymap::Binding> = Vec::new();
        for d in table.iter().filter(|d| d.id == b.id) {
            if !contexts.iter().any(|c| c.context == d.context) {
                contexts.push(d);
            }
        }
        for d in contexts {
            rows.extend(keys.iter().map(|k| Row {
                id: d.id,
                keys: k.clone().into(),
                action: d.action,
                context: d.context,
                label: d.label,
            }));
        }
    }
    rows
}

/// Whether two key contexts can be active at once: equal, either global, or one nested in
/// the other (a prefix followed by a space, as in `"QueryTab > Input"` or `"Explorer && !Input"`).
fn contexts_overlap(a: Option<&str>, b: Option<&str>) -> bool {
    let nested = |long: &str, short: &str| {
        long.strip_prefix(short)
            .is_some_and(|rest| rest.starts_with(' '))
    };
    match (a, b) {
        (Some(a), Some(b)) => a == b || nested(a, b) || nested(b, a),
        _ => true,
    }
}

/// Conflicts among `rows` that involve an id in `overrides`; one per (keys, set of ids), ids sorted.
/// Only the app's own rows are checked: the kit's bindings are not part of `rows`.
pub fn conflicts(rows: &[Row], overrides: &Overrides) -> Vec<KeymapProblem> {
    let parsed: Vec<Option<Vec<Keystroke>>> =
        rows.iter().map(|r| parse_keys(&r.keys).ok()).collect();
    let mut seen: Vec<(Vec<Keystroke>, Vec<String>)> = Vec::new();
    let mut found = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let Some(keys) = parsed[i]
            .as_ref()
            .filter(|_| overrides.get(row.id).is_some())
        else {
            continue;
        };
        let mut ids = BTreeSet::from([row.id]);
        for (j, other) in rows.iter().enumerate() {
            if parsed[j].as_ref() == Some(keys) && contexts_overlap(row.context, other.context) {
                ids.insert(other.id);
            }
        }
        if ids.len() < 2 {
            continue;
        }
        let ids: Vec<String> = ids.into_iter().map(String::from).collect();
        let key = (keys.clone(), ids.clone());
        if !seen.contains(&key) {
            seen.push(key);
            found.push(KeymapProblem::Conflict {
                keys: row.keys.to_string(),
                ids,
            });
        }
    }
    found
}

/// What reading the keymap file produced.
#[derive(Debug)]
pub struct FileLoad {
    /// None when the file was unusable (unparseable or unreadable); Some(empty) when missing.
    pub overrides: Option<Overrides>,
    /// Entry problems, then conflicts; or the one `Unparseable`.
    pub problems: Vec<KeymapProblem>,
    /// With `move_bad`, the toast text when the file was unusable; names the file, never its
    /// content.
    pub warning: Option<String>,
}

/// Reads `path`. With `move_bad` an unparseable file goes to `keymap.json.bad`; without it the
/// file stays. Entry problems come first, then conflicts of `effective(overrides)`.
pub fn load(path: &Path, move_bad: bool) -> FileLoad {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => {
            return FileLoad {
                overrides: Some(Overrides::default()),
                problems: Vec::new(),
                warning: None,
            };
        }
        // Unreadable is not moved (renaming may fail the same way), but startup still warns.
        Err(e) => {
            return FileLoad {
                overrides: None,
                problems: vec![KeymapProblem::Unparseable {
                    reason: e.to_string(),
                }],
                warning: move_bad.then(|| {
                    format!(
                        "{} could not be read; the default keys are in use",
                        file_name(path)
                    )
                }),
            };
        }
    };
    match parse(&text) {
        Ok(parsed) => {
            let mut problems = parsed.problems;
            problems.extend(conflicts(&effective(&parsed.overrides), &parsed.overrides));
            FileLoad {
                overrides: Some(parsed.overrides),
                problems,
                warning: None,
            }
        }
        Err(reason) => FileLoad {
            overrides: None,
            problems: vec![KeymapProblem::Unparseable { reason }],
            warning: move_bad.then(|| move_aside(path)),
        },
    }
}

/// Renames a broken file to `<name>.bad` so the next start is clean; returns the toast text,
/// which names only the file.
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "keymap.json".into())
}

fn move_aside(path: &Path) -> String {
    let name = file_name(path);
    let bad = format!("{name}.bad");
    match std::fs::rename(path, path.with_file_name(&bad)) {
        Ok(()) => {
            format!("{name} could not be read and was moved to {bad}; the default keys are in use")
        }
        Err(_) => format!("{name} could not be read; the default keys are in use"),
    }
}
