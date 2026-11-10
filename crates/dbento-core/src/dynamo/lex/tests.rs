use super::{Piece, SyntaxError, TokKind, quote_ident, quote_string, split, statement_ranges};

fn pieces(text: &str) -> Vec<Piece> {
    split(text).into_iter().map(|r| r.unwrap()).collect()
}

fn texts(text: &str) -> Vec<&str> {
    statement_ranges(text)
        .into_iter()
        .map(|r| &text[r])
        .collect()
}

fn fail(text: &str) -> SyntaxError {
    match split(text).pop() {
        Some(Err(e)) => e,
        other => panic!("{text:?} gave {other:?}"),
    }
}

#[test]
fn a_script_without_a_semicolon_is_one_statement() {
    assert_eq!(
        texts("SELECT * FROM \"orders\""),
        ["SELECT * FROM \"orders\""]
    );
    assert_eq!(statement_ranges("  SELECT 1  \n").first(), Some(&(2..10)));
}

#[test]
fn top_level_semicolons_separate_and_are_never_inside_a_range() {
    let text = "SELECT a FROM t;\nINSERT INTO t VALUE {'a': 1} ; DELETE FROM t WHERE a = 1;";
    assert_eq!(
        texts(text),
        [
            "SELECT a FROM t",
            "INSERT INTO t VALUE {'a': 1}",
            "DELETE FROM t WHERE a = 1"
        ]
    );
}

#[test]
fn semicolons_inside_strings_names_and_comments_do_not_separate() {
    let text = "SELECT \"a;b\" FROM t WHERE x = 'y;z' -- c;\n AND w = 1 /* ; */";
    assert_eq!(texts(text), [&text[..text.len() - 8]]);
}

#[test]
fn comments_before_and_after_the_tokens_are_outside_the_range() {
    let text = "-- lead\n/* block */ SELECT 1 /* mid */ + 2 -- tail\n;";
    assert_eq!(texts(text), ["SELECT 1 /* mid */ + 2"]);
    assert_eq!(statement_ranges(text).first(), Some(&(20..42)));
}

#[test]
fn pieces_of_only_whitespace_and_comments_yield_nothing() {
    assert!(split("").is_empty());
    assert!(split(" ;; -- only\n /* a */ ;").is_empty());
    assert_eq!(texts("SELECT 1;\n-- done\n"), ["SELECT 1"]);
}

#[test]
fn doubled_quotes_escape_inside_strings_and_names() {
    let text = "SELECT \"a\"\"b\" FROM t WHERE c = 'it''s'";
    let kinds: Vec<(TokKind, &str)> = pieces(text)[0]
        .toks
        .iter()
        .map(|t| (t.kind, &text[t.range.clone()]))
        .collect();
    assert_eq!(
        kinds,
        [
            (TokKind::Word, "SELECT"),
            (TokKind::Ident, "\"a\"\"b\""),
            (TokKind::Word, "FROM"),
            (TokKind::Word, "t"),
            (TokKind::Word, "WHERE"),
            (TokKind::Word, "c"),
            (TokKind::Punct, "="),
            (TokKind::Str, "'it''s'"),
        ]
    );
}

#[test]
fn numbers_words_and_punctuation_are_separate_tokens() {
    let text = "UPDATE t SET n = n + 1.5 WHERE pk = 'a' AND s IN <<1,2>>";
    let kinds: Vec<TokKind> = pieces(text)[0].toks.iter().map(|t| t.kind).collect();
    assert_eq!(kinds.iter().filter(|k| **k == TokKind::Number).count(), 3);
    assert_eq!(kinds.iter().filter(|k| **k == TokKind::Str).count(), 1);
    let last = pieces(text)[0].toks.last().cloned().unwrap();
    assert_eq!(&text[last.range], ">");
}

#[test]
fn an_unterminated_string_is_an_error_at_its_quote_and_takes_the_rest() {
    let text = "SELECT 1; SELECT * FROM t WHERE a = 'x; SELECT 2";
    let all = split(text);
    assert_eq!(all.len(), 2);
    assert!(all[0].is_ok());
    let e = fail(text);
    assert_eq!(e.message, "unterminated string");
    assert_eq!(e.range, 10..text.len());
    assert_eq!(e.offset, text.find('\'').unwrap() - 10);
    assert_eq!(statement_ranges(text), [0..8, 10..text.len()]);
}

#[test]
fn an_unterminated_name_or_block_comment_is_an_error_too() {
    let e = fail("SELECT * FROM \"orders");
    assert_eq!(
        (e.message.as_str(), e.offset, e.range),
        ("unterminated quoted name", 14, 0..21)
    );
    let e = fail("  /* never closed; SELECT 1");
    assert_eq!(
        (e.message.as_str(), e.offset, e.range),
        ("unterminated block comment", 0, 2..27)
    );
    let e = fail("SELECT 1 /* open");
    assert_eq!(
        (e.message.as_str(), e.offset),
        ("unterminated block comment", 9)
    );
}

#[test]
fn identifiers_and_strings_quote_as_partiql_reads_them() {
    assert_eq!(quote_ident("orders"), "\"orders\"");
    assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
    assert_eq!(quote_string("it's"), "'it''s'");
    assert_eq!(quote_string(""), "''");
}
