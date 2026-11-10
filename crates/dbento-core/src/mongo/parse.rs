//! Mongosh statements into server commands: `use <name>`, or `db`, its accessors and one
//! method call from [`METHODS`], built into the one command document the method sends.

use std::ops::Range;

use bson::{Bson, Document, doc};

use super::lex::{Kind, Tok, string_value};
use super::split::split;
use super::value::{Cursor, NOT_JS, ParseError, separator, value};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MongoStatement {
    pub(crate) db: Option<String>,
    pub(crate) op: MongoOp,
    pub(crate) range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MongoOp {
    Use(String),
    /// A shell method, built into the one server command it sends.
    Method {
        method: MongoMethod,
        command: Document,
    },
    /// `db.runCommand(doc)` as written.
    RunCommand(Document),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MongoMethod {
    Find,
    FindOne,
    Aggregate,
    CountDocuments,
    EstimatedDocumentCount,
    Distinct,
    GetIndexes,
    GetCollectionNames,
    Stats,
    InsertOne,
    InsertMany,
    UpdateOne,
    UpdateMany,
    ReplaceOne,
    DeleteOne,
    DeleteMany,
    FindOneAndUpdate,
    FindOneAndReplace,
    FindOneAndDelete,
    CreateIndex,
    DropIndex,
    Drop,
    CreateCollection,
    DropDatabase,
}

/// A statement that does not parse: its message, its offset from `range.start`, and the
/// statement's range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyntaxError {
    pub(crate) message: String,
    pub(crate) offset: usize,
    pub(crate) range: Range<usize>,
}

/// One result per `split` piece, in order; a piece's lex error is its result.
pub(crate) fn parse_script(src: &str) -> Vec<Result<MongoStatement, SyntaxError>> {
    let (toks, pieces) = split(src);
    pieces
        .into_iter()
        .map(|piece| {
            let range = piece.range.clone();
            let failed = |e: ParseError| SyntaxError {
                message: e.message,
                // Every error points into its own piece; saturating keeps a slip from panicking.
                offset: e.at.saturating_sub(range.start),
                range: range.clone(),
            };
            if let Some(e) = piece.error {
                return Err(failed(ParseError::new(e.message.to_owned(), e.at)));
            }
            let (db, op) =
                statement(src, &mut Cursor::new(src, &toks[piece.toks])).map_err(failed)?;
            Ok(MongoStatement { db, op, range })
        })
        .collect()
}

#[derive(Clone, Copy)]
enum ArgKind {
    Doc,
    Array,
    Str,
    Num,
    StrOrDoc,
    DocOrArray,
    Any,
}

impl ArgKind {
    fn accepts(self, v: &Bson) -> bool {
        match self {
            Self::Doc => matches!(v, Bson::Document(_)),
            Self::Array => matches!(v, Bson::Array(_)),
            Self::Str => matches!(v, Bson::String(_)),
            Self::Num => matches!(v, Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_)),
            Self::StrOrDoc => matches!(v, Bson::String(_) | Bson::Document(_)),
            Self::DocOrArray => matches!(v, Bson::Document(_) | Bson::Array(_)),
            Self::Any => true,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Doc => "a document",
            Self::Array => "an array",
            Self::Str => "a string",
            Self::Num => "a number",
            Self::StrOrDoc => "a string or a document",
            Self::DocOrArray => "a document or an array",
            Self::Any => "a value",
        }
    }
}

#[derive(Clone, Copy)]
enum Param {
    Req(ArgKind),
    Opt(ArgKind),
}
use ArgKind::{Array, Doc, DocOrArray, Num, Str, StrOrDoc};
use Param::{Opt, Req};

#[derive(PartialEq)]
enum Scope {
    Collection,
    Database,
}

/// One shell method: `method` is `None` for `runCommand`, whose document is sent as written.
struct Row {
    name: &'static str,
    scope: Scope,
    method: Option<MongoMethod>,
    params: &'static [Param],
    build: Build,
}

/// Builds the command from the collection name and checked arguments; an `Err` is the
/// index of the argument at fault and its message.
type Build = fn(&str, &[Bson]) -> Built;
type Built = Result<Document, (usize, String)>;
use MongoMethod as M;

const METHODS: &[Row] = &[
    coll("find", M::Find, &[Opt(Doc), Opt(Doc)], find),
    coll("findOne", M::FindOne, &[Opt(Doc), Opt(Doc)], find_one),
    coll(
        "aggregate",
        M::Aggregate,
        &[Req(Array), Opt(Doc)],
        aggregate,
    ),
    coll(
        "countDocuments",
        M::CountDocuments,
        &[Opt(Doc), Opt(Doc)],
        count_documents,
    ),
    coll(
        "estimatedDocumentCount",
        M::EstimatedDocumentCount,
        &[Opt(Doc)],
        estimated,
    ),
    coll("distinct", M::Distinct, &[Req(Str), Opt(Doc)], distinct),
    coll("getIndexes", M::GetIndexes, &[], |coll, _| {
        Ok(doc! { "listIndexes": coll, "cursor": {} })
    }),
    coll("insertOne", M::InsertOne, &[Req(Doc)], |coll, args| {
        Ok(doc! { "insert": coll, "documents": [args[0].clone()] })
    }),
    coll(
        "insertMany",
        M::InsertMany,
        &[Req(Array), Opt(Doc)],
        |coll, args| {
            let command = doc! { "insert": coll, "documents": args[0].clone() };
            Ok(on_top(command, as_doc(args.get(1))))
        },
    ),
    coll("updateOne", M::UpdateOne, UPDATE, |coll, args| {
        operators(&args[1], true).map(|()| update(coll, args, false))
    }),
    coll("updateMany", M::UpdateMany, UPDATE, |coll, args| {
        operators(&args[1], true).map(|()| update(coll, args, true))
    }),
    coll("replaceOne", M::ReplaceOne, REPLACE, |coll, args| {
        operators(&args[1], false).map(|()| update(coll, args, false))
    }),
    coll(
        "deleteOne",
        M::DeleteOne,
        &[Req(Doc), Opt(Doc)],
        |coll, args| Ok(delete(coll, args, 1)),
    ),
    coll(
        "deleteMany",
        M::DeleteMany,
        &[Opt(Doc), Opt(Doc)],
        |coll, args| Ok(delete(coll, args, 0)),
    ),
    coll(
        "findOneAndUpdate",
        M::FindOneAndUpdate,
        UPDATE,
        |coll, args| {
            let command = find_and_modify(coll, args, ("update", args[1].clone()), args.get(2))?;
            operators(&args[1], true).map(|()| command)
        },
    ),
    coll(
        "findOneAndReplace",
        M::FindOneAndReplace,
        REPLACE,
        |coll, args| {
            let command = find_and_modify(coll, args, ("update", args[1].clone()), args.get(2))?;
            operators(&args[1], false).map(|()| command)
        },
    ),
    coll(
        "findOneAndDelete",
        M::FindOneAndDelete,
        &[Req(Doc), Opt(Doc)],
        |coll, args| find_and_modify(coll, args, ("remove", true.into()), args.get(1)),
    ),
    coll(
        "createIndex",
        M::CreateIndex,
        &[Req(Doc), Opt(Doc)],
        create_index,
    ),
    coll("dropIndex", M::DropIndex, &[Req(StrOrDoc)], |coll, args| {
        Ok(doc! { "dropIndexes": coll, "index": args[0].clone() })
    }),
    coll("drop", M::Drop, &[], |coll, _| Ok(doc! { "drop": coll })),
    db(
        "getCollectionNames",
        Some(M::GetCollectionNames),
        &[],
        |_, _| Ok(doc! { "listCollections": 1, "nameOnly": true, "authorizedCollections": true }),
    ),
    db("stats", Some(M::Stats), &[], |_, _| {
        Ok(doc! { "dbStats": 1 })
    }),
    db(
        "createCollection",
        Some(M::CreateCollection),
        &[Req(Str), Opt(Doc)],
        |_, args| {
            Ok(on_top(
                doc! { "create": args[0].clone() },
                as_doc(args.get(1)),
            ))
        },
    ),
    db("dropDatabase", Some(M::DropDatabase), &[], |_, _| {
        Ok(doc! { "dropDatabase": 1 })
    }),
    db("runCommand", None, &[Req(Doc)], |_, args| {
        Ok(as_doc(args.first()))
    }),
];

const UPDATE: &[Param] = &[Req(Doc), Req(DocOrArray), Opt(Doc)];
const REPLACE: &[Param] = &[Req(Doc), Req(Doc), Opt(Doc)];

// The `find` cursor modifiers, each setting the command key of its own name.
const MODIFIERS: &[(&str, ArgKind)] = &[
    ("sort", Doc),
    ("projection", Doc),
    ("collation", Doc),
    ("limit", Num),
    ("skip", Num),
    ("batchSize", Num),
    ("hint", StrOrDoc),
    ("comment", ArgKind::Any),
];

const fn coll(name: &'static str, method: M, params: &'static [Param], build: Build) -> Row {
    let method = Some(method);
    let scope = Scope::Collection;
    Row {
        name,
        scope,
        method,
        params,
        build,
    }
}

const fn db(name: &'static str, method: Option<M>, params: &'static [Param], build: Build) -> Row {
    let scope = Scope::Database;
    Row {
        name,
        scope,
        method,
        params,
        build,
    }
}

fn as_doc(arg: Option<&Bson>) -> Document {
    match arg {
        Some(Bson::Document(d)) => d.clone(),
        _ => Document::new(),
    }
}

// Options go on top of the command in the order written; `insert` keeps a key's position.
fn on_top(mut command: Document, options: Document) -> Document {
    command.extend(options);
    command
}

fn find(coll: &str, args: &[Bson]) -> Built {
    let mut command = doc! { "find": coll, "filter": as_doc(args.first()) };
    if let Some(projection) = args.get(1) {
        command.insert("projection", projection.clone());
    }
    Ok(command)
}

fn find_one(coll: &str, args: &[Bson]) -> Built {
    Ok(on_top(
        find(coll, args)?,
        doc! { "limit": 1, "singleBatch": true },
    ))
}

// As the Node driver does, a `batchSize` option merges into the `cursor` option, if any.
fn aggregate(coll: &str, args: &[Bson]) -> Built {
    let mut options = as_doc(args.get(1));
    let batch_size = options.remove("batchSize");
    let mut command = doc! { "aggregate": coll, "pipeline": args[0].clone(), "cursor": {} };
    command = on_top(command, options);
    if let Some(n) = batch_size {
        match command.get_mut("cursor") {
            Some(Bson::Document(cursor)) => {
                cursor.insert("batchSize", n);
            }
            _ => {
                command.insert("cursor", doc! { "batchSize": n });
            }
        }
    }
    Ok(command)
}

// The driver's pipeline: `$match`, then the `skip` and `limit` options as stages, then `$group`.
fn count_documents(coll: &str, args: &[Bson]) -> Built {
    let mut options = as_doc(args.get(1));
    let mut pipeline = vec![Bson::from(doc! { "$match": as_doc(args.first()) })];
    for (option, stage) in [("skip", "$skip"), ("limit", "$limit")] {
        if let Some(n) = options.remove(option) {
            pipeline.push(doc! { stage: n }.into());
        }
    }
    pipeline.push(doc! { "$group": { "_id": 1, "n": { "$sum": 1 } } }.into());
    let command = doc! { "aggregate": coll, "pipeline": pipeline, "cursor": {} };
    Ok(on_top(command, options))
}

fn estimated(coll: &str, args: &[Bson]) -> Built {
    Ok(on_top(doc! { "count": coll }, as_doc(args.first())))
}

fn distinct(coll: &str, args: &[Bson]) -> Built {
    let mut command = doc! { "distinct": coll, "key": args[0].clone() };
    if let Some(query) = args.get(1) {
        command.insert("query", query.clone());
    }
    Ok(command)
}

// Moves the options named in `keys` into `statement`, both in the order written.
fn into_statement(options: Document, keys: &[&str], statement: &mut Document) -> Document {
    let (inner, outer): (Vec<_>, Vec<_>) = options
        .into_iter()
        .partition(|(k, _)| keys.contains(&k.as_str()));
    statement.extend(inner);
    outer.into_iter().collect()
}

// Mongosh's rule, so a missing `$set` cannot replace a whole document: an update starts
// with an operator (a pipeline passes), and a replacement holds none.
fn operators(u: &Bson, wanted: bool) -> Result<(), (usize, String)> {
    let Bson::Document(d) = u else { return Ok(()) };
    let fails = if wanted {
        !d.keys().next().is_some_and(|k| k.starts_with('$'))
    } else {
        d.keys().any(|k| k.starts_with('$'))
    };
    if !fails {
        return Ok(());
    }
    let message = if wanted {
        "Update document requires atomic operators"
    } else {
        "Replacement document must not contain atomic operators"
    };
    Err((1, message.into()))
}

// `updateOne`, `updateMany` and `replaceOne` all send one `update` statement.
fn update(coll: &str, args: &[Bson], multi: bool) -> Document {
    let mut statement = doc! { "q": args[0].clone(), "u": args[1].clone(), "multi": multi };
    let keys = ["upsert", "arrayFilters", "hint", "collation", "sort"];
    let options = into_statement(as_doc(args.get(2)), &keys, &mut statement);
    on_top(doc! { "update": coll, "updates": [statement] }, options)
}

fn delete(coll: &str, args: &[Bson], limit: i32) -> Document {
    let mut statement = doc! { "q": as_doc(args.first()), "limit": limit };
    let keys = ["hint", "collation"];
    let options = into_statement(as_doc(args.get(1)), &keys, &mut statement);
    on_top(doc! { "delete": coll, "deletes": [statement] }, options)
}

// The shell's option names become `findAndModify`'s: `returnDocument` and
// `returnNewDocument` set `new`, `projection` sets `fields`.
fn find_and_modify(
    coll: &str,
    args: &[Bson],
    (action, value): (&str, Bson),
    options: Option<&Bson>,
) -> Built {
    let mut command = doc! { "findAndModify": coll, "query": args[0].clone(), action: value };
    for (key, value) in as_doc(options) {
        let (key, value) = match key.as_str() {
            "returnDocument" => match value.as_str() {
                Some("after") => ("new", Bson::Boolean(true)),
                Some("before") => ("new", Bson::Boolean(false)),
                _ => {
                    let message = "returnDocument must be \"before\" or \"after\"";
                    return Err((args.len() - 1, message.into()));
                }
            },
            "returnNewDocument" => ("new", value),
            "projection" => ("fields", value),
            other => (other, value),
        };
        command.insert(key, value);
    }
    Ok(command)
}

// Mongosh's default name joins each `field_value`; a `name` option replaces it in place.
fn create_index(coll: &str, args: &[Bson]) -> Built {
    let key = as_doc(args.first());
    let name = key
        .iter()
        .map(|(field, v)| match v {
            Bson::String(s) => format!("{field}_{s}"),
            Bson::Int32(n) => format!("{field}_{n}"),
            Bson::Int64(n) => format!("{field}_{n}"),
            // f64's Display already drops an integral value's fraction, as JS does.
            Bson::Double(f) => format!("{field}_{f}"),
            other => format!("{field}_{other}"),
        })
        .collect::<Vec<_>>()
        .join("_");
    let index = on_top(doc! { "key": key, "name": name }, as_doc(args.get(1)));
    Ok(doc! { "createIndexes": coll, "indexes": [index] })
}

fn err<T>(message: String, at: usize) -> Result<T, ParseError> {
    Err(ParseError::new(message, at))
}

fn is_punct(t: Option<&Tok>, b: u8) -> bool {
    t.is_some_and(|t| t.kind == Kind::Punct(b))
}

fn here(c: &Cursor<'_>) -> usize {
    c.peek().map_or(c.end_offset(), |t| t.range.start)
}

fn statement(src: &str, c: &mut Cursor<'_>) -> Result<(Option<String>, MongoOp), ParseError> {
    let first = c.bump();
    match first.filter(|t| t.kind == Kind::Ident).map(|t| c.text(t)) {
        Some("use") => return use_name(src, c).map(|name| (None, MongoOp::Use(name))),
        Some("db") if c.at_end() || is_punct(c.peek(), b'.') || is_punct(c.peek(), b'[') => {}
        _ => {
            return err(
                NOT_JS.to_owned(),
                first.map_or(c.end_offset(), |t| t.range.start),
            );
        }
    }
    // Accessors name the collection until a `.name(` call, which is the method.
    let (mut db, mut segments, mut leading) = (None, Vec::<String>::new(), true);
    let (name_tok, args) = loop {
        let Some(tok) = c.bump() else {
            return err("expected a method call".to_owned(), c.end_offset());
        };
        match tok.kind {
            Kind::Punct(b'.') => {}
            Kind::Punct(b'[') => {
                let Some(key) = c.bump().filter(|t| t.kind == Kind::Str) else {
                    return err(NOT_JS.to_owned(), here(c));
                };
                if !is_punct(c.bump(), b']') {
                    return err("expected `]`".to_owned(), here(c));
                }
                segments.push(string_value(c.text(key)));
                leading = false;
                continue;
            }
            Kind::Punct(_) => return err(NOT_JS.to_owned(), tok.range.start),
            _ => return err("expected `;` or a new line".to_owned(), tok.range.start),
        }
        let Some(name) = c.bump().filter(|t| t.kind == Kind::Ident) else {
            return err("expected a name after `.`".to_owned(), here(c));
        };
        if !is_punct(c.peek(), b'(') {
            segments.push(c.text(name).to_owned());
            leading = false;
            continue;
        }
        let args = call(c)?;
        match c.text(name) {
            "getSiblingDB" if leading => db = Some(string_arg("getSiblingDB", args)?),
            "getCollection" if segments.is_empty() => {
                segments.push(string_arg("getCollection", args)?);
            }
            _ => break (name, args),
        }
        leading = false;
    };
    let name = c.text(name_tok);
    let scope = if segments.is_empty() {
        Scope::Database
    } else {
        Scope::Collection
    };
    let Some(row) = METHODS.iter().find(|r| r.name == name && r.scope == scope) else {
        let kind = if scope == Scope::Database {
            "database"
        } else {
            "collection"
        };
        return err(
            format!("`{name}` is not a supported {kind} method"),
            name_tok.range.start,
        );
    };
    let coll = segments.join(".");
    let (close, starts) = (
        args.close,
        args.values.iter().map(|(_, at)| *at).collect::<Vec<_>>(),
    );
    let mut command = (row.build)(&coll, &check(name, row.params, args)?)
        .map_err(|(i, m)| ParseError::new(m, starts.get(i).copied().unwrap_or(close)))?;
    modifiers(c, row, &mut command)?;
    let op = match row.method {
        Some(method) => MongoOp::Method { method, command },
        None => MongoOp::RunCommand(command),
    };
    Ok((db, op))
}

// What follows the call: only `find` takes cursor modifiers, each a `.name(arg)` that sets
// its command key, a repeat replacing the value in place.
fn modifiers(c: &mut Cursor<'_>, row: &Row, command: &mut Document) -> Result<(), ParseError> {
    while let Some(tok) = c.bump() {
        if tok.kind != Kind::Punct(b'.') {
            let at = tok.range.start;
            return match tok.kind {
                Kind::Punct(b) if !b",;:}])".contains(&b) => err(NOT_JS.to_owned(), at),
                _ => err("expected `;` or a new line".to_owned(), at),
            };
        }
        let next = c.bump().filter(|t| t.kind == Kind::Ident);
        let Some(next) = next else {
            return err("expected a name after `.`".to_owned(), here(c));
        };
        let modifier = MODIFIERS.iter().find(|(m, _)| *m == c.text(next));
        let (Some(&(key, kind)), Some(MongoMethod::Find), true) =
            (modifier, row.method, is_punct(c.peek(), b'('))
        else {
            let at = next.range.start;
            return err(
                format!("`.{}` cannot follow `{}`", c.text(next), row.name),
                at,
            );
        };
        let mut value = check(&format!(".{key}"), &[Req(kind)], call(c)?)?.remove(0);
        // Mongosh reads a negative limit as "at most n, in one batch".
        let single_batch = key == "limit" && negative_limit(&mut value);
        command.insert(key, value);
        if single_batch {
            command.insert("singleBatch", true);
        }
    }
    Ok(())
}

// Turns a negative limit into its magnitude, kept in its own type where it fits.
fn negative_limit(v: &mut Bson) -> bool {
    let abs = match *v {
        Bson::Int32(n) if n < 0 => n
            .checked_abs()
            .map_or(Bson::Int64(-i64::from(n)), Bson::Int32),
        Bson::Int64(n) if n < 0 => Bson::Int64(n.saturating_abs()),
        Bson::Double(f) if f < 0.0 => Bson::Double(-f),
        _ => return false,
    };
    *v = abs;
    true
}

// The name runs to the piece's last token, so `use my-db` takes the whole `my-db`.
fn use_name(src: &str, c: &mut Cursor<'_>) -> Result<String, ParseError> {
    let Some(start) = c.peek().map(|t| t.range.start) else {
        return err("`use` needs a database name".to_owned(), c.end_offset());
    };
    let name = &src[start..c.end_offset()];
    if name.contains(|ch: char| ch.is_whitespace() || "/\\.\"$".contains(ch)) {
        return err(format!("`{name}` is not a database name"), start);
    }
    Ok(name.to_owned())
}

struct Args {
    values: Vec<(Bson, usize)>,
    close: usize,
}

// The parenthesised arguments with where each starts, and where the `)` stands.
fn call(c: &mut Cursor<'_>) -> Result<Args, ParseError> {
    c.bump();
    let mut values = Vec::new();
    loop {
        if is_punct(c.peek(), b')') {
            let close = here(c);
            c.bump();
            return Ok(Args { values, close });
        }
        let at = here(c);
        values.push((value(c)?, at));
        let close = here(c);
        if separator(c, b')')? {
            return Ok(Args { values, close });
        }
    }
}

// The arguments against a method's parameters: count first, then each one's kind.
fn check(name: &str, params: &[Param], args: Args) -> Result<Vec<Bson>, ParseError> {
    if let Some((_, at)) = args.values.get(params.len()) {
        return match params.len() {
            0 => err(format!("`{name}` takes no arguments"), *at),
            1 => err(format!("`{name}` takes at most 1 argument"), *at),
            n => err(format!("`{name}` takes at most {n} arguments"), *at),
        };
    }
    for (i, param) in params.iter().enumerate() {
        let (kind, required) = match *param {
            Req(kind) => (kind, true),
            Opt(kind) => (kind, false),
        };
        match args.values.get(i) {
            None if required => {
                let needed = params.iter().filter(|p| matches!(p, Req(_))).count();
                let what = match needed {
                    1 => "an argument".to_owned(),
                    n => format!("{n} arguments"),
                };
                return err(format!("`{name}` needs {what}"), args.close);
            }
            Some((v, at)) if !kind.accepts(v) => {
                let n = i + 1;
                return err(
                    format!("argument {n} of `{name}` must be {}", kind.name()),
                    *at,
                );
            }
            _ => {}
        }
    }
    Ok(args.values.into_iter().map(|(v, _)| v).collect())
}

fn string_arg(name: &str, args: Args) -> Result<String, ParseError> {
    let at = args.values.first().map_or(args.close, |(_, at)| *at);
    match check(name, &[Req(Str)], args)?.pop() {
        Some(Bson::String(s)) => Ok(s),
        _ => err(format!("argument 1 of `{name}` must be a string"), at),
    }
}

#[cfg(test)]
mod tests;
