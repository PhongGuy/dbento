//! The statement generator: one walk over a batch that emits either bound statements for
//! `apply` or literal SQL for the preview, so the two cannot drift.

use std::fmt::Write as _;

use super::{Change, EditBatch, EditValue, RowKey};
use crate::introspect::quote_ident;
use crate::workspace::Engine;

/// The engines row edits generate SQL for; `EditBatch::new` refuses every other engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dialect {
    Postgres,
    Sqlite,
}

impl Dialect {
    /// The dialect of `engine`, or None when row edits cannot target it.
    pub(crate) fn of(engine: Engine) -> Option<Dialect> {
        match engine {
            Engine::Postgres => Some(Dialect::Postgres),
            Engine::Sqlite => Some(Dialect::Sqlite),
            Engine::Mysql | Engine::Mssql => None,
            Engine::Duckdb => None,
            Engine::Mongodb => None,
            Engine::Surrealdb => None,
            Engine::Redis => None,
        }
    }

    /// The engine whose identifier quoting the dialect uses.
    pub(crate) fn engine(self) -> Engine {
        match self {
            Dialect::Postgres => Engine::Postgres,
            Dialect::Sqlite => Engine::Sqlite,
        }
    }
}

/// One bound parameter: Postgres receives every value as text, SQLite text or a blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Param {
    Text(String),
    Bytes(Vec<u8>),
}

/// One statement with its parameters in placeholder order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundStatement {
    pub(crate) sql: String,
    pub(crate) params: Vec<Param>,
    /// Whether the statement returns the row's identity columns, as `Gen::returning` reads them.
    pub(crate) returning: bool,
}

/// One bound statement per change, in batch order. An Update that writes an identity
/// column, or moves a Postgres row its ctid locates, also returns the identity, so the
/// caller learns the key the database stored; a target whose rules refuse it never does.
pub(crate) fn bound(batch: &EditBatch) -> Vec<BoundStatement> {
    let target = batch.target();
    let identity = target.identity();
    let located = batch.dialect() == Dialect::Postgres && !target.locators().is_empty();
    batch
        .changes()
        .iter()
        .map(|c| {
            let mut g = Gen::new(batch, true);
            let mut sql = g.change(c);
            let returning = target.returning()
                && matches!(c, Change::Update { set, .. }
                    if located || set.iter().any(|(col, _)| identity.contains(col)));
            if returning {
                sql.push_str(&g.returning());
            }
            BoundStatement {
                sql,
                params: g.params,
                returning,
            }
        })
        .collect()
}

/// The batch as literal SQL, one `;`-terminated statement per line, for preview and copy.
pub fn render_sql(batch: &EditBatch) -> String {
    batch
        .changes()
        .iter()
        .map(|c| Gen::new(batch, false).change(c) + ";")
        .collect::<Vec<_>>()
        .join("\n")
}

/// Bytes as Postgres `bytea` hex input: `\x` and lowercase hex.
pub(crate) fn pg_hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::from("\\x"), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

struct Gen<'a> {
    batch: &'a EditBatch,
    bind: bool,
    params: Vec<Param>,
}

impl<'a> Gen<'a> {
    fn new(batch: &'a EditBatch, bind: bool) -> Self {
        Gen {
            batch,
            bind,
            params: Vec::new(),
        }
    }

    fn dialect(&self) -> Dialect {
        self.batch.dialect()
    }

    fn ident(&self, name: &str) -> String {
        quote_ident(self.dialect().engine(), name)
    }

    fn relation(&self, only: bool) -> String {
        let r = self.batch.target().relation();
        let only = if only && self.batch.target().only() {
            "ONLY "
        } else {
            ""
        };
        format!("{only}{}.{}", self.ident(r.schema()), self.ident(r.name()))
    }

    /// SET or VALUES are rendered before the key so parameters number in that order.
    fn change(&mut self, change: &Change) -> String {
        match change {
            Change::Update { key, set } => {
                let rel = self.relation(true);
                let set = set
                    .iter()
                    .map(|(c, v)| format!("{} = {}", self.ident(c), self.value(c, v)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("UPDATE {rel} SET {set} WHERE {}", self.key(key))
            }
            Change::Insert { values } => {
                let rel = self.relation(false);
                let given: Vec<_> = values
                    .iter()
                    .filter(|(_, v)| *v != EditValue::Default)
                    .collect();
                if given.is_empty() {
                    return format!("INSERT INTO {rel} DEFAULT VALUES");
                }
                let cols = given
                    .iter()
                    .map(|(c, _)| self.ident(c))
                    .collect::<Vec<_>>()
                    .join(", ");
                let vals = given
                    .iter()
                    .map(|(c, v)| self.value(c, v))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("INSERT INTO {rel} ({cols}) VALUES ({vals})")
            }
            Change::Delete { key } => {
                let rel = self.relation(true);
                format!("DELETE FROM {rel} WHERE {}", self.key(key))
            }
        }
    }

    /// ` RETURNING` the identity in the form a browse reads it: SQLite's own values, and on
    /// Postgres each type's output text, which `format('%s')` writes and `::text` may not.
    fn returning(&self) -> String {
        let cols = self
            .batch
            .target()
            .identity()
            .iter()
            .map(|c| {
                let k = self.ident(c);
                match self.dialect() {
                    Dialect::Sqlite => k,
                    Dialect::Postgres => format!(
                        "CASE WHEN {k} IS NULL THEN NULL ELSE pg_catalog.format('%s', {k}) END"
                    ),
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(" RETURNING {cols}")
    }

    fn key(&mut self, key: &RowKey) -> String {
        key.values()
            .iter()
            .map(|(c, v)| {
                let k = self.ident(c);
                match v {
                    EditValue::Null => format!("{k} IS NULL"),
                    EditValue::Text(_) if self.untyped(c) => {
                        // An untyped SQLite column compares a stored number with text
                        // without converting it, so a text key also matches it numerically.
                        let v = self.value(c, v);
                        format!(
                            "({k} = {v} OR (typeof({k}) IN ('integer','real') AND CAST({k} AS NUMERIC) = {v}))"
                        )
                    }
                    _ => format!("{k} = {}", self.value(c, v)),
                }
            })
            .collect::<Vec<_>>()
            .join(" AND ")
    }

    /// Whether `column` is a SQLite column without type affinity: no declared type, or a
    /// BLOB-like one that no INT or text rule claims first.
    fn untyped(&self, column: &str) -> bool {
        if self.dialect() != Dialect::Sqlite {
            return false;
        }
        let Some(col) = self.batch.target().column(column) else {
            return false;
        };
        let t = col.meta().type_name().to_ascii_uppercase();
        t.trim().is_empty()
            || (t.contains("BLOB")
                && !["INT", "CHAR", "CLOB", "TEXT"]
                    .iter()
                    .any(|s| t.contains(s)))
    }

    /// The Postgres type a value of `column` is cast to; locators have fixed system types.
    fn cast_type(&self, column: &str) -> &'a str {
        let target = self.batch.target();
        match (target.column(column), column) {
            (Some(c), _) => c.cast_type(),
            (None, "ctid") => "pg_catalog.tid",
            (None, "xmin") => "pg_catalog.xid",
            (None, _) => "pg_catalog.text",
        }
    }

    fn value(&mut self, column: &str, value: &EditValue) -> String {
        let dialect = self.dialect();
        match value {
            EditValue::Null => "NULL".to_owned(),
            EditValue::Default => match dialect {
                Dialect::Sqlite => match self
                    .batch
                    .target()
                    .column(column)
                    .and_then(|c| c.meta().default())
                {
                    Some(d) => format!("({d})"),
                    None => "NULL".to_owned(),
                },
                Dialect::Postgres => "DEFAULT".to_owned(),
            },
            EditValue::Text(s) => self.param(column, Param::Text(s.clone())),
            EditValue::Bytes(b) => {
                let param = match dialect {
                    Dialect::Sqlite => Param::Bytes(b.clone()),
                    Dialect::Postgres => Param::Text(pg_hex(b)),
                };
                self.param(column, param)
            }
        }
    }

    /// `param` as a placeholder when binding, else as a literal.
    fn param(&mut self, column: &str, param: Param) -> String {
        if self.bind {
            self.placeholder(column, param)
        } else {
            self.literal(column, &param)
        }
    }

    fn placeholder(&mut self, column: &str, param: Param) -> String {
        self.params.push(param);
        let n = self.params.len();
        match self.dialect() {
            Dialect::Sqlite => format!("?{n}"),
            Dialect::Postgres => format!("${n}::text::{}", self.cast_type(column)),
        }
    }

    fn literal(&self, column: &str, param: &Param) -> String {
        if let Param::Text(s) = param
            && let Some(bare) = self.bare(column, s)
        {
            return bare;
        }
        match (param, self.dialect()) {
            (Param::Bytes(b), _) => {
                let hex = pg_hex(b);
                format!("X'{}'", &hex[2..])
            }
            (Param::Text(s), Dialect::Sqlite) => format!("'{}'", s.replace('\'', "''")),
            (Param::Text(s), Dialect::Postgres) => {
                let cast = self.cast_type(column);
                let quoted = s.replace('\'', "''");
                if s.contains('\\') {
                    format!("E'{}'::{cast}", quoted.replace('\\', "\\\\"))
                } else {
                    format!("'{quoted}'::{cast}")
                }
            }
        }
    }

    /// `text` as a bare literal when the column reads it exactly as it reads the bound
    /// text: Postgres in-range integers, numerics and booleans, and SQLite numbers under
    /// INTEGER, REAL or NUMERIC affinity. Anything else keeps its quoted form.
    fn bare(&self, column: &str, text: &str) -> Option<String> {
        if self.dialect() == Dialect::Sqlite {
            // Affinity rules in SQLite's order; a STRICT `ANY` column stores text as text.
            let t = self.batch.target().column(column)?.meta().type_name();
            let t = t.trim().to_ascii_uppercase();
            let numeric = if t.contains("INT") {
                is_integer(text)
            } else if ["CHAR", "CLOB", "TEXT", "BLOB"]
                .iter()
                .any(|s| t.contains(s))
                || t.is_empty()
                || t == "ANY"
            {
                false
            } else {
                is_decimal(text)
            };
            return numeric.then(|| text.to_owned());
        }
        // Postgres reads `-N` as minus the literal N, so N itself must fit the column's type:
        // a wider N (an out-of-range value, or the type's minimum) compares as a wider type.
        let magnitude = text.strip_prefix('-').unwrap_or(text);
        let fits = match self.cast_type(column).replace('"', "").as_str() {
            "pg_catalog.int2" => is_integer(text) && magnitude.parse::<i16>().is_ok(),
            "pg_catalog.int4" => is_integer(text) && magnitude.parse::<i32>().is_ok(),
            "pg_catalog.int8" => is_integer(text) && magnitude.parse::<i64>().is_ok(),
            "pg_catalog.numeric" => is_decimal(text),
            "pg_catalog.bool" => {
                let word = text.to_ascii_lowercase();
                return match word.as_str() {
                    "t" | "true" | "y" | "yes" | "on" | "1" => Some("true".to_owned()),
                    "f" | "false" | "n" | "no" | "off" | "0" => Some("false".to_owned()),
                    _ => None,
                };
            }
            _ => false,
        };
        fits.then(|| text.to_owned())
    }
}

/// `^-?[0-9]+$`
fn is_integer(text: &str) -> bool {
    digits(text.strip_prefix('-').unwrap_or(text))
}

/// `^-?[0-9]+(\.[0-9]+)?$`
fn is_decimal(text: &str) -> bool {
    let unsigned = text.strip_prefix('-').unwrap_or(text);
    match unsigned.split_once('.') {
        Some((whole, frac)) => digits(whole) && digits(frac),
        None => digits(unsigned),
    }
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}
