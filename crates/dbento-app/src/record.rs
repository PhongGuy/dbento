//! One result row as the record form shows it: per-column fields with key markers, the
//! shown value (staged over loaded), and the display, edit and parse rules of a field.
//! GPUI-free apart from reading a `TabModel`.

use dbento_core::{CellRef, EditValue, FkDirection, RelationRef, key_filter};

use crate::copy::cell_copy;
use crate::staged::{self, GridRow};
use crate::tab_model::TabModel;

/// The key role a column plays in the browsed table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    /// Part of the primary key.
    PrimaryKey,
    /// Part of an outgoing foreign key.
    ForeignKey,
    /// A row locator such as `ctid` or `rowid`.
    Locator,
}

/// One column of one row as the record form shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordField {
    /// The result column.
    pub col: usize,
    /// The column name.
    pub name: String,
    /// `ColumnInfo::type_name`.
    pub type_name: String,
    /// Locator, else PrimaryKey, else ForeignKey; only on the browse result.
    pub marker: Option<Marker>,
    /// The referenced table of the first outgoing key on this column.
    pub references: Option<String>,
    /// Staged over loaded; DEFAULT for an unset inserted cell.
    pub value: EditValue,
    /// `TabModel::settable(at, col).is_some()`.
    pub editable: bool,
    /// The cell has a staged value.
    pub modified: bool,
    /// What the cell was loaded with; only for a staged cell of a loaded row.
    pub was: Option<Was>,
    /// `staged::fresh_text` of what a reload found for a staged cell of a conflicting row.
    pub fresh: Option<String>,
}

/// The "was" line under a staged field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Was {
    /// "was: " + `original_text(loaded, ORIGINAL_CAP)`.
    pub text: String,
    /// The whole loaded value (`copy::cell_copy`) when the text was capped.
    pub copy: Option<String>,
}

impl Was {
    /// The line for a loaded cell; the whole value rides along only when the text is cut.
    pub fn of(cell: CellRef<'_>) -> Self {
        // Bytes past the eight `original_text` shows are cut as surely as long text.
        let capped = match cell {
            CellRef::Text(s) => s.chars().count() > ORIGINAL_CAP,
            CellRef::Bytes(b) => b.len() > 8,
            _ => false,
        };
        Was {
            text: format!("was: {}", original_text(cell, ORIGINAL_CAP)),
            copy: capped.then(|| cell_copy(cell)),
        }
    }
}

/// The grid row behind table row `row` of result `stmt`: `TabModel::grid_row` on the browse
/// result, else `GridRow::Loaded(row)`.
pub fn record_at(m: &TabModel, stmt: usize, row: usize) -> GridRow {
    if stmt == 0 && m.browse_result() {
        m.grid_row(row)
    } else {
        GridRow::Loaded(row as u64)
    }
}

/// Every column of table row `row` of result `stmt`; empty when there is no such row.
pub fn record_fields(m: &TabModel, stmt: usize, row: usize) -> Vec<RecordField> {
    let Some(result) = m.results().get(stmt) else {
        return Vec::new();
    };
    let store = &result.store;
    // Markers, staging and editing belong to the browse result alone; a free result is
    // shown as loaded and read-only.
    let browse = m.browse().filter(|_| stmt == 0 && m.browse_result());
    let edits = m.edits();
    let at = record_at(m, stmt, row);
    let exists = match at {
        GridRow::Inserted(ix) => ix < edits.inserted(),
        GridRow::Loaded(r) => r < store.rows(),
    };
    if !exists {
        return Vec::new();
    }
    store
        .columns()
        .iter()
        .enumerate()
        .map(|(col, info)| {
            let name = info.name();
            let staged = browse.and(match at {
                GridRow::Inserted(ix) => edits.new_cell(ix, col),
                GridRow::Loaded(r) => edits.cell(r, col),
            });
            let settable = browse.and_then(|_| m.settable(at, col));
            let value = match (settable.clone(), staged, at) {
                (Some(shown), _, _) => shown,
                (None, Some(v), _) => v.clone(),
                (None, None, GridRow::Inserted(_)) => EditValue::Default,
                (None, None, GridRow::Loaded(r)) => loaded_value(store.cell(r, col)),
            };
            let (marker, references) = browse.map_or((None, None), |state| {
                let target = state.target();
                let outgoing = state.foreign_keys().iter().find(|k| {
                    k.direction() == FkDirection::Outgoing && k.columns().iter().any(|c| c == name)
                });
                let marker = if target.locators().iter().any(|l| l == name) {
                    Some(Marker::Locator)
                } else if target
                    .column(name)
                    .is_some_and(|c| c.meta().primary_key().is_some())
                {
                    Some(Marker::PrimaryKey)
                } else {
                    outgoing.map(|_| Marker::ForeignKey)
                };
                (marker, outgoing.map(|k| k.referenced().name().to_owned()))
            });
            let was = match (staged, at) {
                (Some(_), GridRow::Loaded(_)) => m.original_at(at, col),
                _ => None,
            }
            .map(Was::of);
            let fresh = was
                .as_ref()
                .and_then(|_| m.fresh_at(at, col))
                .map(staged::fresh_text);
            RecordField {
                col,
                name: name.to_owned(),
                type_name: info.type_name().to_owned(),
                marker,
                references,
                value,
                editable: settable.is_some(),
                modified: staged.is_some(),
                was,
                fresh,
            }
        })
        .collect()
}

/// A loaded cell as a value; a kind the editor does not know shows as its debug text,
/// never as NULL.
fn loaded_value(cell: Option<CellRef<'_>>) -> EditValue {
    match cell {
        None => EditValue::Null,
        Some(cell) => {
            staged::edit_value(cell).unwrap_or_else(|| EditValue::Text(format!("{cell:?}")))
        }
    }
}

/// One foreign key of a loaded browse row, as a link to the far table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefLink {
    /// Which way the key points from the browsed table.
    pub direction: FkDirection,
    /// The far table: `ForeignKey::other()`.
    pub relation: RelationRef,
    /// The far table's key columns with this row's loaded values.
    pub key: Vec<(String, EditValue)>,
    /// `key_filter` on the target's engine; None with a NULL or unsupported value.
    pub filter: Option<String>,
}

/// The links of `at`, one per `BrowseState::foreign_keys` entry in its order; empty unless `at`
/// is a loaded row of the browse result (`browse_result()`).
pub fn references(m: &TabModel, at: GridRow) -> Vec<RefLink> {
    let (Some(state), Some(result), GridRow::Loaded(row)) = (m.browse(), m.results().first(), at)
    else {
        return Vec::new();
    };
    let store = &result.store;
    if !m.browse_result() || row >= store.rows() {
        return Vec::new();
    }
    let engine = state.target().engine();
    // Values come from the row as loaded, never from staged edits. A cell kind the editor does
    // not know keeps its debug text but cannot key a filter.
    let loaded = |name: &str| -> (EditValue, bool) {
        let cell = store.columns().iter().position(|c| c.name() == name);
        match cell.and_then(|col| store.cell(row, col)) {
            None => (EditValue::Null, true),
            Some(cell) => staged::edit_value(cell).map_or_else(
                || (EditValue::Text(format!("{cell:?}")), false),
                |v| (v, true),
            ),
        }
    };
    state
        .foreign_keys()
        .iter()
        .map(|fk| {
            // An outgoing key matches the referenced columns to this row's own columns; an
            // incoming one matches the referencing columns to this row's referenced columns.
            let (far, near) = if fk.direction() == FkDirection::Incoming {
                (fk.columns(), fk.referenced_columns())
            } else {
                (fk.referenced_columns(), fk.columns())
            };
            let mut known = true;
            let key: Vec<(String, EditValue)> = far
                .iter()
                .zip(near)
                .map(|(far, near)| {
                    let (value, ok) = loaded(near);
                    known &= ok;
                    (far.clone(), value)
                })
                .collect();
            let filter = known.then(|| key_filter(engine, &key)).flatten();
            RefLink {
                direction: fk.direction(),
                relation: fk.other().clone(),
                key,
                filter,
            }
        })
        .collect()
}

/// "id = 1, b = x": each value as `display_text` writes it with no type ("NULL", `\x` hex).
pub fn key_text(key: &[(String, EditValue)]) -> String {
    key.iter()
        .map(|(column, value)| format!("{column} = {}", display_text(value, "")))
        .collect::<Vec<_>>()
        .join(", ")
}

/// "Row i of N" for `Loaded(i - 1)` with `loaded` rows; "New row j" for `Inserted(j - 1)`.
pub fn record_position(at: GridRow, loaded: u64) -> String {
    match at {
        GridRow::Loaded(row) => format!("Row {} of {loaded}", row + 1),
        GridRow::Inserted(ix) => format!("New row {}", ix + 1),
    }
}

/// NULL, DEFAULT, `\x` hex for bytes, `pretty_json` for a json, jsonb or JSON column, else the text.
pub fn display_text(value: &EditValue, type_name: &str) -> String {
    match value {
        EditValue::Null => "NULL".to_owned(),
        EditValue::Default => "DEFAULT".to_owned(),
        EditValue::Text(text)
            if type_name.eq_ignore_ascii_case("json")
                || type_name.eq_ignore_ascii_case("jsonb") =>
        {
            pretty_json(text).unwrap_or_else(|| text.clone())
        }
        _ => edit_text(value),
    }
}

/// The editor's text: the text, `\x` hex for bytes, "" for NULL and DEFAULT.
pub fn edit_text(value: &EditValue) -> String {
    match value {
        EditValue::Null | EditValue::Default => String::new(),
        EditValue::Text(text) => text.clone(),
        EditValue::Bytes(bytes) => cell_copy(CellRef::Bytes(bytes)),
        other => format!("{other:?}"),
    }
}

/// Characters of loaded text the "was" line keeps before its ellipsis.
pub const ORIGINAL_CAP: usize = 200;

/// A loaded value for the "was" line, always on one line: "NULL", `""` for empty text, text
/// past `cap` chars cut with "…", and text quoted with `\`, `"`, `\n`, `\r`, `\t` escaped when
/// it has whitespace at an edge, a line break or tab, or a `"` at an edge; bytes as
/// `BLOB · <size> · \x<first 8 bytes>`, and "(unsupported value)" otherwise.
pub fn original_text(cell: CellRef<'_>, cap: usize) -> String {
    match cell {
        CellRef::Null => "NULL".to_owned(),
        CellRef::Text("") => "\"\"".to_owned(),
        CellRef::Text(s) => {
            let mut body: String = s.chars().take(cap).collect();
            if body.len() < s.len() {
                body.push('\u{2026}');
            }
            // Quotes are needed only where the bare text would hide whitespace, break the
            // line, or read as quoted itself.
            let edge = |c: char| c.is_whitespace() || c == '"';
            let quoted = s.starts_with(edge) || s.ends_with(edge) || s.contains(['\n', '\r', '\t']);
            if !quoted {
                return body;
            }
            let mut escaped = String::with_capacity(body.len() + 2);
            escaped.push('"');
            for c in body.chars() {
                match c {
                    '\\' => escaped.push_str("\\\\"),
                    '"' => escaped.push_str("\\\""),
                    '\n' => escaped.push_str("\\n"),
                    '\r' => escaped.push_str("\\r"),
                    '\t' => escaped.push_str("\\t"),
                    c => escaped.push(c),
                }
            }
            escaped.push('"');
            escaped
        }
        CellRef::Bytes(b) => {
            let size = match b.len() {
                1 => "1 byte".to_owned(),
                n if n < 1024 => format!("{n} bytes"),
                // Units switch on the rounded figure, so nothing reads "1024.0 KB".
                n if n * 20 < 1024 * 20479 => format!("{:.1} KB", n as f64 / 1024.0),
                n => format!("{:.1} MB", n as f64 / (1024.0 * 1024.0)),
            };
            if b.is_empty() {
                return format!("BLOB \u{b7} {size}");
            }
            let hex: String = b.iter().take(8).map(|byte| format!("{byte:02x}")).collect();
            let more = if b.len() > 8 { "\u{2026}" } else { "" };
            format!("BLOB \u{b7} {size} \u{b7} \\x{hex}{more}")
        }
        _ => "(unsupported value)".to_owned(),
    }
}

/// A bytes value, or a `bytea` or `blob` column (any case).
pub fn edits_as_bytes(field: &RecordField) -> bool {
    matches!(field.value, EditValue::Bytes(_))
        || field.type_name.eq_ignore_ascii_case("bytea")
        || field.type_name.eq_ignore_ascii_case("blob")
}

/// What editor text stages over `shown`: Ok(None) for no change. Empty text keeps a shown
/// NULL or DEFAULT; a bytes field takes `\x` plus hex pairs, or "" for empty bytes.
pub fn parse_edit(text: &str, shown: &EditValue, bytes: bool) -> Result<Option<EditValue>, String> {
    // Text equal to what the editor opened with is no change, whatever the field parses as;
    // this also keeps a shown NULL or DEFAULT under empty text.
    if text == edit_text(shown) {
        return Ok(None);
    }
    let value = if bytes {
        EditValue::Bytes(parse_hex(text).ok_or("bytes are written as \\x and hex digits")?)
    } else {
        EditValue::Text(text.to_owned())
    };
    Ok((value != *shown).then_some(value))
}

/// `\x` followed by pairs of hex digits (either case); "" is no bytes.
fn parse_hex(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() {
        return Some(Vec::new());
    }
    let digits = text.strip_prefix("\\x")?.as_bytes();
    if digits.len() % 2 != 0 || !digits.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    digits
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

/// JSON re-indented two spaces, numbers and key order as written; None when not JSON.
pub fn pretty_json(text: &str) -> Option<String> {
    // Validate only; the output re-indents the original tokens so no number is reprinted.
    serde_json::from_str::<serde_json::Value>(text).ok()?;
    fn newline(out: &mut String, depth: usize) {
        out.push('\n');
        out.push_str(&"  ".repeat(depth));
    }
    let mut out = String::with_capacity(text.len() * 2);
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    let mut chars = text.trim().chars().peekable();
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if esc {
                esc = false
            } else if c == '\\' {
                esc = true
            } else if c == '"' {
                in_str = false
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c)
            }
            '{' | '[' => {
                out.push(c);
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
                // An empty container stays on one line.
                match chars.peek() {
                    Some('}' | ']') => out.extend(chars.next()),
                    _ => {
                        depth += 1;
                        newline(&mut out, depth)
                    }
                }
            }
            '}' | ']' => {
                depth -= 1;
                newline(&mut out, depth);
                out.push(c)
            }
            ',' => {
                out.push(c);
                newline(&mut out, depth)
            }
            ':' => out.push_str(": "),
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    Some(out)
}
