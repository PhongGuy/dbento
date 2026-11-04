use super::{Kind, LexError, lex, regex_parts, string_value};
use Kind::*;

fn kinds(src: &str) -> Vec<(Kind, &str)> {
    let (toks, err) = lex(src);
    assert_eq!(err, None, "{src}");
    toks.iter()
        .map(|t| (t.kind, &src[t.range.clone()]))
        .collect()
}

#[test]
fn tokens_keep_their_kind_and_text() {
    let (dot, open, close, colon) = (
        (Punct(b'.'), "."),
        (Punct(b'{'), "{"),
        (Punct(b'}'), "}"),
        (Punct(b':'), ":"),
    );
    for (src, want) in [
        (
            "db.t.find({n: -1.5e3})",
            vec![
                (Ident, "db"),
                dot,
                (Ident, "t"),
                dot,
                (Ident, "find"),
                (Punct(b'('), "("),
                open,
                (Ident, "n"),
                colon,
                (Punct(b'-'), "-"),
                (Num, "1.5e3"),
                close,
                (Punct(b')'), ")"),
            ],
        ),
        (
            "$match café_1 'a;b'",
            vec![(Ident, "$match"), (Ident, "café_1"), (Str, "'a;b'")],
        ),
        (
            "2.0 1e3 1E+2 7",
            vec![(Num, "2.0"), (Num, "1e3"), (Num, "1E+2"), (Num, "7")],
        ),
        (
            "1.x 2e",
            vec![(Num, "1"), dot, (Ident, "x"), (Num, "2"), (Ident, "e")],
        ),
        (
            "a = `t`",
            vec![
                (Ident, "a"),
                (Punct(b'='), "="),
                (Punct(b'`'), "`"),
                (Ident, "t"),
                (Punct(b'`'), "`"),
            ],
        ),
        (
            r"{p: /[/;]x\/y/im, q: /a/}",
            vec![
                open,
                (Ident, "p"),
                colon,
                (Regex, r"/[/;]x\/y/im"),
                (Punct(b','), ","),
                (Ident, "q"),
                colon,
                (Regex, "/a/"),
                close,
            ],
        ),
    ] {
        assert_eq!(kinds(src), want, "{src}");
    }
    assert_eq!(regex_parts(r"/[/;]x\/y/im"), (r"[/;]x\/y", "im"));
    assert_eq!(regex_parts("/a/"), ("a", ""));
}

#[test]
fn comments_are_skipped_and_line_breaks_are_recorded() {
    let src = "db // one\n.a /* two\nlines */ .b /* same */ c\r\nd";
    let (toks, err) = lex(src);
    assert_eq!(err, None);
    let seen: Vec<(&str, bool)> = toks
        .iter()
        .map(|t| (&src[t.range.clone()], t.line_break_before))
        .collect();
    assert_eq!(
        seen,
        [
            ("db", false),
            (".", true),
            ("a", false),
            (".", true),
            ("b", false),
            ("c", false),
            ("d", true)
        ]
    );
}

#[test]
fn strings_decode_their_escapes() {
    for (lit, want) in [
        (r#""a\"b""#, "a\"b"),
        (r"'it\'s'", "it's"),
        (r#"'say "hi"'"#, "say \"hi\""),
        (r#""t\tn\nr\rb\\""#, "t\tn\nr\rb\\"),
        (r#""\0\b\f\v""#, "\0\u{8}\u{c}\u{b}"),
        (r#""\u00e9\x41\u{1F600}""#, "éA😀"),
        (r#""\q\/""#, "q/"),
        ("\"a\\\nb\"", "ab"),
        (r#""\uD83D\uDE00""#, "😀"),
        ("\"a\\\r\nb\"", "ab"),
    ] {
        let (toks, err) = lex(lit);
        assert_eq!((err, toks.len(), toks[0].kind), (None, 1, Str), "{lit}");
        assert_eq!(string_value(&lit[toks[0].range.clone()]), want, "{lit}");
    }
}

#[test]
fn a_bad_token_stops_the_lex_at_its_first_byte() {
    for (src, message, at) in [
        ("db.a.find({s: 'open", "unterminated string", 14),
        ("x \"a\nb\"", "unterminated string", 2),
        ("db /* open", "unterminated comment", 3),
        ("{p: /ab", "unterminated regular expression", 4),
        ("{p: /a\n/}", "unterminated regular expression", 4),
        ("'\\u12'", "bad escape in string", 0),
        ("'\\uD83D'", "bad escape in string", 0),
        ("'\\u{41'", "bad escape in string", 0),
        ("'a\rb'", "unterminated string", 0),
    ] {
        assert_eq!(lex(src).1, Some(LexError { message, at }), "{src}");
    }
    assert_eq!(
        lex("db.a 'open").0.len(),
        3,
        "the tokens before the error are kept"
    );
}
