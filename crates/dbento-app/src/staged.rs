//! The staged edit set of a browse tab and the words the save flow shows.
//!
//! Edits of loaded rows are keyed by the row's index in the loaded browse result; each
//! carries the `RowKey` read from its identity cells when it was first staged. Inserted
//! rows are kept apart, in the order added. GPUI-free.

use std::collections::BTreeMap;

use dbento_core::{
    ApplyError, CellRef, Change, ChangeOutcome, EditValue, ExecuteError, RelationRef, RowKey,
};

use crate::record::{ORIGINAL_CAP, display_text, key_text, original_text};

/// Statements the apply prompt shows before counting the rest.
pub const PREVIEW_LINES: usize = 30;

/// Undo steps kept per tab; the oldest is dropped past this.
pub const UNDO_DEPTH: usize = 100;

/// Where a grid row's data lives: a staged inserted row, or a row of the loaded result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GridRow {
    /// Inserted row `n`, in the order added.
    Inserted(usize),
    /// Row `n` of the loaded result.
    Loaded(u64),
}

impl GridRow {
    /// The table row showing this row when `inserted` rows are pinned on top.
    pub fn table_row(self, inserted: usize) -> usize {
        match self {
            GridRow::Inserted(ix) => ix,
            GridRow::Loaded(row) => inserted + row as usize,
        }
    }
}

/// Table row `table_row` when `inserted` rows are pinned on top.
pub fn grid_row(table_row: usize, inserted: usize) -> GridRow {
    if table_row < inserted {
        GridRow::Inserted(table_row)
    } else {
        GridRow::Loaded((table_row - inserted) as u64)
    }
}

/// Cells keyed by data column index: (column name, value).
type Cells = BTreeMap<usize, (String, EditValue)>;

/// The staged change of one loaded row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowEdit {
    /// Cell edits keyed by data column index: (column name, new value).
    Update {
        /// The row's identity at staging time.
        key: RowKey,
        /// The edited cells.
        cells: Cells,
    },
    /// The row is marked deleted.
    Delete {
        /// The row's identity at staging time.
        key: RowKey,
    },
}

/// Staged row edits and the outcome marks of the last rejected save.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StagedEdits {
    rows: BTreeMap<u64, RowEdit>,
    marks: BTreeMap<u64, String>,
    /// Inserted rows in the order added; an absent cell is DEFAULT.
    new_rows: Vec<Cells>,
    /// Marks of inserted rows, keyed by insert index.
    new_marks: BTreeMap<usize, String>,
}

impl StagedEdits {
    /// The number of staged changes: loaded rows plus inserted rows.
    pub fn len(&self) -> usize {
        self.rows.len() + self.new_rows.len()
    }

    /// True when nothing is staged.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.new_rows.is_empty()
    }

    /// True when a row of either kind carries an outcome mark.
    pub fn has_marks(&self) -> bool {
        !self.marks.is_empty() || !self.new_marks.is_empty()
    }

    /// The number of inserted rows.
    pub fn inserted(&self) -> usize {
        self.new_rows.len()
    }

    /// Appends an inserted row holding `(data column, column name, value)` cells; its index.
    /// A DEFAULT cell is dropped, since an absent cell is DEFAULT.
    pub fn add_row(&mut self, cells: Vec<(usize, String, EditValue)>) -> usize {
        let cells = cells
            .into_iter()
            .filter(|(_, _, value)| *value != EditValue::Default)
            .map(|(col, name, value)| (col, (name, value)))
            .collect();
        self.new_rows.push(cells);
        self.new_rows.len() - 1
    }

    /// An inserted row's staged cell; None for an unset (DEFAULT) cell or no such row.
    pub fn new_cell(&self, ix: usize, col: usize) -> Option<&EditValue> {
        self.new_rows.get(ix)?.get(&col).map(|(_, v)| v)
    }

    /// Sets an inserted cell (DEFAULT removes it); false when there is no such row.
    pub fn stage_new(&mut self, ix: usize, col: usize, column: &str, value: EditValue) -> bool {
        let Some(cells) = self.new_rows.get_mut(ix) else {
            return false;
        };
        if value == EditValue::Default {
            cells.remove(&col);
        } else {
            cells.insert(col, (column.to_owned(), value));
        }
        true
    }

    /// Removes inserted row `ix`; later rows and their marks move up. False when absent.
    pub fn remove_new(&mut self, ix: usize) -> bool {
        if ix >= self.new_rows.len() {
            return false;
        }
        self.new_rows.remove(ix);
        self.new_marks = std::mem::take(&mut self.new_marks)
            .into_iter()
            .filter(|&(i, _)| i != ix)
            .map(|(i, mark)| (if i > ix { i - 1 } else { i }, mark))
            .collect();
        true
    }

    /// Drops loaded row `row`'s change and mark; false when it had none.
    pub fn revert_row(&mut self, row: u64) -> bool {
        self.marks.remove(&row);
        self.rows.remove(&row).is_some()
    }

    /// Unstages one cell; drops the row edit and its mark once its last cell goes. False when
    /// not staged.
    pub fn revert_cell(&mut self, row: u64, col: usize) -> bool {
        let Some(RowEdit::Update { cells, .. }) = self.rows.get_mut(&row) else {
            return false;
        };
        if cells.remove(&col).is_none() {
            return false;
        }
        if cells.is_empty() {
            self.revert_row(row);
        }
        true
    }

    /// The outcome mark of an inserted row from the last rejected save.
    pub fn new_mark(&self, ix: usize) -> Option<&str> {
        self.new_marks.get(&ix).map(String::as_str)
    }

    /// Stages `value` for cell `col` of `row`. False (nothing changes) on a deleted row.
    /// `value == *original` unstages the cell, and the row and its mark once its last cell goes.
    pub fn stage(
        &mut self,
        row: u64,
        key: RowKey,
        col: usize,
        column: &str,
        original: &EditValue,
        value: EditValue,
    ) -> bool {
        if value == *original {
            if let Some(RowEdit::Update { cells, .. }) = self.rows.get_mut(&row) {
                cells.remove(&col);
                if cells.is_empty() {
                    self.revert_row(row);
                }
            }
            return !self.is_deleted(row);
        }
        match self.rows.entry(row).or_insert_with(|| RowEdit::Update {
            key,
            cells: BTreeMap::new(),
        }) {
            RowEdit::Update { cells, .. } => {
                cells.insert(col, (column.to_owned(), value));
                true
            }
            RowEdit::Delete { .. } => false,
        }
    }

    /// Marks `row` deleted, replacing any cell edits.
    pub fn delete(&mut self, row: u64, key: RowKey) {
        self.rows.insert(row, RowEdit::Delete { key });
    }

    /// The staged value of a cell, if any.
    pub fn cell(&self, row: u64, col: usize) -> Option<&EditValue> {
        match self.rows.get(&row)? {
            RowEdit::Update { cells, .. } => cells.get(&col).map(|(_, v)| v),
            RowEdit::Delete { .. } => None,
        }
    }

    /// The staged change of a row, if any.
    pub fn row(&self, row: u64) -> Option<&RowEdit> {
        self.rows.get(&row)
    }

    /// True when `row` is marked deleted.
    pub fn is_deleted(&self, row: u64) -> bool {
        matches!(self.rows.get(&row), Some(RowEdit::Delete { .. }))
    }

    /// The outcome mark of a row from the last rejected save.
    pub fn mark(&self, row: u64) -> Option<&str> {
        self.marks.get(&row).map(String::as_str)
    }

    /// One change per staged row and the rows: loaded rows in row order (SET columns in
    /// column order), then inserts in the order added (values in column order).
    pub fn changes(&self) -> (Vec<Change>, Vec<GridRow>) {
        let loaded = self.rows.iter().map(|(&row, edit)| {
            let change = match edit {
                RowEdit::Update { key, cells } => Change::Update {
                    key: key.clone(),
                    set: cells.values().cloned().collect(),
                },
                RowEdit::Delete { key } => Change::Delete { key: key.clone() },
            };
            (change, GridRow::Loaded(row))
        });
        let inserts = self.new_rows.iter().enumerate().map(|(ix, cells)| {
            let values = cells.values().cloned().collect();
            (Change::Insert { values }, GridRow::Inserted(ix))
        });
        loaded.chain(inserts).unzip()
    }

    /// Replaces every mark: `outcome_text` of outcome i marks `rows[i]`.
    pub fn set_outcomes(&mut self, rows: &[GridRow], outcomes: &[ChangeOutcome]) {
        self.clear_marks();
        for (&row, outcome) in rows.iter().zip(outcomes) {
            let Some(text) = outcome_text(outcome) else {
                continue;
            };
            match row {
                GridRow::Loaded(row) => self.marks.insert(row, text),
                GridRow::Inserted(ix) => self.new_marks.insert(ix, text),
            };
        }
    }

    /// Drops every outcome mark.
    pub fn clear_marks(&mut self) {
        self.marks.clear();
        self.new_marks.clear();
    }

    /// Drops every edit, inserted row and mark.
    pub fn clear(&mut self) {
        self.rows.clear();
        self.new_rows.clear();
        self.clear_marks();
    }
}

/// Snapshots of the staged set for undo and redo.
#[derive(Debug, Clone, Default)]
pub struct EditHistory {
    undo: Vec<StagedEdits>,
    redo: Vec<StagedEdits>,
}

impl EditHistory {
    /// Pushes the state before a change, forgets the redo steps, keeps the newest UNDO_DEPTH.
    pub fn record(&mut self, before: StagedEdits) {
        self.redo.clear();
        self.undo.push(before);
        if self.undo.len() > UNDO_DEPTH {
            self.undo.remove(0);
        }
    }

    /// Swaps `edits` with the last undo step, pushing the current one for redo.
    pub fn undo(&mut self, edits: &mut StagedEdits) -> bool {
        let Some(before) = self.undo.pop() else {
            return false;
        };
        self.redo.push(std::mem::replace(edits, before));
        true
    }

    /// Swaps `edits` with the last redo step, pushing the current one for undo.
    pub fn redo(&mut self, edits: &mut StagedEdits) -> bool {
        let Some(after) = self.redo.pop() else {
            return false;
        };
        self.undo.push(std::mem::replace(edits, after));
        true
    }

    /// True when there is a step to undo.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// True when there is a step to redo.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Drops the outcome marks of every step, so a restored step never shows the marks of
    /// a save older than the last one.
    pub fn clear_marks(&mut self) {
        for step in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            step.clear_marks();
        }
    }

    /// Forgets every step.
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

/// The edit value of a loaded cell; None for a kind core adds later.
pub fn edit_value(cell: CellRef<'_>) -> Option<EditValue> {
    match cell {
        CellRef::Null => Some(EditValue::Null),
        CellRef::Text(s) => Some(EditValue::Text(s.to_owned())),
        CellRef::Bytes(b) => Some(EditValue::Bytes(b.to_vec())),
        _ => None,
    }
}

/// "Was: " and the loaded value as `record::original_text` writes it, capped at `ORIGINAL_CAP`.
pub fn was_text(original: CellRef<'_>) -> String {
    format!("Was: {}", original_text(original, ORIGINAL_CAP))
}

/// What a reload of a conflicting row found for one of its cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fresh<'a> {
    /// The database's value now.
    Value(CellRef<'a>),
    /// No row has the key now.
    Gone,
}

/// "now in database: " and the value as `record::original_text` writes it, or "row deleted".
pub fn fresh_text(fresh: Fresh<'_>) -> String {
    match fresh {
        Fresh::Value(cell) => format!("now in database: {}", original_text(cell, ORIGINAL_CAP)),
        Fresh::Gone => "now in database: row deleted".into(),
    }
}

/// `was_text`, then " · " and `fresh_text` once a reload has found the row's current state.
pub fn was_fresh_text(original: CellRef<'_>, fresh: Option<Fresh<'_>>) -> String {
    match fresh {
        Some(fresh) => format!("{} \u{b7} {}", was_text(original), fresh_text(fresh)),
        None => was_text(original),
    }
}

/// The identity cells named in identity order; None when one is missing or unsupported.
pub fn row_key(identity: &[String], row: &[(&str, CellRef<'_>)]) -> Option<RowKey> {
    identity
        .iter()
        .map(|name| {
            let (_, cell) = row.iter().find(|(n, _)| n == name)?;
            Some((name.clone(), edit_value(*cell)?))
        })
        .collect::<Option<Vec<_>>>()
        .map(RowKey::new)
}

/// The row mark for an outcome; None when the change went through or was not sent.
pub fn outcome_text(outcome: &ChangeOutcome) -> Option<String> {
    match outcome {
        ChangeOutcome::Conflict => Some("modified or deleted by someone else".to_owned()),
        ChangeOutcome::Ambiguous(n) => Some(format!("matched {n} rows")),
        ChangeOutcome::Failed(message) => Some(message.clone()),
        ChangeOutcome::Applied(_) | ChangeOutcome::NotAttempted | _ => None,
    }
}

/// The banner for a rejected batch: the first failure wins, then conflicts, then ambiguity.
pub fn rejection_text(outcomes: &[ChangeOutcome]) -> String {
    let count = |want: fn(&ChangeOutcome) -> bool| outcomes.iter().filter(|o| want(o)).count();
    let failed = outcomes.iter().find_map(|o| match o {
        ChangeOutcome::Failed(message) => Some(message.clone()),
        ChangeOutcome::Applied(_)
        | ChangeOutcome::Conflict
        | ChangeOutcome::Ambiguous(_)
        | ChangeOutcome::NotAttempted
        | _ => None,
    });
    let conflicts = count(|o| matches!(o, ChangeOutcome::Conflict));
    let reason = if let Some(message) = failed {
        message
    } else if conflicts > 0 {
        let (rows, verb) = if conflicts == 1 {
            ("row", "was")
        } else {
            ("rows", "were")
        };
        format!("{conflicts} {rows} {verb} modified by someone else")
    } else {
        let ambiguous = count(|o| matches!(o, ChangeOutcome::Ambiguous(_)));
        let changes = if ambiguous == 1 { "change" } else { "changes" };
        format!("{ambiguous} {changes} matched more than one row")
    };
    format!("Nothing was committed: {reason}")
}

/// The banner for a COMMIT that failed: a server refusal, or a lost connection whose
/// outcome is unknown.
pub fn commit_error_text(error: &ApplyError) -> String {
    match error {
        // SQLITE_BUSY and its extended codes: a lock the commit's release could not reach.
        ApplyError::CommitFailed(ExecuteError::Database { code, .. })
            if matches!(code.as_str(), "5" | "261" | "517" | "773") =>
        {
            "Commit failed: another connection still holds the database file (a query or \
             transaction open in another tab, or another program). Nothing was committed."
                .to_owned()
        }
        ApplyError::CommitFailed(ExecuteError::Database { message, .. }) => {
            format!("Commit failed: {message}. Nothing was committed.")
        }
        ApplyError::CommitFailed(e) => format!("Commit failed: {e}. Nothing was committed."),
        ApplyError::ReadOnly
        | ApplyError::Connect(_)
        | ApplyError::Session(_)
        | ApplyError::CommitUnknown(_)
        | ApplyError::Rejected { .. }
        | _ => "The connection closed during COMMIT, so whether the changes were saved is \
              unknown. Run the table query again to check."
            .to_owned(),
    }
}

/// Kept rows the band lists before it counts the rest.
pub const KEPT_SHOWN: usize = 20;

/// "id = 1 · status: shipped · no longer matches filter": the key, then each changed
/// column as `display_text` writes it with no type.
pub fn kept_label(key: &RowKey, changed: &[(String, EditValue)]) -> String {
    let mut parts = vec![key_text(key.values())];
    parts.extend(
        changed
            .iter()
            .map(|(column, value)| format!("{column}: {}", display_text(value, ""))),
    );
    parts.push("no longer matches filter".to_owned());
    parts.join(" \u{b7} ")
}

/// "and N more" for the kept rows past `KEPT_SHOWN`; None when the band shows them all.
pub fn kept_more(total: usize) -> Option<String> {
    (total > KEPT_SHOWN).then(|| format!("and {} more", total - KEPT_SHOWN))
}

/// "N pending change(s)", counting staged rows.
pub fn pending_label(n: usize) -> String {
    format!("{n} pending {}", if n == 1 { "change" } else { "changes" })
}

/// "Discard N pending change(s)?"
pub fn discard_title(n: usize) -> String {
    format!("Discard {}?", pending_label(n))
}

/// The notice after a commit: "Committed N change(s) to schema.table". `changes` counts
/// staged rows, as [`pending_label`] does, so the notice repeats the pill's number.
pub fn committed_text(changes: usize, rel: &RelationRef) -> String {
    let noun = if changes == 1 { "change" } else { "changes" };
    format!(
        "Committed {changes} {noun} to {}.{}",
        rel.schema(),
        rel.name()
    )
}

/// The apply prompt's title.
pub fn prompt_title(rel: &RelationRef) -> String {
    format!("Apply changes to {}.{}", rel.schema(), rel.name())
}

/// The apply prompt's detail line.
pub fn prompt_detail(affected: u64) -> String {
    let rows = if affected == 1 { "row" } else { "rows" };
    format!("Affects {affected} {rows}. Locks are held until you choose.")
}

/// The first `PREVIEW_LINES` statements of `sql` (one per line), then a count of the rest.
pub fn preview(sql: &str) -> String {
    let lines: Vec<&str> = sql.lines().collect();
    if lines.len() <= PREVIEW_LINES {
        return sql.to_owned();
    }
    let rest = lines.len() - PREVIEW_LINES;
    let noun = if rest == 1 { "statement" } else { "statements" };
    format!(
        "{}\n-- and {rest} more {noun}",
        lines[..PREVIEW_LINES].join("\n")
    )
}
