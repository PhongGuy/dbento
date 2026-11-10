//! SQL formatting: a lossless lexer and a layout pass over its tokens.

mod lex;
#[cfg(test)]
mod props;

use crate::Engine;
use crate::ranges::{MssqlBatch, mssql_ranges};
use crate::tsql::{TsqlKind, tsql_tokens};
use lex::{Kind, Tok};
use std::ops::Range;
use unicode_width::UnicodeWidthStr;

/// How keywords are cased.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeywordCase {
    /// Keywords in upper case, `SELECT`.
    #[default]
    Upper,
    /// Keywords in lower case, `select`.
    Lower,
    /// Keywords as written.
    Preserve,
}

/// Options for [`format_sql`]; `Default` uppercases keywords.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FormatOptions {
    /// How keywords are cased.
    pub keyword_case: KeywordCase,
}

impl FormatOptions {
    /// These options with `case` for keywords.
    pub fn with_keyword_case(mut self, case: KeywordCase) -> Self {
        self.keyword_case = case;
        self
    }
}

// Words cased as keywords when bare. KEY, FIRST and LAST count only after the word that
// makes them one, and LEFT, RIGHT, REPLACE, IF and ROW before `(` are functions.
const KEYWORDS: &str = "after all alter and any as asc before begin between by case commit conflict \
    constraint create cross current default delete desc distinct do drop each else end escape \
    except execute exists explain false fetch filter following for foreign from full function \
    group having if ilike in index inner insert intersect interval into is join language \
    lateral left like limit locked natural next not nothing nowait null nulls offset on only \
    or order outer over partition preceding primary recursive references replace returning \
    returns right rollback row rows select set skip some table then trigger true unbounded \
    union unique update using values view when where window with within";

// Microsoft's reserved keyword table for T-SQL; WITHIN is reserved only as WITHIN GROUP. A
// case-sensitive database tells `Rows` from `ROWS`, so no other word is recased on SQL Server.
const TSQL_RESERVED: &str = "add all alter and any as asc authorization backup begin between break browse bulk by \
    cascade case check checkpoint close clustered coalesce collate column commit compute \
    constraint contains containstable continue convert create cross current current_date \
    current_time current_timestamp current_user cursor database dbcc deallocate declare \
    default delete deny desc disk distinct distributed double drop dump else end errlvl \
    escape except exec execute exists exit external fetch file fillfactor for foreign \
    freetext freetexttable from full function goto grant group having holdlock identity \
    identity_insert identitycol if in index inner insert intersect into is join key kill \
    left like lineno load merge national nocheck nonclustered not null nullif of off offsets \
    on open opendatasource openquery openrowset openxml option or order outer over percent \
    pivot plan precision primary print proc procedure public raiserror read readtext \
    reconfigure references replication restore restrict return revert revoke right rollback \
    rowcount rowguidcol rule save schema securityaudit select semantickeyphrasetable \
    semanticsimilaritydetailstable semanticsimilaritytable session_user set setuser shutdown \
    some statistics system_user table tablesample textsize then to top tran transaction \
    trigger truncate try_convert tsequal union unique unpivot update updatetext use user \
    values varying view waitfor when where while with writetext";

fn tsql_reserved(word: &str) -> bool {
    TSQL_RESERVED
        .split_whitespace()
        .any(|k| k.eq_ignore_ascii_case(word))
}

/// Byte offsets where each `--` line comment (and on MySQL each `#` one) of `sql` starts,
/// outside strings, quoted identifiers, dollar quotes and block comments; None when `sql`
/// does not lex.
pub fn line_comment_starts(engine: Engine, sql: &str) -> Option<Vec<usize>> {
    let toks = lex::lex(engine, sql)?;
    Some(
        toks.iter()
            .filter(|t| t.kind == Kind::LineComment)
            .map(|t| t.range.start)
            .collect(),
    )
}

/// True when either end of `range` falls inside a token of `sql` (a word, number, string,
/// quoted identifier, dollar quote or comment), or `sql` up to `range` does not lex. An end
/// in whitespace or between two tokens is no split, and text after `range` is not read.
pub fn splits_token(engine: Engine, sql: &str, range: std::ops::Range<usize>) -> bool {
    let Some(toks) = lex::lex_to(engine, sql, range.end.max(range.start)) else {
        return true;
    };
    let inside = |at: usize| {
        toks.iter()
            .any(|t| t.kind != Kind::Space && t.range.start < at && at < t.range.end)
    };
    inside(range.start) || inside(range.end)
}

/// `sql` in Dbento's house style: one clause per line, list items and `AND`/`OR` aligned
/// under the clause's first item, keywords recased per `options` and function names as
/// written. Comments, literals and quoted identifiers are untouched, no token boundary
/// moves, and input that does not lex comes back unchanged. On SQL Server each GO batch is
/// laid out alone, GO lines and the text between batches are kept, and text whose layout would
/// change its batches, their GO counts or their tokens comes back unchanged.
pub fn format_sql(engine: Engine, sql: &str, options: &FormatOptions) -> String {
    if engine != Engine::Mssql {
        return format_text(engine, sql, options);
    }
    if lex::lex(engine, sql).is_none() {
        return sql.to_owned();
    }
    let (mut out, mut copied) = (String::new(), 0);
    for batch in mssql_ranges(sql) {
        out.push_str(&sql[copied..batch.range.start]);
        out.push_str(&format_text(engine, &sql[batch.range.clone()], options));
        copied = batch.range.end;
    }
    out.push_str(&sql[copied..]);
    if same_batches(sql, &out) {
        out
    } else {
        sql.to_owned()
    }
}

/// Whether two scripts run the same: the same batches with the same GO counts, and the same
/// tokens in each, where only a reserved word's case may differ. A break can leave a word
/// alone on its line, and a bare `go` there is a separator, so this holds for any word a
/// layout moves.
fn same_batches(before: &str, after: &str) -> bool {
    let (old, new) = (mssql_ranges(before), mssql_ranges(after));
    old.len() == new.len()
        && old.iter().zip(&new).all(|(a, b)| {
            let (a_code, b_code) = (batch_code(before, a), batch_code(after, b));
            a.repeat == b.repeat
                && a_code.len() == b_code.len()
                && a_code.iter().zip(&b_code).all(|(x, y)| {
                    let word = x.0 == TsqlKind::Word && y.0 == x.0 && tsql_reserved(x.1);
                    x == y || (word && x.1.eq_ignore_ascii_case(y.1))
                })
        })
}

/// The kind and text of each token of `batch` past whitespace, except the whitespace after a
/// `\`: a newline there continues a binary constant, so it is code the layout must not touch.
fn batch_code<'a>(sql: &'a str, batch: &MssqlBatch) -> Vec<(TsqlKind, &'a str)> {
    let text = &sql[batch.range.clone()];
    let mut code: Vec<(TsqlKind, &str)> = Vec::new();
    for t in tsql_tokens(text) {
        let continued = code.last().is_some_and(|last| last.1 == "\\");
        if t.kind != TsqlKind::Space || continued {
            code.push((t.kind, &text[t.range]));
        }
    }
    code
}

/// One piece of text in the house style, with no notion of a batch.
fn format_text(engine: Engine, sql: &str, options: &FormatOptions) -> String {
    let Some(toks) = lex::lex(engine, sql) else {
        return sql.to_owned();
    };
    let lead = toks.iter().take_while(|t| t.kind == Kind::Space).count();
    let body = &toks[lead..];
    let trail = body
        .iter()
        .rev()
        .take_while(|t| t.kind == Kind::Space)
        .count();
    let body = &body[..body.len() - trail];
    let (Some(first), Some(last)) = (body.first(), body.last()) else {
        return sql.to_owned();
    };
    let mut l = Layout {
        engine,
        sql,
        toks: body,
        case: options.keyword_case,
        out: String::new(),
        frames: Vec::new(),
        brk: None,
        prev: None,
    };
    // Two layout statements are parted by one blank line; next to any other statement
    // the source gap is kept, so a script's own spacing survives around DDL. A layout
    // statement that spans lines still starts its own line, or its list would align
    // far right.
    let mut prev: Option<(usize, bool)> = None;
    for (r, layout) in l.statements() {
        let mut same_line = false;
        if let Some((end, prev_layout)) = prev {
            if prev_layout && layout {
                l.out.push_str("\n\n");
            } else {
                let gap = &sql[body[end - 1].range.end..body[r.start].range.start];
                // A comment leading the statement trails the previous one and stays put.
                let code_first =
                    !matches!(body[r.start].kind, Kind::LineComment | Kind::BlockComment);
                same_line = code_first && !gap.contains('\n');
                l.out.push_str(gap);
            }
        }
        prev = Some((r.end, layout));
        if layout {
            let mark = l.out.len();
            l.statement(r.clone());
            if same_line && l.out[mark..].contains('\n') {
                l.out.truncate(mark);
                l.out.truncate(l.out.trim_end_matches([' ', '\t']).len());
                l.out.push('\n');
                l.statement(r);
            }
        } else {
            for i in r {
                let text = l.cased(i);
                l.out.push_str(&text);
            }
        }
    }
    format!(
        "{}{}{}",
        &sql[..first.range.start],
        l.out,
        &sql[last.range.end..]
    )
}

/// What the current clause does with its depth-0 commas and `AND`/`OR`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clause {
    Plain,
    List,
    Cond,
    Join,
}

/// A query level: the top of a statement or a paren that opens with a query.
#[derive(Debug, Clone)]
struct Query {
    base: usize,
    list_col: Option<usize>,
    clause: Clause,
    between: bool,
    /// Open `CASE`s; their `AND`/`OR` stay inline.
    cases: usize,
    first: bool,
    await_col: bool,
    /// Where the first item goes once the clause keyword is out; a line comment before
    /// that item breaks to it.
    item_col: usize,
    words_left: usize,
}

#[derive(Debug, Clone)]
enum Frame {
    Query(Query),
    Inline,
}

impl Frame {
    fn query(base: usize) -> Self {
        Frame::Query(Query {
            base,
            list_col: None,
            clause: Clause::Plain,
            between: false,
            cases: 0,
            first: true,
            await_col: false,
            item_col: 0,
            words_left: 0,
        })
    }
}

struct Layout<'a> {
    engine: Engine,
    sql: &'a str,
    toks: &'a [Tok],
    case: KeywordCase,
    out: String,
    frames: Vec<Frame>,
    /// A line break owed before the next token, to this column.
    brk: Option<usize>,
    prev: Option<usize>,
}

impl<'a> Layout<'a> {
    fn text(&self, i: usize) -> &'a str {
        &self.sql[self.toks[i].range.clone()]
    }

    /// The display column of the output's end, so wide CJK text keeps alignment.
    fn col(&self) -> usize {
        self.out
            .rsplit('\n')
            .next()
            .map_or(0, UnicodeWidthStr::width)
    }

    fn query(&mut self) -> Option<&mut Query> {
        match self.frames.last_mut() {
            Some(Frame::Query(q)) => Some(q),
            _ => None,
        }
    }

    /// Where a line continues after a line comment: the innermost query's item column.
    fn cont(&self) -> usize {
        self.frames
            .iter()
            .rev()
            .find_map(|f| match f {
                Frame::Query(q) => Some(
                    q.list_col
                        .or(q.await_col.then_some(q.item_col))
                        .unwrap_or(q.base),
                ),
                Frame::Inline => None,
            })
            .unwrap_or(0)
    }

    /// Token ranges of the statements, each with whether it gets the clause layout. A
    /// statement ends after a depth-0 `;`, except inside a non-layout statement's
    /// `BEGIN ... END` or `CASE ... END`, so a trigger body stays whole. Ranges are
    /// trimmed of outer whitespace; empty ones are dropped.
    fn statements(&self) -> Vec<(Range<usize>, bool)> {
        let mut out = Vec::new();
        let (mut start, mut depth, mut block) = (0, 0i32, 0usize);
        let mut first: Option<String> = None;
        for i in 0..self.toks.len() {
            let kind = self.toks[i].kind;
            if matches!(kind, Kind::Space | Kind::LineComment | Kind::BlockComment) {
                continue;
            }
            let word =
                (kind == Kind::Word && self.bare(i)).then(|| self.text(i).to_ascii_lowercase());
            // The statement's first code word picks its kind and is never a block opener.
            let opens = first.is_none();
            let layout =
                is_layout_lead(first.get_or_insert_with(|| word.clone().unwrap_or_default()));
            match (self.text(i), word.as_deref().filter(|_| !opens)) {
                ("(", _) => depth += 1,
                (")", _) => depth -= 1,
                (";", _) if depth <= 0 && block == 0 => {
                    out.push((start..i + 1, layout));
                    (start, first) = (i + 1, None);
                }
                (_, Some("case")) if depth <= 0 && !layout => block += 1,
                (_, Some("begin")) if depth <= 0 && !layout && self.opens_body(i) => block += 1,
                (_, Some("end")) if depth <= 0 && !layout => block = block.saturating_sub(1),
                _ => {}
            }
        }
        let lead = first.as_deref().unwrap_or_default();
        out.push((start..self.toks.len(), is_layout_lead(lead)));
        let space = |i: usize| self.toks[i].kind == Kind::Space;
        out.into_iter()
            .map(|(mut r, layout)| {
                while r.start < r.end && space(r.start) {
                    r.start += 1;
                }
                while r.end > r.start && space(r.end - 1) {
                    r.end -= 1;
                }
                (r, layout)
            })
            .filter(|(r, _)| !r.is_empty())
            .collect()
    }

    /// Whether the `BEGIN` at `i` opens a body (`BEGIN ATOMIC` or a trigger's statements)
    /// rather than naming something.
    fn opens_body(&self, i: usize) -> bool {
        self.next_code(i + 1).is_some_and(|n| {
            self.bare(n)
                && [
                    "atomic", "select", "insert", "update", "delete", "with", "values", "replace",
                ]
                .iter()
                .any(|k| self.text(n).eq_ignore_ascii_case(k))
        })
    }

    fn statement(&mut self, r: Range<usize>) {
        self.frames = vec![Frame::query(0)];
        (self.brk, self.prev) = (None, None);
        let (mut space, mut nl) = (false, false);
        let mut gap = 0;
        let mut i = r.start;
        while i < r.end {
            if self.toks[i].kind == Kind::Space {
                if !space {
                    gap = self.toks[i].range.start;
                }
                space = true;
                nl |= self.text(i).contains('\n');
                i += 1;
                continue;
            }
            let gap = if space {
                &self.sql[gap..self.toks[i].range.start]
            } else {
                ""
            };
            self.token(i, gap, nl);
            // A continued string is one literal to the server, comments and all, so
            // everything after its first piece is copied verbatim.
            let last = self.continuation_end(i, r.end);
            if last > i {
                let tail = self.toks[i].range.end..self.toks[last].range.end;
                self.out.push_str(&self.sql[tail]);
                self.prev = Some(last);
                i = last;
            }
            (space, nl) = (false, false);
            i += 1;
        }
    }

    /// The last piece of the string continuation starting at token `i`, or `i` itself.
    /// Postgres joins string literals separated by whitespace and `--` comments that hold
    /// a newline; a block comment ends the continuation.
    fn continuation_end(&self, i: usize, end: usize) -> usize {
        let (mut last, mut nl) = (i, false);
        if self.toks[i].kind != Kind::String {
            return i;
        }
        for k in i + 1..end {
            match self.toks[k].kind {
                Kind::Space => nl |= self.text(k).contains(['\n', '\r']),
                Kind::LineComment => {}
                Kind::String if nl => (last, nl) = (k, false),
                Kind::BlockComment
                | Kind::Word
                | Kind::QuotedIdent
                | Kind::String
                | Kind::Number
                | Kind::Param
                | Kind::Punct
                | Kind::Op => break,
            }
        }
        last
    }

    /// Emits token `i`; `gap` is the source whitespace before it and `nl` whether it
    /// holds a newline.
    fn token(&mut self, i: usize, gap: &str, nl: bool) {
        let space = !gap.is_empty();
        let kind = self.toks[i].kind;
        let comment = matches!(kind, Kind::LineComment | Kind::BlockComment);
        let text = self.text(i);
        if text == ")" && self.frames.len() > 1 {
            self.frames.pop();
        }
        let prev = self.prev.map(|p| self.text(p));
        let after_sep = matches!(prev, Some("," | ";"));
        // Tokens that touch in the source stay touching; only a comma or semicolon may
        // gain a break or a space after it.
        let touching = prev.is_some() && !space && !after_sep;
        let mut brk = self.brk.take();
        if kind == Kind::LineComment && !nl {
            // A comment on the comma's own line stays there; the break follows it.
            self.brk = brk.take();
        } else if kind == Kind::LineComment && prev.is_some() && brk.is_none() {
            // A comment on its own line keeps it, indented where the next code goes.
            brk = Some(self.peek_break(i));
        }
        if !comment && let Some(b) = self.decide(i) {
            brk = Some(b);
        }
        // A `;` pushed off a line comment's line closes the statement at its margin.
        if text == ";" && brk.is_some() {
            brk = Some(0);
        }
        if let Some(c) = brk.filter(|_| !touching) {
            self.out.push('\n');
            self.out.extend(std::iter::repeat_n(' ', c));
        } else if prev.is_some()
            && prev != Some("(")
            && !matches!(text, "," | ";" | ")")
            && (space || after_sep)
        {
            self.out.push(' ');
        }
        let col = self.col();
        let mut opened = false;
        if !comment && let Some(q) = self.query() {
            if q.await_col && q.words_left == 0 {
                (q.list_col, q.await_col) = (Some(col), false);
            }
            q.first = false;
            if q.words_left > 0 {
                q.words_left -= 1;
                q.await_col = q.words_left == 0 && matches!(q.clause, Clause::List | Clause::Cond);
                opened = q.await_col;
            }
        }
        let text = self.cased(i);
        self.out.push_str(&text);
        let end = self.col();
        if opened && let Some(q) = self.query() {
            q.item_col = end + 1;
        }
        self.prev = Some(i);
        self.after(i, kind);
    }

    /// Layout that follows a token: owed breaks after comments and list commas, and a
    /// new frame after `(`.
    fn after(&mut self, i: usize, kind: Kind) {
        let root_first =
            self.frames.len() == 1 && matches!(&self.frames[0], Frame::Query(q) if q.first);
        match (kind, self.text(i)) {
            (Kind::LineComment, _) => self.brk = Some(self.brk.unwrap_or(self.cont())),
            (Kind::BlockComment, _) if root_first => self.brk = Some(0),
            (_, "(") => {
                let next = self.next_code(i + 1);
                let opens_query = next.is_some_and(|n| {
                    self.bare(n)
                        && ["select", "with", "values"]
                            .iter()
                            .any(|k| self.text(n).eq_ignore_ascii_case(k))
                });
                let frame = if opens_query {
                    Frame::query(self.col())
                } else {
                    Frame::Inline
                };
                self.frames.push(frame);
            }
            // A leading comma under a line comment keeps its item on its line.
            (_, ",") if self.prev_kind(i) == Some(Kind::LineComment) => {}
            (_, ",") => {
                if let Some(q) = self.query()
                    && q.clause == Clause::List
                    && q.words_left == 0
                {
                    self.brk = q.list_col;
                }
            }
            _ => {}
        }
    }

    /// The break a code token at depth 0 of a query asks for: before a clause keyword,
    /// or before `AND`/`OR` in a condition. Also tracks the clause it opens.
    fn decide(&mut self, i: usize) -> Option<usize> {
        let mut words = self.words_from(i);
        // The FROM of `IS [NOT] DISTINCT FROM` and the VALUES of `DEFAULT VALUES` belong to
        // the phrase before them, not to a new clause.
        let joined = match words.first().map(String::as_str) {
            Some("from") => Some("distinct"),
            Some("values") => Some("default"),
            _ => None,
        };
        if joined.is_some() && self.prev_word(i).as_deref() == joined {
            words.clear();
        }
        let q = self.query()?;
        if q.words_left > 0 || words.is_empty() {
            return None;
        }
        let w0 = words[0].as_str();
        // A join's ON opens a condition inline; every other clause keyword breaks.
        let (hit, breaks) = match clause_at(&words, q.first) {
            Some(hit) => (Some(hit), !q.first),
            None => (
                (w0 == "on" && q.clause == Clause::Join).then_some((1, Clause::Cond)),
                false,
            ),
        };
        if let Some((n, clause)) = hit {
            (q.clause, q.words_left, q.list_col) = (clause, n, None);
            (q.await_col, q.between) = (false, false);
            return breaks.then_some(q.base);
        }
        match w0 {
            "between" => q.between = true,
            "and" if q.between => q.between = false,
            "case" => q.cases += 1,
            "end" => q.cases = q.cases.saturating_sub(1),
            "and" | "or" if q.clause == Clause::Cond && q.cases == 0 => return q.list_col,
            _ => {}
        }
        None
    }

    /// The column the next code token after `i` will start a line at, without moving the
    /// layout state.
    fn peek_break(&mut self, i: usize) -> usize {
        let Some(n) = self.next_code(i + 1) else {
            return self.cont();
        };
        let saved = self.frames.clone();
        let b = self.decide(n);
        self.frames = saved;
        b.unwrap_or_else(|| self.cont())
    }

    fn next_code(&self, from: usize) -> Option<usize> {
        (from..self.toks.len()).find(|&j| {
            !matches!(
                self.toks[j].kind,
                Kind::Space | Kind::LineComment | Kind::BlockComment
            )
        })
    }

    /// Up to five lowercased bare words starting at `i`, separated only by whitespace
    /// and comments.
    fn words_from(&self, i: usize) -> Vec<String> {
        let mut words = Vec::new();
        let mut j = Some(i);
        while let Some(k) = j.filter(|&k| words.len() < 5 && self.bare(k)) {
            words.push(self.text(k).to_ascii_lowercase());
            j = self.next_code(k + 1);
        }
        words
    }

    /// The kind of the token before `i`, past whitespace.
    fn prev_kind(&self, i: usize) -> Option<Kind> {
        self.toks[..i]
            .iter()
            .rev()
            .map(|t| t.kind)
            .find(|&k| k != Kind::Space)
    }

    /// A word not joined to a `.`, so not part of a qualified name.
    fn bare(&self, i: usize) -> bool {
        let dot = |j: Option<usize>| {
            j.and_then(|j| self.toks.get(j))
                .is_some_and(|t| &self.sql[t.range.clone()] == ".")
        };
        self.toks[i].kind == Kind::Word && !dot(i.checked_sub(1)) && !dot(Some(i + 1))
    }

    fn cased(&self, i: usize) -> String {
        let text = self.text(i);
        if !self.is_keyword(i) {
            return text.to_owned();
        }
        match self.case {
            KeywordCase::Upper => text.to_ascii_uppercase(),
            KeywordCase::Lower => text.to_ascii_lowercase(),
            KeywordCase::Preserve => text.to_owned(),
        }
    }

    /// The lowercased word just before `i`, past whitespace and comments.
    fn prev_word(&self, i: usize) -> Option<String> {
        (0..i)
            .rev()
            .find(|&j| {
                !matches!(
                    self.toks[j].kind,
                    Kind::Space | Kind::LineComment | Kind::BlockComment
                )
            })
            .filter(|&j| self.toks[j].kind == Kind::Word)
            .map(|j| self.text(j).to_ascii_lowercase())
    }

    fn is_keyword(&self, i: usize) -> bool {
        if !self.bare(i) {
            return false;
        }
        let w = self.text(i).to_ascii_lowercase();
        if self.engine == Engine::Mssql && !tsql_reserved(&w) {
            return false;
        }
        let prev_word = self.prev_word(i);
        let paren_next = self
            .toks
            .get(i + 1)
            .is_some_and(|t| &self.sql[t.range.clone()] == "(");
        match w.as_str() {
            "key" => matches!(prev_word.as_deref(), Some("primary" | "foreign")),
            "first" | "last" => matches!(prev_word.as_deref(), Some("nulls" | "fetch")),
            "left" | "right" | "replace" | "if" | "row" if paren_next => false,
            _ => KEYWORDS.split_whitespace().any(|k| k == w),
        }
    }
}

/// Whether a statement whose first code word is `w` gets the clause layout.
fn is_layout_lead(w: &str) -> bool {
    ["select", "with", "insert", "update", "delete", "values"].contains(&w)
}

/// The clause keyword sequence that `w` starts with: its word count and its kind. Leading
/// clauses (`WITH`, `INSERT ... INTO`, `UPDATE`, `DELETE FROM`) count only as a frame's
/// first word.
fn clause_at(w: &[String], first: bool) -> Option<(usize, Clause)> {
    use Clause::*;
    let is = |i: usize, s: &str| w.get(i).is_some_and(|x| x == s);
    if first {
        let lead = match w[0].as_str() {
            "with" => Some((1 + usize::from(is(1, "recursive")), List)),
            "insert" if is(1, "into") => Some((2, Plain)),
            "insert" if is(1, "or") && is(3, "into") => Some((4, Plain)),
            "update" => Some((1, Plain)),
            "delete" if is(1, "from") => Some((2, Plain)),
            _ => None,
        };
        if lead.is_some() {
            return lead;
        }
    }
    Some(match w[0].as_str() {
        // `DISTINCT ON (...)` also spans its parens: the `(` and `)` count as two words.
        "select" if is(1, "distinct") && is(2, "on") => (5, List),
        "select" => (1 + usize::from(is(1, "distinct")), List),
        "from" | "values" | "set" | "returning" => (1, List),
        "where" | "having" => (1, Cond),
        "group" | "order" if is(1, "by") => (2, List),
        "window" | "limit" | "offset" | "intersect" | "except" => (1, Plain),
        "union" => (1 + usize::from(is(1, "all")), Plain),
        "on" if is(1, "conflict") => (2, Plain),
        _ => {
            // [NATURAL] [LEFT|RIGHT|FULL|INNER|CROSS] [OUTER] JOIN
            let mut n = usize::from(is(0, "natural"));
            n += usize::from(
                ["left", "right", "full", "inner", "cross"]
                    .iter()
                    .any(|s| is(n, s)),
            );
            n += usize::from(is(n, "outer"));
            return is(n, "join").then_some((n + 1, Join));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::same_batches;

    #[test]
    fn batches_compare_by_count_go_count_and_tokens_with_only_word_case_free() {
        assert!(same_batches(
            "select a\nGO 3\nselect 'x' -- c",
            "SELECT\n  a\nGO 3\nSELECT 'x' -- c"
        ));
        assert!(same_batches("select 'a\\\nb'", "SELECT 'a\\\nb'"));
        for (before, after) in [
            ("select a, go from t", "select a,\ngo\nfrom t"),
            ("select 1\nGO 2", "select 1\nGO 3"),
            ("select 1\nGO", "select 1 GO"),
            ("select a", "select b"),
            ("select a b", "select ab"),
            ("select 'x'", "select 'X'"),
            ("select [x]", "select [X]"),
            ("select 1 -- c", "select 1 -- C"),
            ("select @a", "select @A"),
            ("select a", "select A"),
            ("select 0xAB\\\nCD from t", "select 0xAB\\ CD from t"),
            ("select 0xAB\\ from t", "select 0xAB\\\nfrom t"),
        ] {
            assert!(!same_batches(before, after), "{before:?} {after:?}");
        }
    }
}
