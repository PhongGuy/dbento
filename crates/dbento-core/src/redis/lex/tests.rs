use super::{RedisStatement, SyntaxError, parse_script, statement_ranges};

fn b(words: &[&str]) -> Vec<Vec<u8>> {
    words.iter().map(|w| w.as_bytes().to_vec()).collect()
}

fn args(text: &str) -> Vec<Vec<Vec<u8>>> {
    parse_script(text)
        .into_iter()
        .map(|r| r.unwrap().args)
        .collect()
}

fn one(text: &str) -> Vec<Vec<u8>> {
    let mut all = args(text);
    assert_eq!(all.len(), 1, "{text:?}");
    all.remove(0)
}

fn fail(text: &str) -> SyntaxError {
    match parse_script(text).remove(0) {
        Err(e) => e,
        Ok(s) => panic!("{text:?} parsed as {s:?}"),
    }
}

#[test]
fn words_split_on_whitespace_and_the_range_is_the_trimmed_line() {
    assert_eq!(
        parse_script("  HGETALL\tuser:1  "),
        [Ok(RedisStatement {
            args: b(&["HGETALL", "user:1"]),
            range: 2..16,
        })]
    );
    assert_eq!(one("GET\u{b}a\u{c}b"), b(&["GET", "a", "b"]));
}

#[test]
fn each_non_blank_line_is_a_statement_and_hash_lines_are_comments() {
    let text = "GET a\r\n\n   # SET x 1\nSET b 1\r\n\t#\n";
    assert_eq!(args(text), [b(&["GET", "a"]), b(&["SET", "b", "1"])]);
    assert_eq!(statement_ranges(text), [0..5, 21..28]);
}

#[test]
fn a_mid_line_hash_and_a_semicolon_are_ordinary_characters() {
    assert_eq!(one("GET #x"), b(&["GET", "#x"]));
    assert_eq!(one("GET a; GET b"), b(&["GET", "a;", "GET", "b"]));
}

#[test]
fn double_quotes_take_the_redis_cli_escapes() {
    assert_eq!(
        one(r#"SET k "a\n\r\t\b\a\\\"\x41\xff\q""#),
        [
            b"SET".to_vec(),
            b"k".to_vec(),
            b"a\n\r\t\x08\x07\\\"A\xffq".to_vec()
        ]
    );
}

#[test]
fn a_bad_hex_escape_keeps_the_letter_and_utf8_passes_through() {
    assert_eq!(one(r#"SET k "\x4g""#)[2], b"x4g");
    let s = one("SET k \"é\" ü");
    assert_eq!(s[2], "é".as_bytes());
    assert_eq!(s[3], "ü".as_bytes());
}

#[test]
fn single_quotes_take_only_an_escaped_single_quote() {
    assert_eq!(one(r"SET k 'it\'s \n'")[2], br"it's \n");
}

#[test]
fn a_quote_may_open_mid_word_and_an_empty_quote_is_an_empty_argument() {
    assert_eq!(one(r#"SET k ab"c d""#), b(&["SET", "k", "abc d"]));
    assert_eq!(one(r#"SET k """#), b(&["SET", "k", ""]));
    assert_eq!(one("SET k ''"), b(&["SET", "k", ""]));
}

#[test]
fn an_unterminated_quote_points_at_its_opening() {
    for (text, offset) in [
        (r#"SET k "abc"#, 6),
        ("  SET k 'abc", 6),
        (r#"SET k "abc\"#, 6),
    ] {
        let e = fail(text);
        assert!(
            e.message.contains("unterminated"),
            "{text:?}: {}",
            e.message
        );
        assert_eq!(e.offset, offset, "{text:?}");
    }
    assert_eq!(fail("  SET k 'abc").range, 2..12);
}

#[test]
fn a_closing_quote_glued_to_a_word_is_an_error_at_the_glued_character() {
    for (text, offset) in [
        (r#"SET k "a"b"#, 9),
        ("SET k 'a'b", 9),
        (r#"SET "k""v""#, 7),
    ] {
        let e = fail(text);
        assert!(
            e.message.contains("closing quote"),
            "{text:?}: {}",
            e.message
        );
        assert_eq!(e.offset, offset, "{text:?}");
    }
}

#[test]
fn an_error_ends_only_its_own_line_and_ranges_match_the_parse() {
    let got = parse_script("GET a\nSET k \"x\nGET b\n");
    assert_eq!(got.len(), 3);
    assert_eq!(got[0].as_ref().unwrap().args, b(&["GET", "a"]));
    assert_eq!(got[1].as_ref().unwrap_err().range, 6..14);
    assert_eq!(got[2].as_ref().unwrap().range, 15..20);
    assert!(parse_script("").is_empty());
    assert!(parse_script(" \n\t\r\n# only\n").is_empty());
    let text = "PING\n  ECHO \"a b\"  \nSET k 'x\n";
    let ranges: Vec<_> = parse_script(text)
        .into_iter()
        .map(|r| match r {
            Ok(s) => s.range,
            Err(e) => e.range,
        })
        .collect();
    assert_eq!(statement_ranges(text), ranges);
    assert_eq!(ranges, [0..4, 7..17, 20..28]);
}
