//! The grid's context menu: which items a right-click offers and which apply.

use dbento_core::{
    CellRef, EditValue, Engine, RelationRef, default_schema, quote_ident, single_source,
};
use gpui_kit::App;

use crate::browse::joinable;
use crate::copy::{CopyCells, sql_literal};
use crate::grid::data_col;
use crate::keymap::{DeleteRow, EditCell, all_keys_for, keys_for};
use crate::staged::GridRow;
use crate::tab_model::{CopyKind, SaveState, TabModel};

/// Where a right-click landed, in table indexes (the gutter is column 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GridTarget {
    /// A body cell: table row and table column.
    Cell(usize, usize),
    /// A column header: table column.
    Header(usize),
    /// Below the rows, or nowhere the grid tracks.
    #[default]
    Below,
}

/// Which group of items a menu section holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionKind {
    /// A header's sort, copy-name, hide, show and fit items.
    Column,
    /// The browse result's edits and row copies, `menu_entries`.
    Edit,
    /// Filtering the browse by a column or by a cell's value.
    Filter,
    /// Copying the selection in each format.
    Copy,
}

/// One separated group of the grid menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuSection {
    /// What the section holds.
    pub kind: SectionKind,
    /// Its items in menu order.
    pub entries: Vec<MenuEntry>,
}

/// Column (a header target), then Edit (statement 0 of a browse result: `menu_entries`, Add
/// row alone off a cell), then Filter (a browse header's `header-menu-filter`, a browse cell's
/// `FILTER_OPS`), then Copy (any result with columns), which ends with Export. `target` uses
/// full indexes.
pub fn menu_sections(m: &TabModel, stmt: usize, target: GridTarget, cx: &App) -> Vec<MenuSection> {
    let mut sections = Vec::new();
    let browse = stmt == 0 && m.browse_result();
    let header = match target {
        GridTarget::Header(col) => data_col(col),
        _ => None,
    };
    if let Some(data) = header
        && let Some(entries) = column_entries(m, stmt, data)
    {
        sections.push(MenuSection {
            kind: SectionKind::Column,
            entries,
        });
    }
    if browse {
        let entries = match target {
            GridTarget::Cell(row, col) => menu_entries(m, Some(m.grid_row(row)), data_col(col), cx),
            _ => {
                let mut all = menu_entries(m, None, None, cx);
                all.retain(|e| e.id == "grid-menu-add-row");
                all
            }
        };
        sections.push(MenuSection {
            kind: SectionKind::Edit,
            entries,
        });
    }
    if browse && header.is_some() {
        sections.push(MenuSection {
            kind: SectionKind::Filter,
            entries: vec![MenuEntry {
                id: "header-menu-filter",
                label: "Filter by\u{2026}",
                enabled: filter_joinable(m),
                keys: None,
            }],
        });
    }
    if browse && matches!(target, GridTarget::Cell(..)) {
        let entries = FILTER_OPS
            .iter()
            .map(|&(id, label, _)| MenuEntry {
                id,
                label,
                enabled: filter_clause(m, stmt, target, id).is_some(),
                keys: None,
            })
            .collect();
        sections.push(MenuSection {
            kind: SectionKind::Filter,
            entries,
        });
    }
    let Some(store) = m.results().get(stmt).map(|r| &r.store) else {
        return sections;
    };
    if store.columns().is_empty() {
        return sections;
    }
    // A copy needs a loaded value under the click: an inserted row has none yet.
    let inserted = if stmt == 0 { m.edits().inserted() } else { 0 };
    let copyable = match target {
        GridTarget::Cell(row, _) => row
            .checked_sub(inserted)
            .is_some_and(|r| (r as u64) < store.rows()),
        GridTarget::Header(col) => data_col(col).is_some(),
        GridTarget::Below => false,
    };
    let sql = copyable && insert_table(m, stmt).is_some();
    let entry = |id, label, enabled| MenuEntry {
        id,
        label,
        enabled,
        keys: None,
    };
    sections.push(MenuSection {
        kind: SectionKind::Copy,
        entries: vec![
            MenuEntry {
                keys: keys_for(&CopyCells, cx),
                ..entry("grid-menu-copy", "Copy", copyable)
            },
            entry("grid-menu-copy-headers", "Copy with headers", copyable),
            entry("grid-menu-copy-csv", "Copy as CSV", copyable),
            entry("grid-menu-copy-json", "Copy as JSON", copyable),
            entry("grid-menu-copy-markdown", "Copy as Markdown", copyable),
            entry("grid-menu-copy-sql", "Copy as SQL INSERT", sql),
            // Export writes every loaded row, so it needs rows rather than a target.
            entry("grid-menu-export", "Export…", store.rows() > 0),
        ],
    });
    sections
}

/// A header's items for data column `data` of result `stmt`; None past its columns. A browse
/// sorts the query on the server, any other result sorts its loaded rows in place.
fn column_entries(m: &TabModel, stmt: usize, data: usize) -> Option<Vec<MenuEntry>> {
    let store = &m.results().get(stmt)?.store;
    let name = store.columns().get(data)?.name();
    let (labels, sortable, current, sorted) = if stmt == 0 && m.browse_result() {
        let b = m.browse()?;
        (
            ["Sort ascending", "Sort descending", "Clear sort"],
            b.target().sortable(name),
            b.sort_of(name),
            b.browse().sort.is_some(),
        )
    } else {
        let local = store.local_sort();
        (
            [
                "Sort loaded rows ascending",
                "Sort loaded rows descending",
                "Restore loaded order",
            ],
            store.rows() > 0,
            local.filter(|(col, _)| *col == data).map(|(_, d)| d),
            local.is_some(),
        )
    };
    let view = m.column_view(stmt);
    let visible = view.map_or(store.columns().len(), |v| v.visible_count());
    let entry = |id, label, enabled| MenuEntry {
        id,
        label,
        enabled,
        keys: None,
    };
    Some(vec![
        entry(
            "header-menu-sort-asc",
            labels[0],
            sortable && current != Some(false),
        ),
        entry(
            "header-menu-sort-desc",
            labels[1],
            sortable && current != Some(true),
        ),
        entry("header-menu-sort-clear", labels[2], sorted),
        entry("header-menu-copy-name", "Copy column name", true),
        entry("header-menu-hide", "Hide column", visible > 1),
        entry(
            "header-menu-show-all",
            "Show all columns",
            view.is_some_and(|v| v.any_hidden()),
        ),
        entry("header-menu-fit", "Fit width", true),
    ])
}

/// How a cell filter compares its column with the cell's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterOp {
    Eq,
    Ne,
    IsNull,
}

/// A browse cell's filter items: id, static label and the comparison it writes.
const FILTER_OPS: [(&str, &str, FilterOp); 3] = [
    ("grid-menu-filter-eq", "Filter: equals", FilterOp::Eq),
    ("grid-menu-filter-ne", "Filter: not equal", FilterOp::Ne),
    ("grid-menu-filter-null", "Filter: IS NULL", FilterOp::IsNull),
];

/// The clause entry `id` would apply at `target` (full indexes); None when it does not apply.
/// The value is the cell's loaded one, so an inserted row offers none and a staged edit is
/// ignored. The literal is the one Copy as INSERT writes, so it follows the column type on
/// Postgres and the cell's storage class on SQLite.
pub fn filter_clause(m: &TabModel, stmt: usize, target: GridTarget, id: &str) -> Option<String> {
    let GridTarget::Cell(row, col) = target else {
        return None;
    };
    if stmt != 0 || !m.browse_result() {
        return None;
    }
    let op = FILTER_OPS.iter().find(|(i, ..)| *i == id)?.2;
    let data = data_col(col)?;
    let GridRow::Loaded(loaded) = m.grid_row(row) else {
        return None;
    };
    let cell = m.original_at(GridRow::Loaded(loaded), data)?;
    let store = &m.results().first()?.store;
    let info = store.columns().get(data)?;
    let spec = m.spec();
    let engine = spec.workspace.connection(&spec.connection)?.engine();
    let oid = info.pg_type_oid();
    let literal = sql_literal(
        engine,
        cell,
        info.type_name(),
        oid,
        store.is_number(loaded, data),
    )?;
    let mut column = quote_ident(engine, info.name());
    let clause = match op {
        FilterOp::IsNull => format!("{column} IS NULL"),
        _ if matches!(cell, CellRef::Null) => return None,
        FilterOp::Eq | FilterOp::Ne => {
            if engine == Engine::Postgres && oid.is_some_and(|o| PG_NO_EQUALITY.contains(&o)) {
                column.push_str("::text");
            }
            let cmp = if op == FilterOp::Eq { "=" } else { "<>" };
            format!("{column} {cmp} {literal}")
        }
    };
    filter_joinable(m).then_some(clause)
}

/// Postgres types (and their arrays) with no `=`, or one that is not value equality (box,
/// circle and path compare area or point count): json, xml, point, path, box, polygon,
/// circle, jsonpath, int2vector, pg_node_tree, txid_snapshot, pg_snapshot. A value filter on
/// one compares the column's text form, which is the text the cell shows.
const PG_NO_EQUALITY: [u32; 23] = [
    114, 199, 142, 143, 600, 1017, 602, 1019, 603, 1020, 604, 1027, 718, 719, 4072, 4073, 22, 1006,
    194, 2970, 2949, 5038, 5039,
];

/// Whether the browse's applied filter, which the Filter entries build on, takes a clause.
fn filter_joinable(m: &TabModel) -> bool {
    m.browse()
        .is_none_or(|b| joinable(b.target().engine(), b.browse().filter.as_deref()))
}

/// Longest clause a menu label shows before it is cut.
const LABEL_CLAUSE_CHARS: usize = 40;

/// "Filter: <clause>", the clause cut to 40 chars plus "…".
pub fn filter_label(clause: &str) -> String {
    match clause.char_indices().nth(LABEL_CLAUSE_CHARS) {
        Some((cut, _)) => format!("Filter: {}\u{2026}", &clause[..cut]),
        None => format!("Filter: {clause}"),
    }
}

/// The engine the statement's result ran on, with the browse target, else `single_source`
/// of the statement's SQL.
pub fn insert_table(m: &TabModel, stmt: usize) -> Option<(Engine, RelationRef)> {
    let result = m.results().get(stmt)?;
    let engine = result.engine?;
    if stmt == 0
        && m.browse_result()
        && let Some(b) = m.browse()
    {
        return Some((engine, b.target().relation().clone()));
    }
    sql_source(m, stmt)
}

/// The engine and `single_source` of result `stmt`'s SQL; MySQL's default schema is the tab
/// connection's `default_schema` while `result.epoch == m.epoch()`, else None. None on SQL
/// Server.
pub(crate) fn sql_source(m: &TabModel, stmt: usize) -> Option<(Engine, RelationRef)> {
    let result = m.results().get(stmt)?;
    let engine = result.engine?;
    // SQL Server rows cannot be edited and its columns carry no source, and a bare name's
    // schema is assumed rather than read from the login, so no free result names a table.
    if engine == Engine::Mssql {
        return None;
    }
    // An older epoch ran before a connection edit, so today's database may not be the one it read.
    let schema = (result.epoch == m.epoch())
        .then(|| m.spec().workspace.connection(&m.spec().connection))
        .flatten()
        .and_then(|conn| default_schema(engine, conn));
    Some((
        engine,
        single_source(engine, &result.sql, schema.as_deref())?,
    ))
}

pub use crate::labels::MenuEntry;

/// Every item in menu order for a right-click on `at` (None: below the rows) and data column
/// `col` (None: the gutter); empty unless `m.browse_result()`.
pub fn menu_entries(
    m: &TabModel,
    at: Option<GridRow>,
    col: Option<usize>,
    cx: &App,
) -> Vec<MenuEntry> {
    if !m.browse_result() {
        return Vec::new();
    }
    let cell = at.zip(col);
    let shown = cell.and_then(|(at, col)| m.settable(at, col));
    let idle = m.save_state() == SaveState::Idle;
    let row_ops = at.is_some() && m.editable() && idle;
    let deleted = matches!(at, Some(GridRow::Loaded(row)) if m.edits().is_deleted(row));
    let copy = |kind| at.is_some_and(|at| m.copy_query(at, kind).is_some());
    let entry = |id, label, enabled| MenuEntry {
        id,
        label,
        enabled,
        keys: None,
    };
    vec![
        MenuEntry {
            // Edit shows its first key (Enter), not the F2 alias GPUI would pick.
            keys: all_keys_for(&EditCell, cx).into_iter().next(),
            ..entry(
                "grid-menu-edit",
                "Edit",
                cell.is_some_and(|(at, col)| m.cell_text_at(at, col).is_some()),
            )
        },
        entry(
            "grid-menu-set-null",
            "Set NULL",
            shown.as_ref().is_some_and(|v| *v != EditValue::Null)
                && col.is_some_and(|col| nullable(m, col)),
        ),
        entry(
            "grid-menu-set-default",
            "Set DEFAULT",
            shown.as_ref().is_some_and(|v| *v != EditValue::Default),
        ),
        entry("grid-menu-add-row", "Add row", m.editable() && idle),
        entry("grid-menu-duplicate-row", "Duplicate row", row_ops),
        MenuEntry {
            keys: keys_for(&DeleteRow, cx),
            ..entry("grid-menu-delete-row", "Delete row", row_ops && !deleted)
        },
        entry(
            "grid-menu-revert-row",
            "Revert row",
            idle && at.is_some_and(|at| m.has_change(at)),
        ),
        entry(
            "grid-menu-copy-insert",
            "Copy row as INSERT",
            copy(CopyKind::Insert),
        ),
        entry(
            "grid-menu-copy-update",
            "Copy row as UPDATE",
            copy(CopyKind::Update),
        ),
        entry(
            "grid-menu-copy-delete",
            "Copy row as DELETE",
            copy(CopyKind::Delete),
        ),
    ]
}

/// Whether the browsed table's column behind result column `col` accepts NULL.
fn nullable(m: &TabModel, col: usize) -> bool {
    let Some(name) = m.results().first().and_then(|r| r.store.columns().get(col)) else {
        return false;
    };
    m.browse()
        .and_then(|b| b.target().column(name.name()))
        .is_some_and(|c| c.meta().nullable())
}
