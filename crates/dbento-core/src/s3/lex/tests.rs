use super::{S3Statement, SyntaxError, parse_script, quote_literal, statement_ranges};

fn words(text: &str) -> Vec<Vec<String>> {
    parse_script(text)
        .into_iter()
        .map(|r| r.unwrap_or_else(|e| panic!("{text:?}: {e:?}")).words)
        .collect()
}

#[test]
fn a_command_is_its_words_and_its_trimmed_range() {
    assert_eq!(
        parse_script("  ls -r 's3://b/a b'  "),
        [Ok(S3Statement {
            words: vec!["ls".into(), "-r".into(), "s3://b/a b".into()],
            range: 2..20,
        })]
    );
}

#[test]
fn a_newline_or_a_semicolon_outside_quotes_ends_a_statement() {
    let text = "ls\nstat 's3://b/k'; cat s3://b/k\r\nls";
    assert_eq!(statement_ranges(text), [0..2, 3..18, 20..32, 34..36]);
    assert_eq!(
        words(text),
        [
            vec!["ls"],
            vec!["stat", "s3://b/k"],
            vec!["cat", "s3://b/k"],
            vec!["ls"]
        ]
    );
}

#[test]
fn a_quoted_word_keeps_newlines_semicolons_and_hashes() {
    let text = "cat 's3://b/a;b\n#c'\nls";
    assert_eq!(statement_ranges(text), [0..19, 20..22]);
    assert_eq!(words(text)[0], ["cat", "s3://b/a;b\n#c"]);
}

#[test]
fn two_quotes_inside_a_quoted_word_are_one_quote() {
    assert_eq!(words("stat 's3://b/it''s'"), [vec!["stat", "s3://b/it's"]]);
    assert_eq!(words("ls ''"), [vec!["ls", ""]]);
}

#[test]
fn a_hash_starting_a_word_comments_out_the_rest_of_the_line() {
    let text = "# every bucket\n  ls # the buckets\n#x; ls\nls s3://b/a#b";
    assert_eq!(statement_ranges(text), [17..19, 41..54]);
    assert_eq!(words(text), [vec!["ls"], vec!["ls", "s3://b/a#b"]]);
}

#[test]
fn blank_text_and_empty_statements_yield_nothing() {
    for text in ["", "   ", "\n;\n ; ", "# only a comment", ";;\r\n"] {
        assert!(parse_script(text).is_empty(), "{text:?}");
    }
}

#[test]
fn an_unterminated_quote_runs_to_the_end_and_ends_the_script() {
    let text = "ls\ncat 's3://b/k\nls";
    assert_eq!(
        parse_script(text),
        [
            Ok(S3Statement {
                words: vec!["ls".into()],
                range: 0..2,
            }),
            Err(SyntaxError {
                message: "unterminated quote".into(),
                offset: 4,
                range: 3..text.len(),
            }),
        ]
    );
    assert_eq!(statement_ranges(text), [0..2, 3..text.len()]);
}

#[test]
fn a_quote_touching_another_word_is_an_error_for_that_statement_only() {
    for (text, offset) in [("cat 's3://b/k'x\nls", 14), ("ls s3://b/'k'\nls", 10)] {
        let parsed = parse_script(text);
        let end = text.find('\n').unwrap();
        assert_eq!(
            parsed[0],
            Err(SyntaxError {
                message: "a quoted word must be separated from the next word by a space".into(),
                offset,
                range: 0..end,
            }),
            "{text:?}"
        );
        assert_eq!(
            parsed[1].as_ref().map(|s| s.range.clone()),
            Ok(end + 1..end + 3)
        );
    }
}

#[test]
fn a_quoted_key_lexes_back_to_itself() {
    for key in [
        "",
        "a b",
        "it's",
        "''",
        "x;y",
        "line\nbreak",
        "cr\r\nlf",
        "#hash",
        "tab\tz",
        "back\\slash",
        "\u{202e}gpj.exe",
        "\u{1b}[31mred",
        "日本/語",
    ] {
        let text = format!("stat {}", quote_literal(&format!("s3://b/{key}")));
        assert_eq!(
            words(&text),
            [vec!["stat".to_owned(), format!("s3://b/{key}")]],
            "{key:?}"
        );
        let ranges = statement_ranges(&text);
        assert_eq!(
            (ranges.len(), ranges[0].clone()),
            (1, 0..text.len()),
            "{key:?}"
        );
    }
}

#[test]
fn quote_literal_doubles_single_quotes_only() {
    assert_eq!(quote_literal("s3://b/it's"), "'s3://b/it''s'");
    assert_eq!(quote_literal("a\"b\\c"), "'a\"b\\c'");
}

#[test]
fn ranges_count_bytes_past_multibyte_text() {
    let text = "ls 's3://b/é'\nls";
    assert_eq!(statement_ranges(text), [0..14, 15..17]);
}
