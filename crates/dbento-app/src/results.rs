//! A statement's result rows, kept as the batches core delivered them.

use std::cmp::Ordering;
use std::sync::Arc;

use dbento_core::{CellRef, ColumnInfo, DocumentRef, ResultBatch};

use crate::grid::{cell_display, is_numeric};
use crate::runner::StatementEnd;

/// One statement's columns and rows. Batches are kept exactly as they arrive, so nothing is
/// converted on receipt; a cell is located by binary search over the batch start rows. They
/// are shared, so a `snapshot` costs no row copies.
pub struct ResultStore {
    columns: Arc<[ColumnInfo]>,
    batches: Vec<Arc<ResultBatch>>,
    starts: Vec<u64>,
    rows: u64,
    heap_bytes: usize,
    sample_chars: Vec<usize>,
    end: Option<StatementEnd>,
    /// While locally sorted: the data column, descending, and the loaded row each shown row reads.
    local: Option<LocalSort>,
}

struct LocalSort {
    col: usize,
    descending: bool,
    order: Vec<u32>,
}

impl ResultStore {
    /// An empty store for a result with `columns`.
    pub fn new(columns: Arc<[ColumnInfo]>) -> Self {
        ResultStore {
            columns,
            batches: Vec::new(),
            starts: Vec::new(),
            rows: 0,
            heap_bytes: 0,
            sample_chars: Vec::new(),
            end: None,
            local: None,
        }
    }

    /// Appends `batch`; an empty batch is ignored so every kept batch owns at least one row.
    pub fn push(&mut self, batch: ResultBatch) {
        if batch.is_empty() {
            return;
        }
        // Only the first batch is sampled, once, so column widths never cost a render scan.
        if self.batches.is_empty() {
            self.sample_chars = (0..batch.column_count())
                .map(|col| {
                    (0..batch.len())
                        .map(|row| cell_display(batch.cell(row, col)).0.chars().count())
                        .max()
                        .unwrap_or(0)
                })
                .collect();
        }
        let first = self.rows;
        self.starts.push(self.rows);
        self.rows += batch.len() as u64;
        self.heap_bytes += batch.heap_bytes();
        self.batches.push(Arc::new(batch));
        // A sorted store merges the new rows into its order: they are sorted alone, then
        // interleaved in one pass, with earlier rows first on a tie.
        if let Some(mut local) = self.local.take() {
            let (col, descending) = (local.col, local.descending);
            let typed = self.typed_numeric(col);
            let mut fresh = self
                .sorted_keys(first..self.rows, col, descending)
                .into_iter()
                .peekable();
            let mut merged = Vec::with_capacity(local.order.len() + fresh.len());
            for row in local.order {
                let key = self.key(row.into(), col, typed);
                while let Some((_, next)) =
                    fresh.next_if(|(k, _)| compare(k, &key, descending).is_lt())
                {
                    merged.push(next);
                }
                merged.push(row);
            }
            merged.extend(fresh.map(|(_, row)| row));
            local.order = merged;
            self.local = Some(local);
        }
    }

    /// Reorders every reader of `cell` by data column `col`: numbers before other text, text
    /// before bytes, NULL last ascending; descending reverses it. Ties keep loaded order.
    pub fn sort_local(&mut self, col: usize, descending: bool) {
        let order = self
            .sorted_keys(0..self.rows, col, descending)
            .into_iter()
            .map(|(_, row)| row)
            .collect();
        self.local = Some(LocalSort {
            col,
            descending,
            order,
        });
    }

    /// Restores loaded order.
    pub fn clear_local_sort(&mut self) {
        self.local = None;
    }

    /// (data column, descending) while sorted.
    pub fn local_sort(&self) -> Option<(usize, bool)> {
        self.local.as_ref().map(|l| (l.col, l.descending))
    }

    /// Loaded rows `rows` with their keys in sort order; each key is computed once.
    fn sorted_keys(
        &self,
        rows: std::ops::Range<u64>,
        col: usize,
        descending: bool,
    ) -> Vec<(SortKey<'_>, u32)> {
        let typed = self.typed_numeric(col);
        let mut keyed: Vec<_> = rows
            .map(|row| (self.key(row, col, typed), row as u32))
            .collect();
        keyed.sort_by(|a, b| compare(&a.0, &b.0, descending));
        keyed
    }

    /// Whether column `col` is declared numeric, as `grid::is_numeric` judges its type.
    fn typed_numeric(&self, col: usize) -> bool {
        self.columns
            .get(col)
            .is_some_and(|c| is_numeric(c.type_name(), c.pg_type_oid()))
    }

    /// Loaded row `row`'s sort key in column `col`. A typed numeric column, or a cell the driver
    /// stored as a number, renders infinities and NaN as words that are still numbers.
    fn key(&self, row: u64, col: usize, typed: bool) -> SortKey<'_> {
        let located = self.locate(row, col);
        let number = typed || located.is_some_and(|(b, r)| b.is_number(r, col));
        sort_key(located.map(|(b, r)| b.cell(r, col)), number)
    }

    /// The result's columns.
    pub fn columns(&self) -> &[ColumnInfo] {
        &self.columns
    }

    /// The number of rows received so far.
    pub fn rows(&self) -> u64 {
        self.rows
    }

    /// The heap bytes owned by the kept batches and any local sort order; 0 for a new store.
    pub fn heap_bytes(&self) -> usize {
        self.heap_bytes
            + self
                .local
                .as_ref()
                .map_or(0, |l| l.order.capacity() * size_of::<u32>())
    }

    /// The widest `grid::cell_display` text of column `col` in the first non-empty batch, in
    /// chars; None before any row arrives. Computed once, in `push`, so reading it is O(1).
    pub fn sample_chars(&self, col: usize) -> Option<usize> {
        self.sample_chars.get(col).copied()
    }

    /// The widest `cell_display` text of column `col` over every loaded row, in chars.
    pub fn max_chars(&self, col: usize) -> usize {
        self.batches
            .iter()
            .filter(|b| col < b.column_count())
            .flat_map(|b| (0..b.len()).map(move |row| b.cell(row, col)))
            .map(|cell| cell_display(cell).0.chars().count())
            .max()
            .unwrap_or(0)
    }

    /// The cell at `row`, `col`, or `None` when either is out of range.
    pub fn cell(&self, row: u64, col: usize) -> Option<CellRef<'_>> {
        if row >= self.rows || col >= self.columns.len() {
            return None;
        }
        let row = match &self.local {
            Some(l) => u64::from(l.order[row as usize]),
            None => row,
        };
        self.loaded_cell(row, col)
    }

    /// The stored document of shown row `row`, following a local sort; None for SQL engines or
    /// past the loaded rows.
    pub fn document(&self, row: u64) -> Option<DocumentRef<'_>> {
        if row >= self.rows {
            return None;
        }
        let row = match &self.local {
            Some(l) => u64::from(l.order[row as usize]),
            None => row,
        };
        let ix = self.starts.partition_point(|&start| start <= row) - 1;
        self.batches[ix].document((row - self.starts[ix]) as usize)
    }

    /// Whether the cell at `row`, `col` was stored as a number, as `ResultBatch::is_number` says;
    /// false when either is out of range.
    pub fn is_number(&self, row: u64, col: usize) -> bool {
        if row >= self.rows || col >= self.columns.len() {
            return false;
        }
        let row = match &self.local {
            Some(l) => u64::from(l.order[row as usize]),
            None => row,
        };
        self.locate(row, col)
            .is_some_and(|(batch, r)| batch.is_number(r, col))
    }

    /// The cell at loaded row `row`, ignoring any local sort.
    fn loaded_cell(&self, row: u64, col: usize) -> Option<CellRef<'_>> {
        self.locate(row, col).map(|(batch, r)| batch.cell(r, col))
    }

    /// The batch holding loaded row `row` and the row's index in it, when it has column `col`.
    fn locate(&self, row: u64, col: usize) -> Option<(&ResultBatch, usize)> {
        if row >= self.rows || col >= self.columns.len() {
            return None;
        }
        // The batch holding `row` is the last one starting at or before it.
        let ix = self.starts.partition_point(|&start| start <= row) - 1;
        let batch: &ResultBatch = &self.batches[ix];
        (col < batch.column_count()).then(|| (batch, (row - self.starts[ix]) as usize))
    }

    /// The rows loaded so far, in the current local order, as a store another thread may own;
    /// it shares the batches, and later pushes or sorts leave it as it was. How the statement
    /// ended is not carried over.
    pub fn snapshot(&self) -> ResultStore {
        ResultStore {
            columns: Arc::clone(&self.columns),
            batches: self.batches.clone(),
            starts: self.starts.clone(),
            rows: self.rows,
            heap_bytes: self.heap_bytes,
            sample_chars: self.sample_chars.clone(),
            end: None,
            local: self.local.as_ref().map(|l| LocalSort {
                col: l.col,
                descending: l.descending,
                order: l.order.clone(),
            }),
        }
    }

    /// Records how the statement ended.
    pub fn finish(&mut self, end: StatementEnd) {
        self.end = Some(end);
    }

    /// Clears the end, so rows can be appended again.
    pub fn reopen(&mut self) {
        self.end = None;
    }

    /// How the statement ended, once it has.
    pub fn end(&self) -> Option<&StatementEnd> {
        self.end.as_ref()
    }
}

/// A cell's place in the local sort order; variants rank in declaration order.
#[derive(PartialEq, PartialOrd)]
enum SortKey<'a> {
    Number(f64),
    Text(&'a str),
    Bytes(&'a [u8]),
    Null,
}

/// `number` says the cell is known numeric, so its "inf" or "NaN" is a number too; otherwise only
/// text with a digit counts, since `f64::parse` also takes those words in any case.
fn sort_key(cell: Option<CellRef<'_>>, number: bool) -> SortKey<'_> {
    match cell {
        Some(CellRef::Text(t)) if number || t.bytes().any(|c| c.is_ascii_digit()) => {
            t.parse::<f64>().map_or(SortKey::Text(t), SortKey::Number)
        }
        Some(CellRef::Text(t)) => SortKey::Text(t),
        Some(CellRef::Bytes(b)) => SortKey::Bytes(b),
        _ => SortKey::Null,
    }
}

fn compare(a: &SortKey<'_>, b: &SortKey<'_>, descending: bool) -> Ordering {
    let ord = match (a, b) {
        (SortKey::Number(x), SortKey::Number(y)) => x.total_cmp(y),
        _ => a.partial_cmp(b).unwrap_or(Ordering::Equal),
    };
    if descending { ord.reverse() } else { ord }
}
