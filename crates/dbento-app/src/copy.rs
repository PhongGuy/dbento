//! Copying the grid's selection as text: a cell raw, rows and columns as TSV, or rows in
//! one of the other `CopyFormat`s.

use std::fmt::Write as _;
use std::ops::Range;

use dbento_core::{CellRef, Engine, RelationRef, quote_ident, quote_literal};
use gpui_kit::actions;
use gpui_kit::component::table::TableSelection;

use crate::grid::{data_col, is_numeric};
use crate::results::ResultStore;

actions!(
    dbento,
    [
        /// Copies the grid's selection as TSV.
        CopyCells
    ]
);

/// The whole value: NULL is empty, text unclipped, bytes as `\x` and every byte in hex.
pub fn cell_copy(cell: CellRef<'_>) -> String {
    match cell {
        CellRef::Null => String::new(),
        CellRef::Text(s) => s.to_owned(),
        CellRef::Bytes(b) => {
            let mut out = String::from("\\x");
            for byte in b {
                let _ = write!(out, "{byte:02x}");
            }
            out
        }
        // A kind core adds later is flagged, never copied as empty.
        _ => "(unsupported value)".to_owned(),
    }
}

/// A TSV field: quoted, with `"` doubled, when it holds a tab, CR, LF or `"`.
fn field(cell: CellRef<'_>) -> String {
    let text = cell_copy(cell);
    if text.contains(['\t', '\r', '\n', '"']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    }
}

fn row_text(store: &ResultStore, row: u64) -> Option<String> {
    let fields: Option<Vec<String>> = (0..store.columns().len())
        .map(|col| store.cell(row, col).map(field))
        .collect();
    fields.map(|f| f.join("\t"))
}

/// The text for `sel`, with table columns mapped through `grid::data_col`. A cell is raw. A row
/// (or a gutter cell) is its data cells joined by tabs. A data column is its loaded rows joined
/// by newlines. Row and column fields holding a tab, CR, LF or `"` are quoted, with `"` doubled.
/// None for nothing selected, the gutter column, or out-of-range indexes.
pub fn copy_text(store: &ResultStore, sel: TableSelection) -> Option<String> {
    match sel {
        TableSelection::None => None,
        TableSelection::Row(row) | TableSelection::Cell(row, 0) => row_text(store, row as u64),
        TableSelection::Cell(row, col) => store.cell(row as u64, data_col(col)?).map(cell_copy),
        TableSelection::Column(col) => {
            let col = data_col(col).filter(|c| *c < store.columns().len())?;
            let fields: Vec<String> = (0..store.rows())
                .filter_map(|row| store.cell(row, col).map(field))
                .collect();
            Some(fields.join("\n"))
        }
    }
}

/// How copied or exported rows are written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyFormat {
    /// Tab-separated, fields quoted as `copy_text` quotes them.
    Tsv,
    /// Comma-separated, a field holding `,`, `"`, CR or LF quoted with `"` doubled.
    Csv,
    /// An array of objects, one per row, keyed by column name in column order.
    Json,
    /// A Markdown table.
    Markdown,
    /// One INSERT statement per row into `table`.
    Insert {
        /// The dialect the literals and identifiers follow.
        engine: Engine,
        /// The table the statements insert into.
        table: RelationRef,
        /// Per result column, the column of `table` it writes; None leaves it out.
        columns: Vec<Option<String>>,
    },
}

/// Rows `rows` and data columns `cols` of `store` as `format`; the renderer `copy_as` and
/// `export::export_text` share. Rows past the loaded ones, unknown columns, a row missing a cell
/// (as in `copy_text`) and an INSERT row holding a value it cannot write are left out; `headers`
/// adds a header line to TSV and CSV, while the other formats always name columns.
pub fn copy_rows(
    store: &ResultStore,
    rows: Range<u64>,
    cols: &[usize],
    format: &CopyFormat,
    headers: bool,
) -> String {
    let columns = store.columns();
    let cols: Vec<usize> = cols
        .iter()
        .copied()
        .filter(|c| *c < columns.len())
        .collect();
    let names: Vec<&str> = cols.iter().map(|&c| columns[c].name()).collect();
    let loaded = rows.start.min(store.rows())..rows.end.min(store.rows());
    // Every row as (cell, column type, Postgres oid, stored as a number), the inputs each format
    // needs; a row the store cannot supply whole is skipped rather than given a made-up cell.
    let cells = |row: u64| {
        cols.iter()
            .map(|&c| {
                let info = &columns[c];
                let cell = store.cell(row, c)?;
                Some((
                    cell,
                    info.type_name(),
                    info.pg_type_oid(),
                    store.is_number(row, c),
                ))
            })
            .collect::<Option<Vec<_>>>()
    };
    let loaded_cells = || loaded.clone().filter_map(cells);
    let delimited = |sep: &str, quote: fn(&str) -> String, lines: &mut Vec<String>| {
        if headers {
            lines.push(names.iter().map(|n| quote(n)).collect::<Vec<_>>().join(sep));
        }
        for row in loaded_cells() {
            let fields: Vec<String> = row
                .into_iter()
                .map(|(c, ..)| quote(&cell_copy(c)))
                .collect();
            lines.push(fields.join(sep));
        }
    };
    let mut lines = Vec::new();
    match format {
        CopyFormat::Tsv => delimited("\t", tsv_quote, &mut lines),
        CopyFormat::Csv => delimited(",", csv_quote, &mut lines),
        CopyFormat::Json => {
            // Rows that carry a stored document copy it whole, since the cells are only a flat
            // view of it; any row without one falls back to the cell objects for every row.
            let documents: Option<Vec<String>> = loaded
                .clone()
                .map(|row| {
                    let doc = store.document(row)?.to_extended_json(false);
                    Some(format!("  {}", document_json(&doc)))
                })
                .collect();
            if let Some(documents) = documents.filter(|d| !d.is_empty()) {
                return format!("[\n{}\n]", documents.join(",\n"));
            }
            let objects: Vec<String> = loaded_cells()
                .map(|row| {
                    let pairs: Vec<String> = names
                        .iter()
                        .zip(row)
                        .map(|(name, (cell, t, oid, num))| {
                            let value = json_literal(cell, t, oid, num);
                            format!("{}: {}", json_string(name), value)
                        })
                        .collect();
                    format!("  {{{}}}", pairs.join(", "))
                })
                .collect();
            if objects.is_empty() {
                return "[]".to_owned();
            }
            return format!("[\n{}\n]", objects.join(",\n"));
        }
        CopyFormat::Markdown => {
            let line = |fields: Vec<String>| format!("| {} |", fields.join(" | "));
            lines.push(line(names.iter().map(|n| markdown_text(n)).collect()));
            lines.push(line(vec!["---".to_owned(); names.len()]));
            for row in loaded_cells() {
                lines.push(line(
                    row.into_iter().map(|(c, ..)| markdown_cell(c)).collect(),
                ));
            }
        }
        CopyFormat::Insert {
            engine,
            table,
            columns: targets,
        } => {
            let target = insert_target(*engine, table);
            // A result label is not a table column: write each mapped column under the name
            // it has in the table, once, since a column named twice fails the INSERT. The
            // labels left out are listed on one comment line.
            let mut written: Vec<String> = Vec::new();
            let writes: Vec<Option<String>> = cols
                .iter()
                .map(|&c| {
                    let name = targets.get(c).cloned().flatten()?;
                    (!written.contains(&name)).then(|| {
                        written.push(name.clone());
                        name
                    })
                })
                .collect();
            let target_of = |i: usize| writes[i].clone();
            let kept: Vec<usize> = (0..cols.len())
                .filter(|&i| target_of(i).is_some())
                .collect();
            let skipped: Vec<String> = (0..cols.len())
                .filter(|&i| target_of(i).is_none())
                .map(|i| names[i].replace("\r\n", " ").replace(['\r', '\n'], " "))
                .collect();
            if !skipped.is_empty() {
                lines.push(format!(
                    "-- Skipped columns not in {target}: {}",
                    skipped.join(", ")
                ));
            }
            let idents: Vec<String> = kept
                .iter()
                .filter_map(|&i| target_of(i))
                .map(|n| quote_ident(*engine, &n))
                .collect();
            for row in loaded_cells() {
                let Some(values) = kept
                    .iter()
                    .map(|&i| {
                        let (cell, t, oid, num) = row[i];
                        sql_literal(*engine, cell, t, oid, num)
                    })
                    .collect::<Option<Vec<String>>>()
                else {
                    continue;
                };
                lines.push(format!(
                    "INSERT INTO {target} ({}) VALUES ({});",
                    idents.join(", "),
                    values.join(", ")
                ));
            }
        }
    }
    lines.join("\n")
}

/// The selection (table indexes, gutter at 0) as `format`; Tsv without headers equals
/// `copy_text`. A cell is one value, a row (or gutter cell) every column, a data column every
/// loaded row. None for nothing selected, the gutter column, or out-of-range indexes.
pub fn copy_as(
    store: &ResultStore,
    sel: TableSelection,
    format: &CopyFormat,
    headers: bool,
) -> Option<String> {
    if *format == CopyFormat::Tsv && !headers {
        return copy_text(store, sel);
    }
    let (rows, cols) = selected(store, sel)?;
    Some(copy_rows(store, rows, &cols, format, headers))
}

/// Whether `format` is an INSERT none of whose selected columns belongs to its table, so the
/// copy would hold no column at all.
pub fn insert_maps_nothing(store: &ResultStore, sel: TableSelection, format: &CopyFormat) -> bool {
    let CopyFormat::Insert { columns, .. } = format else {
        return false;
    };
    selected(store, sel).is_some_and(|(_, cols)| {
        cols.iter()
            .all(|&c| columns.get(c).is_none_or(Option::is_none))
    })
}

/// The `schema.name` an INSERT into `table` names, quoted as `engine` needs.
pub fn insert_target(engine: Engine, table: &RelationRef) -> String {
    format!(
        "{}.{}",
        quote_ident(engine, table.schema()),
        quote_ident(engine, table.name())
    )
}

/// The loaded rows and data columns `sel` (table indexes, gutter at 0) covers; None for nothing
/// selected, the gutter column, or out-of-range indexes.
fn selected(store: &ResultStore, sel: TableSelection) -> Option<(Range<u64>, Vec<usize>)> {
    let width = store.columns().len();
    let data = |col| data_col(col).filter(|c| *c < width);
    let one = |row: usize| {
        Some(row as u64)
            .filter(|r| *r < store.rows())
            .map(|r| r..r + 1)
    };
    let (rows, cols) = match sel {
        TableSelection::None => return None,
        TableSelection::Row(row) | TableSelection::Cell(row, 0) => {
            (one(row)?, (0..width).collect())
        }
        TableSelection::Cell(row, col) => (one(row)?, vec![data(col)?]),
        TableSelection::Column(col) => (0..store.rows(), vec![data(col)?]),
    };
    Some((rows, cols))
}

/// `cell` as a JSON value: null, a boolean for t/true/f/false in a `bool` or `boolean`
/// column, a bare number when the text is a JSON number and the column is numeric or the cell
/// was `stored_number`, else a string (bytes as their `\x` hex text).
pub fn json_literal(
    cell: CellRef<'_>,
    type_name: &str,
    pg_oid: Option<u32>,
    stored_number: bool,
) -> String {
    let CellRef::Text(text) = cell else {
        return match cell {
            CellRef::Null => "null".to_owned(),
            other => json_string(&cell_copy(other)),
        };
    };
    let t = type_name.to_ascii_lowercase();
    if t == "bool" || t == "boolean" {
        match text {
            "t" | "true" => return "true".to_owned(),
            "f" | "false" => return "false".to_owned(),
            _ => {}
        }
    }
    if bare_number(text, type_name, pg_oid, stored_number) {
        text.to_owned()
    } else {
        json_string(text)
    }
}

/// Whether `text` may go unquoted: it reads as a JSON number (so NaN and Infinity do not) in
/// a numeric column, or the engine stored it as a number, as SQLite does per value in an
/// untyped or non-numeric column.
fn bare_number(text: &str, type_name: &str, pg_oid: Option<u32>, stored_number: bool) -> bool {
    let numeric = is_numeric(type_name, pg_oid) || stored_number;
    numeric && text.trim() == text && serde_json::from_str::<serde_json::Number>(text).is_ok()
}

/// A document's relaxed Extended JSON as JSON: as is when it is an object, else (a document
/// that will not decode renders as a comment) quoted as a string.
pub fn document_json(ext_json: &str) -> String {
    if ext_json.starts_with('{') {
        ext_json.to_owned()
    } else {
        json_string(ext_json)
    }
}

fn json_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_owned())
}

fn tsv_quote(text: &str) -> String {
    field(CellRef::Text(text))
}

fn csv_quote(text: &str) -> String {
    if text.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_owned()
    }
}

/// Markdown table text: `|` escaped and line breaks as `<br>`, so a value stays in its cell.
fn markdown_text(text: &str) -> String {
    text.replace('|', "\\|")
        .replace("\r\n", "<br>")
        .replace(['\r', '\n'], "<br>")
}

fn markdown_cell(cell: CellRef<'_>) -> String {
    match cell {
        CellRef::Null => "NULL".to_owned(),
        other => markdown_text(&cell_copy(other)),
    }
}

/// `cell` as an SQL literal for `engine`: NULL, a bare number (as `bare_number` allows, and an
/// infinite SQLite real as `9e999` or `-9e999`), bytes as `X'..'` on SQLite and MySQL,
/// `E'\\x..'::bytea` on Postgres, `'\xHH..'::BLOB` on DuckDB or `0x..` on SQL Server, text as
/// `quote_literal` writes it. None for a kind or engine core adds later, so an INSERT never
/// carries a value it did not read.
pub(crate) fn sql_literal(
    engine: Engine,
    cell: CellRef<'_>,
    type_name: &str,
    pg_oid: Option<u32>,
    stored_number: bool,
) -> Option<String> {
    Some(match cell {
        CellRef::Null => "NULL".to_owned(),
        // SQLite has no infinity literal; an overflowing one reads back infinite, as its quote()
        // writes it. NaN never arrives, since SQLite stores it as NULL.
        CellRef::Text(text) if engine == Engine::Sqlite && stored_number && text == "inf" => {
            "9e999".to_owned()
        }
        CellRef::Text(text) if engine == Engine::Sqlite && stored_number && text == "-inf" => {
            "-9e999".to_owned()
        }
        CellRef::Text(text) if bare_number(text, type_name, pg_oid, stored_number) => {
            text.to_owned()
        }
        // DuckDB reads `X'..'` and `\x00ff` as text, so every byte gets its own escape.
        CellRef::Bytes(bytes) if engine == Engine::Duckdb => {
            let escaped: String = bytes.iter().map(|b| format!("\\x{b:02x}")).collect();
            format!("'{escaped}'::BLOB")
        }
        CellRef::Bytes(_) => {
            let hex = cell_copy(cell);
            match engine {
                Engine::Sqlite | Engine::Mysql => format!("X'{}'", &hex[2..]),
                Engine::Postgres => format!("{}::bytea", quote_literal(engine, &hex)),
                Engine::Mssql => format!("0x{}", &hex[2..]),
                _ => return None,
            }
        }
        CellRef::Text(text) => quote_literal(engine, text),
        _ => return None,
    })
}
