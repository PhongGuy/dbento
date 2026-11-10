//! Compact columnar result batches.
//!
//! A finished batch keeps every cell in one byte arena, column after column, with one end offset
//! per cell and three bitmaps (null, bytes, number). It costs a handful of allocations however
//! many columns it has, since Postgres can close tens of thousands of small batches per result.
//! MongoDB and DynamoDB rows also keep their exact document in a side arena, which costs SQL
//! engines nothing.

#[cfg(feature = "dynamodb")]
use crate::dynamo::render as dynamo_render;
#[cfg(feature = "mongodb")]
use crate::mongo::render;

/// A borrowed view of one result cell.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellRef<'a> {
    /// SQL NULL.
    Null,
    /// A value rendered as text.
    Text(&'a str),
    /// Raw bytes (blobs).
    Bytes(&'a [u8]),
}

/// Name and type of one result column.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfo {
    name: String,
    type_name: String,
    pg_type_oid: Option<u32>,
    source: Option<ColumnSource>,
}

/// The table column a result column reads, as the engine reports it; computed columns have none.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnSource {
    /// The table's OID and the column's attribute number, resolved by `Session::source_columns`.
    #[non_exhaustive]
    Postgres {
        /// `pg_class.oid` of the table.
        table_oid: u32,
        /// `pg_attribute.attnum` of the column.
        column_id: i16,
    },
    /// Names as SQLite's column metadata gives them.
    #[non_exhaustive]
    Sqlite {
        /// The attached database, such as `main`.
        schema: String,
        /// The table.
        table: String,
        /// The column inside the table.
        column: String,
    },
    /// Names as the MySQL column definition gives them, before any alias.
    #[non_exhaustive]
    Mysql {
        /// The database (MySQL's schema).
        schema: String,
        /// The table, not its alias.
        table: String,
        /// The column inside the table, not its alias.
        column: String,
    },
}

impl ColumnInfo {
    pub(crate) fn new(
        name: impl Into<String>,
        type_name: impl Into<String>,
        pg_type_oid: Option<u32>,
    ) -> Self {
        Self {
            name: name.into(),
            type_name: type_name.into(),
            pg_type_oid,
            source: None,
        }
    }

    pub(crate) fn with_source(mut self, source: Option<ColumnSource>) -> Self {
        self.source = source;
        self
    }

    /// The column name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The declared type, or empty when the engine reports none.
    pub fn type_name(&self) -> &str {
        &self.type_name
    }

    /// The Postgres type OID; always `None` for SQLite.
    pub fn pg_type_oid(&self) -> Option<u32> {
        self.pg_type_oid
    }

    /// The table column this result column reads; None for a computed one.
    pub fn source(&self) -> Option<&ColumnSource> {
        self.source.as_ref()
    }
}

/// One column of a batch being built, grown cell by cell until `finish` packs it.
#[derive(Debug, Clone, Default)]
struct Column {
    arena: Vec<u8>,
    ends: Vec<u32>,
    nulls: Vec<u64>,
    bytes: Vec<u64>,
    /// Cells the engine stored as a number; empty unless SQLite reported one.
    numbers: Vec<u64>,
}

fn bit(map: &[u64], i: usize) -> bool {
    map.get(i / 64).is_some_and(|w| w >> (i % 64) & 1 == 1)
}

fn set_bit(map: &mut Vec<u64>, i: usize) {
    if map.len() <= i / 64 {
        map.resize(i / 64 + 1, 0);
    }
    map[i / 64] |= 1 << (i % 64);
}

/// A set of result rows stored column by column.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ResultBatch {
    rows: usize,
    cols: usize,
    /// Every column's cells back to back, column 0 first.
    arena: Vec<u8>,
    /// Where each column's cells begin in `arena`, plus the arena's end: `cols + 1` entries.
    starts: Vec<usize>,
    /// Each cell's end relative to its column's start, at index `col * rows + row`; a column's
    /// data stays under 4 GiB (see `MAX_BATCH_BYTES`) even when the whole arena does not.
    ends: Vec<u32>,
    /// Bitmaps over the same `col * rows + row` index; empty when no cell is marked.
    nulls: Vec<u64>,
    bytes: Vec<u64>,
    numbers: Vec<u64>,
    /// The exact stored form of each row back to back; empty unless the engine has documents.
    documents: Vec<u8>,
    /// Each row's end in `documents`: one per row, or none at all.
    document_ends: Vec<u32>,
    /// How every document in `documents` is encoded.
    doc_format: DocFormat,
}

/// How a batch's documents are encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DocFormat {
    /// MongoDB's raw BSON.
    #[default]
    Bson,
    /// Typed DynamoDB JSON, such as `{"pk":{"S":"a"}}`.
    #[cfg_attr(not(feature = "dynamodb"), allow(dead_code))]
    // only the DynamoDB driver builds it
    DynamoJson,
}

/// The exact stored document behind a row: MongoDB BSON or typed DynamoDB JSON. Rendering needs
/// the engine's feature, without which no batch holds such a document.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentRef<'a> {
    bytes: &'a [u8],
    format: DocFormat,
}

impl DocumentRef<'_> {
    /// The engine's own syntax: mongosh for MongoDB, a PartiQL literal for DynamoDB; one field
    /// per line when `pretty`.
    pub fn to_shell(self, pretty: bool) -> String {
        match self.format {
            DocFormat::Bson => self.bson_shell(pretty),
            DocFormat::DynamoJson => self.dynamo_shell(pretty),
        }
    }

    /// JSON in field order. `canonical` keeps every type (MongoDB extended JSON, or typed
    /// DynamoDB JSON); otherwise it reads as plain JSON.
    pub fn to_extended_json(self, canonical: bool) -> String {
        match self.format {
            DocFormat::Bson => self.bson_extended_json(canonical),
            DocFormat::DynamoJson => self.dynamo_json(canonical),
        }
    }

    fn bson_shell(self, pretty: bool) -> String {
        #[cfg(feature = "mongodb")]
        {
            self.rendered(|document| {
                if pretty {
                    render::shell_pretty(&document)
                } else {
                    render::shell(&bson::Bson::Document(document))
                }
            })
        }
        #[cfg(not(feature = "mongodb"))]
        {
            let _ = pretty;
            String::new()
        }
    }

    fn bson_extended_json(self, canonical: bool) -> String {
        #[cfg(feature = "mongodb")]
        {
            self.rendered(|document| render::extended_json(document, canonical))
        }
        #[cfg(not(feature = "mongodb"))]
        {
            let _ = canonical;
            String::new()
        }
    }

    fn dynamo_shell(self, pretty: bool) -> String {
        #[cfg(feature = "dynamodb")]
        {
            dynamo_render::document_literal(self.bytes, pretty)
        }
        #[cfg(not(feature = "dynamodb"))]
        {
            let _ = pretty;
            String::new()
        }
    }

    // The stored bytes already are typed DynamoDB JSON, so the canonical form is them as text.
    fn dynamo_json(self, canonical: bool) -> String {
        #[cfg(feature = "dynamodb")]
        {
            if canonical {
                String::from_utf8_lossy(self.bytes).into_owned()
            } else {
                dynamo_render::document_plain_json(self.bytes)
            }
        }
        #[cfg(not(feature = "dynamodb"))]
        {
            let _ = canonical;
            String::new()
        }
    }

    // These return text with no error path, so a document that will not decode gives its reason.
    #[cfg(feature = "mongodb")]
    fn rendered(&self, write: impl FnOnce(bson::Document) -> String) -> String {
        bson::RawDocument::from_bytes(self.bytes)
            .map_err(|e| e.to_string())
            .and_then(render::decode)
            .map_or_else(
                |reason| format!("/* undecodable document: {reason} */"),
                write,
            )
    }
}

impl ResultBatch {
    /// The number of rows.
    pub fn len(&self) -> usize {
        self.rows
    }

    /// Whether the batch holds no rows.
    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    /// The number of columns.
    pub fn column_count(&self) -> usize {
        self.cols
    }

    /// The cell at `row`, `col`.
    ///
    /// # Panics
    /// Panics when `row` or `col` is out of range.
    pub fn cell(&self, row: usize, col: usize) -> CellRef<'_> {
        assert!(
            row < self.rows,
            "row {row} out of range ({} rows)",
            self.rows
        );
        assert!(
            col < self.cols,
            "column {col} out of range ({} columns)",
            self.cols
        );
        let i = col * self.rows + row;
        if bit(&self.nulls, i) {
            return CellRef::Null;
        }
        let start = if row == 0 { 0 } else { self.ends[i - 1] };
        let base = self.starts[col];
        let data = &self.arena[base + start as usize..base + self.ends[i] as usize];
        if bit(&self.bytes, i) {
            return CellRef::Bytes(data);
        }
        // Text was valid UTF-8 when pushed; fall back to bytes rather than panic.
        std::str::from_utf8(data).map_or(CellRef::Bytes(data), CellRef::Text)
    }

    /// Whether the cell at `row`, `col` was stored as a number (a SQLite integer or real) rather
    /// than rendered from text. Postgres never marks cells; its column types say it instead.
    pub fn is_number(&self, row: usize, col: usize) -> bool {
        col < self.cols && row < self.rows && bit(&self.numbers, col * self.rows + row)
    }

    /// The exact document behind `row`; `None` for an engine without documents or past the end.
    pub fn document(&self, row: usize) -> Option<DocumentRef<'_>> {
        // Every row has its document or none does; anything else would show a neighbour's.
        if self.document_ends.len() != self.rows {
            return None;
        }
        let end = *self.document_ends.get(row)?;
        let start = row
            .checked_sub(1)
            .map_or(0, |prev| self.document_ends[prev]);
        Some(DocumentRef {
            bytes: &self.documents[start as usize..end as usize],
            format: self.doc_format,
        })
    }

    /// The bytes this batch owns on the heap: its arena, column starts, end offsets, bitmaps and
    /// documents at their capacity. Excludes the struct itself.
    pub fn heap_bytes(&self) -> usize {
        self.arena.capacity()
            + self.starts.capacity() * size_of::<usize>()
            + self.ends.capacity() * size_of::<u32>()
            + (self.nulls.capacity() + self.bytes.capacity() + self.numbers.capacity())
                * size_of::<u64>()
            + self.documents.capacity()
            + self.document_ends.capacity() * size_of::<u32>()
    }

    /// The rows from `n` on, same columns; empty when `n >= len`.
    pub fn skip(&self, n: usize) -> ResultBatch {
        let n = n.min(self.rows);
        let rows = self.rows - n;
        let mut out = ResultBatch::empty(rows, self.cols);
        out.doc_format = self.doc_format;
        // Each column keeps its tail of cells, with ends rebased to the tail's first byte.
        let cut = |col: usize| match n {
            0 => 0,
            _ => self.ends[col * self.rows + n - 1],
        };
        let dropped: usize = (0..self.cols).map(|c| cut(c) as usize).sum();
        out.arena.reserve_exact(self.arena.len() - dropped);
        for col in 0..self.cols {
            let first = col * self.rows;
            let cut = cut(col);
            let base = self.starts[col];
            out.arena
                .extend_from_slice(&self.arena[base + cut as usize..self.starts[col + 1]]);
            out.starts.push(out.arena.len());
            out.ends.extend(
                self.ends[first + n..first + self.rows]
                    .iter()
                    .map(|e| e - cut),
            );
            for row in n..self.rows {
                let (from, to) = (first + row, col * rows + row - n);
                for (src, dst) in [
                    (&self.nulls, &mut out.nulls),
                    (&self.bytes, &mut out.bytes),
                    (&self.numbers, &mut out.numbers),
                ] {
                    if bit(src, from) {
                        set_bit(dst, to);
                    }
                }
            }
        }
        out.shrink_bitmaps();
        // Documents keep their tail the same way; a batch without them has no ends to cut.
        if !self.document_ends.is_empty() {
            let cut = n.checked_sub(1).map_or(0, |last| self.document_ends[last]);
            out.documents = self.documents[cut as usize..].to_vec();
            out.document_ends = self.document_ends[n..].iter().map(|e| e - cut).collect();
        }
        out
    }

    /// A batch of `rows` by `cols` with no cells yet: callers push each column's data in turn.
    fn empty(rows: usize, cols: usize) -> ResultBatch {
        let mut starts = Vec::with_capacity(cols + 1);
        starts.push(0);
        ResultBatch {
            rows,
            cols,
            arena: Vec::new(),
            starts,
            ends: Vec::with_capacity(rows * cols),
            nulls: Vec::new(),
            bytes: Vec::new(),
            numbers: Vec::new(),
            documents: Vec::new(),
            document_ends: Vec::new(),
            doc_format: DocFormat::Bson,
        }
    }

    /// Packs built columns into one arena, sized to fit exactly.
    fn pack(rows: usize, columns: &[Column]) -> ResultBatch {
        let mut out = ResultBatch::empty(rows, columns.len());
        out.arena
            .reserve_exact(columns.iter().map(|c| c.arena.len()).sum());
        for (col, c) in columns.iter().enumerate() {
            out.arena.extend_from_slice(&c.arena);
            out.starts.push(out.arena.len());
            out.ends.extend_from_slice(&c.ends);
            // Only marked columns are walked, so a batch without NULLs or blobs skips this.
            for (src, dst) in [
                (&c.nulls, &mut out.nulls),
                (&c.bytes, &mut out.bytes),
                (&c.numbers, &mut out.numbers),
            ] {
                if src.is_empty() {
                    continue;
                }
                for row in (0..rows).filter(|&r| bit(src, r)) {
                    set_bit(dst, col * rows + row);
                }
            }
        }
        out.shrink_bitmaps();
        out
    }

    /// Bitmaps grow by doubling as bits are set; a finished batch keeps only the words it uses.
    fn shrink_bitmaps(&mut self) {
        self.nulls.shrink_to_fit();
        self.bytes.shrink_to_fit();
        self.numbers.shrink_to_fit();
    }

    #[cfg(test)]
    pub(crate) fn data_capacity(&self) -> usize {
        self.arena.capacity()
    }
}

pub(crate) const MAX_BATCH_ROWS: usize = 1024;
pub(crate) const MAX_BATCH_BYTES: usize = 8 << 20;
/// The largest value either engine returns: Postgres sends a signed int32 field length,
/// and SQLite's `SQLITE_MAX_LENGTH` is 1,000,000,000.
pub(crate) const MAX_CELL_BYTES: usize = i32::MAX as usize;
// A column arena holds at most one batch plus one value, so `u32` offsets always fit.
const _: () = assert!(
    MAX_BATCH_BYTES + MAX_CELL_BYTES <= u32::MAX as usize,
    "a batch plus one cell fits u32 offsets"
);

/// An arena length as a `u32` end offset; the cap above makes the panic unreachable.
#[allow(clippy::expect_used)] // the const assert above proves every arena length fits
fn end_offset(len: usize) -> u32 {
    u32::try_from(len).expect("a column arena grew past 4 GiB")
}

/// Fills a [`ResultBatch`] row by row, closing it at the row and byte caps.
pub(crate) struct BatchBuilder {
    columns: Vec<Column>,
    rows: usize,
    data_bytes: usize,
    /// Cell and document bytes of every row pushed so far; `finish` leaves it alone.
    total: u64,
    documents: Vec<u8>,
    document_ends: Vec<u32>,
    /// Stamped on every batch `finish` closes.
    doc_format: DocFormat,
}

impl BatchBuilder {
    pub(crate) fn new(columns: usize) -> Self {
        Self {
            columns: vec![Column::default(); columns],
            rows: 0,
            data_bytes: 0,
            total: 0,
            documents: Vec::new(),
            document_ends: Vec::new(),
            doc_format: DocFormat::Bson,
        }
    }

    /// Marks every batch this builder closes as holding documents in `format`.
    #[cfg_attr(not(feature = "dynamodb"), allow(dead_code))]
    pub(crate) fn with_document_format(mut self, format: DocFormat) -> Self {
        self.doc_format = format;
        self
    }

    /// Cell and document bytes of every row pushed since creation, across batches.
    pub(crate) fn total_bytes(&self) -> u64 {
        self.total
    }

    /// Appends one row. When the builder already holds rows and this row would take the
    /// data past `MAX_BATCH_BYTES`, the held rows are closed first and returned.
    pub(crate) fn push_row(&mut self, row: &[CellRef<'_>]) -> Option<ResultBatch> {
        self.push_typed_row(row, |_| false)
    }

    /// `push_row`, marking the cells whose index `number` accepts as stored numbers.
    pub(crate) fn push_typed_row(
        &mut self,
        row: &[CellRef<'_>],
        number: impl Fn(usize) -> bool,
    ) -> Option<ResultBatch> {
        self.push_cells(row, number, None)
    }

    /// `push_typed_row` plus the raw document, whose bytes count toward the caps and the total.
    #[cfg_attr(not(any(feature = "mongodb", feature = "dynamodb")), allow(dead_code))]
    pub(crate) fn push_document_row(
        &mut self,
        row: &[CellRef<'_>],
        number: impl Fn(usize) -> bool,
        document: &[u8],
    ) -> Option<ResultBatch> {
        self.push_cells(row, number, Some(document))
    }

    fn push_cells(
        &mut self,
        row: &[CellRef<'_>],
        number: impl Fn(usize) -> bool,
        document: Option<&[u8]>,
    ) -> Option<ResultBatch> {
        debug_assert_eq!(
            row.len(),
            self.columns.len(),
            "a row has one cell per column"
        );
        let cells: usize = row
            .iter()
            .map(|c| match c {
                CellRef::Null => 0,
                CellRef::Text(s) => s.len(),
                CellRef::Bytes(b) => b.len(),
            })
            .sum();
        let size = cells + document.map_or(0, <[u8]>::len);
        let closed =
            (self.rows > 0 && self.data_bytes + size > MAX_BATCH_BYTES).then(|| self.finish());
        let i = self.rows;
        for (ix, (col, cell)) in self.columns.iter_mut().zip(row).enumerate() {
            if number(ix) {
                set_bit(&mut col.numbers, i);
            }
            match cell {
                CellRef::Null => set_bit(&mut col.nulls, i),
                CellRef::Text(s) => col.arena.extend_from_slice(s.as_bytes()),
                CellRef::Bytes(b) => {
                    col.arena.extend_from_slice(b);
                    set_bit(&mut col.bytes, i);
                }
            }
            col.ends.push(end_offset(col.arena.len()));
        }
        if let Some(document) = document {
            self.documents.extend_from_slice(document);
            self.document_ends.push(end_offset(self.documents.len()));
        }
        self.rows += 1;
        // A row without its document would hand every later row its neighbour's.
        debug_assert!(
            self.document_ends.is_empty() || self.document_ends.len() == self.rows,
            "a document batch has one document per row"
        );
        self.data_bytes += size;
        self.total += size as u64;
        closed
    }

    pub(crate) fn is_full(&self) -> bool {
        self.rows >= MAX_BATCH_ROWS || self.data_bytes >= MAX_BATCH_BYTES
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rows == 0
    }

    /// Closes the batch, packed to fit, and starts a fresh one.
    pub(crate) fn finish(&mut self) -> ResultBatch {
        let fresh = vec![Column::default(); self.columns.len()];
        let built = std::mem::replace(&mut self.columns, fresh);
        let rows = std::mem::take(&mut self.rows);
        self.data_bytes = 0;
        let mut batch = ResultBatch::pack(rows, &built);
        batch.documents = std::mem::take(&mut self.documents);
        batch.documents.shrink_to_fit();
        batch.document_ends = std::mem::take(&mut self.document_ends);
        batch.document_ends.shrink_to_fit();
        batch.doc_format = self.doc_format;
        batch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic]
    fn a_row_of_the_wrong_width_is_rejected_in_debug() {
        let mut b = BatchBuilder::new(2);
        b.push_row(&[CellRef::Null]);
    }

    fn rows_of(batch: &ResultBatch) -> Vec<Vec<CellRef<'_>>> {
        (0..batch.len())
            .map(|r| {
                (0..batch.column_count())
                    .map(|c| batch.cell(r, c))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn every_cell_kind_round_trips() {
        let mut b = BatchBuilder::new(3);
        assert!(
            b.push_row(&[
                CellRef::Text("héllo"),
                CellRef::Bytes(&[0, 255]),
                CellRef::Null
            ])
            .is_none()
        );
        assert!(
            b.push_row(&[CellRef::Null, CellRef::Text("x"), CellRef::Bytes(b"")])
                .is_none()
        );
        let batch = b.finish();
        assert_eq!(batch.len(), 2);
        assert_eq!(batch.column_count(), 3);
        assert_eq!(
            rows_of(&batch),
            vec![
                vec![
                    CellRef::Text("héllo"),
                    CellRef::Bytes(&[0, 255]),
                    CellRef::Null
                ],
                vec![CellRef::Null, CellRef::Text("x"), CellRef::Bytes(b"")],
            ]
        );
    }

    #[test]
    fn a_number_mark_survives_skip() {
        let mut b = BatchBuilder::new(1);
        assert!(b.push_typed_row(&[CellRef::Text("1")], |_| true).is_none());
        assert!(b.push_row(&[CellRef::Text("2")]).is_none());
        assert!(b.push_typed_row(&[CellRef::Text("3")], |_| true).is_none());
        let batch = b.finish();
        assert_eq!(
            (0..3).map(|r| batch.is_number(r, 0)).collect::<Vec<_>>(),
            [true, false, true]
        );
        let tail = batch.skip(1);
        assert_eq!(
            (0..2).map(|r| tail.is_number(r, 0)).collect::<Vec<_>>(),
            [false, true]
        );
    }

    #[test]
    fn skip_rebases_every_bitmap_across_the_64_row_word() {
        // Each row's kind and number mark follow its index, so a tail that shifts a bit into
        // the wrong word or position reads back a different kind.
        let kind = |i: usize| match i % 5 {
            0 => CellRef::Null,
            1 | 3 => CellRef::Bytes(&[0xab]),
            _ => CellRef::Text("t"),
        };
        let mut b = BatchBuilder::new(1);
        for i in 0..150 {
            assert!(b.push_typed_row(&[kind(i)], |_| i % 7 == 0).is_none());
        }
        let batch = b.finish();
        for n in [1, 63, 64, 65, 70, 128, 149] {
            let tail = batch.skip(n);
            assert_eq!(tail.len(), 150 - n);
            for r in 0..tail.len() {
                assert_eq!(tail.cell(r, 0), kind(n + r), "skip {n}, row {r}");
                assert_eq!(tail.is_number(r, 0), (n + r) % 7 == 0, "skip {n}, row {r}");
            }
        }
    }

    #[test]
    fn an_empty_string_is_not_null() {
        let mut b = BatchBuilder::new(2);
        assert!(b.push_row(&[CellRef::Text(""), CellRef::Null]).is_none());
        let batch = b.finish();
        assert_eq!(batch.cell(0, 0), CellRef::Text(""));
        assert_eq!(batch.cell(0, 1), CellRef::Null);
    }

    #[test]
    fn a_batch_is_full_at_the_row_cap() {
        let mut b = BatchBuilder::new(1);
        for i in 0..MAX_BATCH_ROWS {
            assert!(!b.is_full(), "full after {i} rows");
            assert!(b.push_row(&[CellRef::Text("r")]).is_none());
        }
        assert!(b.is_full());
        assert_eq!(b.finish().len(), MAX_BATCH_ROWS);
        assert!(b.is_empty());
        assert!(!b.is_full());
    }

    #[test]
    fn a_batch_is_full_at_the_byte_cap() {
        let chunk = "a".repeat(1 << 20);
        let mut b = BatchBuilder::new(1);
        for _ in 0..7 {
            assert!(b.push_row(&[CellRef::Text(&chunk)]).is_none());
        }
        assert!(!b.is_full());
        assert!(
            b.push_row(&[CellRef::Text(&chunk)]).is_none(),
            "exactly 8 MiB still fits"
        );
        assert!(b.is_full());
        assert_eq!(b.finish().len(), 8);
    }

    #[test]
    fn an_oversized_value_gets_a_batch_of_its_own() {
        let big = vec![7u8; MAX_BATCH_BYTES + 1];
        let mut b = BatchBuilder::new(1);
        assert!(b.push_row(&[CellRef::Text("small")]).is_none());
        let before = b
            .push_row(&[CellRef::Bytes(&big)])
            .expect("the small row closes first");
        assert_eq!(rows_of(&before), vec![vec![CellRef::Text("small")]]);
        assert!(b.is_full());
        let own = b.finish();
        assert_eq!(own.len(), 1);
        assert_eq!(own.cell(0, 0), CellRef::Bytes(&big));
    }

    #[test]
    fn heap_bytes_counts_every_buffer_at_capacity() {
        let mut b = BatchBuilder::new(2);
        assert!(b.push_row(&[CellRef::Text("abc"), CellRef::Null]).is_none());
        assert!(
            b.push_row(&[CellRef::Bytes(&[1, 2]), CellRef::Text("z")])
                .is_none()
        );
        let batch = b.finish();
        // Arena 5 + 1, starts 3 x 8, ends 4 x 4; the null bit (cell 2) and the bytes bit
        // (cell 1) take one word each.
        let want = 6 + 3 * size_of::<usize>() + 4 * size_of::<u32>() + 2 * size_of::<u64>();
        assert_eq!(batch.heap_bytes(), want);
    }

    #[test]
    fn heap_bytes_uses_capacity_not_length() {
        let batch = ResultBatch {
            rows: 0,
            cols: 0,
            arena: Vec::with_capacity(10),
            starts: Vec::with_capacity(6),
            ends: Vec::with_capacity(5),
            nulls: Vec::with_capacity(3),
            bytes: Vec::with_capacity(2),
            numbers: Vec::with_capacity(4),
            documents: Vec::with_capacity(7),
            document_ends: Vec::with_capacity(1),
            doc_format: DocFormat::Bson,
        };
        let want = 10 + 6 * size_of::<usize>() + 5 * 4 + (3 + 2 + 4) * 8 + 7 + 4;
        assert_eq!(batch.heap_bytes(), want);
    }

    #[test]
    fn a_finished_batch_costs_a_few_bytes_per_column_beyond_its_cells() {
        // Postgres closes a batch whenever the socket runs dry, so a wide result can hold tens
        // of thousands of small batches; a fixed cost per column per batch adds up across them.
        let cols = 20;
        let mut b = BatchBuilder::new(cols);
        let row = vec![CellRef::Text("x"); cols];
        assert!(b.push_row(&row).is_none());
        let batch = b.finish();
        let cells = cols * (1 + size_of::<u32>());
        let overhead = batch.heap_bytes() - cells;
        assert!(
            overhead <= cols * 8 + 64,
            "{overhead} bytes beyond the cells and their end offsets"
        );
    }

    #[test]
    fn finish_shrinks_buffers_to_fit() {
        let mut b = BatchBuilder::new(1);
        assert!(b.push_row(&[CellRef::Text("abc")]).is_none());
        assert_eq!(b.finish().data_capacity(), 3);
    }

    #[test]
    fn an_end_offset_up_to_u32_max_is_kept() {
        assert_eq!(end_offset(0), 0);
        assert_eq!(end_offset(u32::MAX as usize), u32::MAX);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    #[should_panic(expected = "4 GiB")]
    fn an_end_offset_past_u32_max_panics_instead_of_wrapping() {
        end_offset(u32::MAX as usize + 1);
    }

    #[test]
    fn total_bytes_counts_every_row_across_finishes() {
        let mut b = BatchBuilder::new(2);
        assert!(b.push_row(&[CellRef::Text("abc"), CellRef::Null]).is_none());
        b.finish();
        assert!(
            b.push_row(&[CellRef::Bytes(&[1, 2]), CellRef::Text("z")])
                .is_none()
        );
        assert_eq!(b.total_bytes(), 6);
    }

    #[test]
    fn a_document_row_keeps_its_bytes_and_counts_them() {
        let mut b = BatchBuilder::new(1);
        assert!(
            b.push_document_row(&[CellRef::Text("a")], |_| false, b"one")
                .is_none()
        );
        assert!(
            b.push_document_row(&[CellRef::Null], |_| false, b"three")
                .is_none()
        );
        assert_eq!(b.total_bytes(), 9);
        let batch = b.finish();
        assert_eq!(batch.document(0).unwrap().bytes, b"one");
        assert_eq!(batch.document(1).unwrap().bytes, b"three");
        assert!(batch.document(2).is_none());
    }

    #[test]
    fn a_batch_without_documents_answers_none() {
        let mut b = BatchBuilder::new(1);
        assert!(b.push_row(&[CellRef::Text("a")]).is_none());
        assert!(b.finish().document(0).is_none());
    }

    #[test]
    fn skip_keeps_rows_and_documents_aligned() {
        let mut b = BatchBuilder::new(1);
        for (cell, document) in [("a", &b"one"[..]), ("b", b"three"), ("c", b"")] {
            assert!(
                b.push_document_row(&[CellRef::Text(cell)], |_| false, document)
                    .is_none()
            );
        }
        let tail = b.finish().skip(1);
        assert_eq!(tail.cell(0, 0), CellRef::Text("b"));
        assert_eq!(tail.document(0).unwrap().bytes, b"three");
        assert_eq!(tail.document(1).unwrap().bytes, b"");
        assert!(tail.document(2).is_none());
    }

    #[test]
    fn skip_on_a_batch_without_documents_stays_without() {
        let mut b = BatchBuilder::new(1);
        assert!(b.push_row(&[CellRef::Text("a")]).is_none());
        assert!(b.push_row(&[CellRef::Text("b")]).is_none());
        let tail = b.finish().skip(1);
        assert_eq!(tail.cell(0, 0), CellRef::Text("b"));
        assert!(tail.document(0).is_none());
    }

    #[test]
    fn a_batch_whose_documents_do_not_match_its_rows_hands_out_none() {
        let mut b = BatchBuilder::new(1);
        b.push_document_row(&[CellRef::Text("x")], |_| false, b"one");
        let mut batch = b.finish();
        assert!(batch.document(0).is_some());
        // The state a release build reaches if a row ever lands without its document.
        batch.rows = 2;
        assert!(batch.document(0).is_none());
    }

    #[test]
    fn documents_count_toward_heap_bytes_and_close_a_batch_at_the_cap() {
        let big = vec![1u8; MAX_BATCH_BYTES];
        let mut b = BatchBuilder::new(1);
        assert!(
            b.push_document_row(&[CellRef::Text("x")], |_| false, b"small")
                .is_none()
        );
        let before = b.push_document_row(&[CellRef::Text("y")], |_| false, &big);
        assert_eq!(before.expect("the small row closes first").len(), 1);
        assert!(b.is_full());
        let own = b.finish();
        assert!(own.heap_bytes() >= MAX_BATCH_BYTES);
        assert_eq!(own.document(0).unwrap().bytes.len(), MAX_BATCH_BYTES);
    }

    #[test]
    fn a_builder_stamps_its_document_format_on_every_batch_and_skip_keeps_it() {
        let mut b = BatchBuilder::new(1).with_document_format(DocFormat::DynamoJson);
        for doc in [&b"{}"[..], b"{\"a\":{\"S\":\"x\"}}"] {
            assert!(
                b.push_document_row(&[CellRef::Null], |_| false, doc)
                    .is_none()
            );
        }
        let batch = b.finish();
        assert_eq!(batch.document(0).unwrap().format, DocFormat::DynamoJson);
        let tail = batch.skip(1);
        assert_eq!(tail.document(0).unwrap().format, DocFormat::DynamoJson);
        assert_eq!(tail.document(0).unwrap().bytes, b"{\"a\":{\"S\":\"x\"}}");
        assert!(
            b.push_document_row(&[CellRef::Null], |_| false, b"{}")
                .is_none()
        );
        assert_eq!(
            b.finish().document(0).unwrap().format,
            DocFormat::DynamoJson
        );
        let mut plain = BatchBuilder::new(1);
        assert!(
            plain
                .push_document_row(&[CellRef::Null], |_| false, b"x")
                .is_none()
        );
        assert_eq!(plain.finish().document(0).unwrap().format, DocFormat::Bson);
    }

    #[cfg(feature = "mongodb")]
    #[test]
    fn a_document_renders_in_the_shell_and_as_extended_json() {
        let raw = bson::rawdoc! { "b": 5_i64, "a": [1] };
        let document = DocumentRef {
            bytes: raw.as_bytes(),
            format: DocFormat::Bson,
        };
        assert_eq!(document.to_shell(false), "{ b: Long('5'), a: [ 1 ] }");
        assert_eq!(
            document.to_shell(true),
            "{\n  b: Long('5'),\n  a: [\n    1\n  ]\n}"
        );
        assert_eq!(document.to_extended_json(false), r#"{"b":5,"a":[1]}"#);
        assert_eq!(
            document.to_extended_json(true),
            r#"{"b":{"$numberLong":"5"},"a":[{"$numberInt":"1"}]}"#
        );
    }

    #[cfg(feature = "mongodb")]
    #[test]
    fn a_document_nested_too_deep_renders_its_refusal_in_place() {
        // {a: {a: ... {}}}, a thousand documents deep, built from the inside out.
        let mut bytes = vec![5u8, 0, 0, 0, 0];
        for _ in 1..1000 {
            let len = i32::try_from(bytes.len() + 8).unwrap();
            bytes = [&len.to_le_bytes()[..], &[3, b'a', 0], &bytes, &[0]].concat();
        }
        let document = DocumentRef {
            bytes: &bytes,
            format: DocFormat::Bson,
        };
        for text in [document.to_shell(true), document.to_extended_json(false)] {
            assert!(text.starts_with("/* undecodable document: "), "{text}");
            assert!(text.contains("nested deeper than 200 levels"), "{text}");
        }
    }

    #[cfg(feature = "mongodb")]
    #[test]
    fn an_undecodable_document_says_so_and_why() {
        // Too short to be a document, then a sound frame around an element type BSON lacks.
        for bytes in [&b"one"[..], &[8, 0, 0, 0, 0x42, b'a', 0, 0]] {
            let document = DocumentRef {
                bytes,
                format: DocFormat::Bson,
            };
            for text in [document.to_shell(false), document.to_extended_json(true)] {
                let reason = text
                    .strip_prefix("/* undecodable document: ")
                    .and_then(|t| t.strip_suffix(" */"))
                    .unwrap_or_else(|| panic!("{text}"));
                assert!(!reason.is_empty());
            }
        }
    }
}
