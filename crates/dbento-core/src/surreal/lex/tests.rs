use super::{Kind, Tok, alternate, lex, quote_ident, quote_string};

fn toks(src: &str) -> Vec<Tok> {
    let (toks, error) = lex(src);
    assert_eq!(error, None, "{src}");
    toks
}

fn texts(src: &str) -> Vec<&str> {
    toks(src).into_iter().map(|t| &src[t.range]).collect()
}

fn one(src: &str) -> Kind {
    let mut all = toks(src);
    assert_eq!(all.len(), 1, "{src}");
    all.remove(0).kind
}

fn ident(name: &str, quoted: bool) -> Kind {
    Kind::Ident {
        name: name.to_owned(),
        quoted,
    }
}

// F6 "Comments".
#[test]
fn the_three_line_comments_and_the_block_comment_are_skipped() {
    assert_eq!(texts("RETURN 1--1"), ["RETURN", "1"]);
    assert_eq!(texts("RETURN 4 // 2\n"), ["RETURN", "4"]);
    assert_eq!(texts("RETURN 1 #2\n"), ["RETURN", "1"]);
    assert_eq!(
        texts("RETURN 1 -- it's\n; RETURN 2"),
        ["RETURN", "1", ";", "RETURN", "2"]
    );
    assert_eq!(
        texts("RETURN 1 /* it's */; RETURN 2"),
        ["RETURN", "1", ";", "RETURN", "2"]
    );
    assert_eq!(texts("-- a;b"), [""; 0]);
    assert_eq!(texts("/* a;b */"), [""; 0]);
    // A comment does not open inside a string.
    assert_eq!(texts("RETURN '-- a'"), ["RETURN", "'-- a'"]);
    assert_eq!(
        texts("RETURN '/*'; RETURN '*/'"),
        ["RETURN", "'/*'", ";", "RETURN", "'*/'"]
    );
    assert_eq!(texts("RETURN '#a;'"), ["RETURN", "'#a;'"]);
}

#[test]
fn a_line_comment_ends_at_each_line_terminator_the_server_has() {
    for opener in ["--", "//", "#"] {
        for end in ["\n", "\r", "\u{85}", "\u{2028}", "\u{2029}"] {
            let src = format!("RETURN 1; {opener} x{end}DELETE ed; RETURN 2");
            assert!(toks(&src).iter().any(|t| t.is_word("delete")), "{src:?}");
        }
        for not_end in ["\u{b}", "\u{c}"] {
            let src = format!("RETURN 1; {opener} x{not_end}DELETE ed; RETURN 2");
            assert!(!toks(&src).iter().any(|t| t.is_word("delete")), "{src:?}");
        }
    }
}

#[test]
fn a_block_comment_does_not_nest() {
    // The first `*/` closes it, so the second is ordinary text.
    let src = "/* a /* b */ RETURN 1; */ RETURN 2";
    let (toks, _) = lex(src);
    let head: Vec<&str> = toks.iter().take(4).map(|t| &src[t.range.clone()]).collect();
    assert_eq!(head, ["RETURN", "1", ";", "*"]);
}

// F6 "Escapes", "Prefixes".
#[test]
fn strings_take_backslash_escapes_only() {
    for src in [
        r"'a\'b;c'",
        r#""a\"b""#,
        r"'a\\'",
        r"'a\nb'",
        r"'\u{e9}'",
        "'a\nb;c'",
        r#""it's;""#,
        r#"'a"b;'"#,
    ] {
        assert_eq!(one(src), Kind::Str { prefix: None }, "{src}");
    }
    assert_eq!(texts(r"'a\\'; RETURN 2").len(), 4);
    // Doubling is two adjacent tokens, not an escape.
    assert_eq!(texts("'a''b'"), ["'a'", "'b'"]);
    assert_eq!(texts("\"a\"\"b\""), ["\"a\"", "\"b\""]);
    assert_eq!(texts("`a``b`"), ["`a`", "`b`"]);
}

#[test]
fn prefixed_strings_keep_their_letter_with_either_quote() {
    for (src, p) in [
        ("r'seed:1'", b'r'),
        ("d'2020-01-01T00:00:00Z'", b'd'),
        ("u'018f0000-0000-7000-8000-000000000000'", b'u'),
        ("b'6162'", b'b'),
        ("s'a;b'", b's'),
        ("f'bk:/a.txt'", b'f'),
        ("r\"seed:⟨a;b⟩\"", b'r'),
    ] {
        assert_eq!(one(src), Kind::Str { prefix: Some(p) }, "{src}");
    }
    // A longer word or another letter before a quote is a name and a string.
    assert_eq!(texts("xr'a'"), ["xr", "'a'"]);
    assert_eq!(texts("q'a'"), ["q", "'a'"]);
}

// F6 "`;` that does not split".
#[test]
fn quoted_names_lose_their_quoting() {
    assert_eq!(one("`a;b`"), ident("a;b", true));
    assert_eq!(one("⟨a;b⟩"), ident("a;b", true));
    assert_eq!(one(r"`a\`b`"), ident("a`b", true));
    assert_eq!(one(r"`a\\b`"), ident(r"a\b", true));
    assert_eq!(one("`http`"), ident("http", true));
    assert_eq!(one("select"), ident("select", false));
    assert_eq!(
        toks("seed:⟨a;b⟩")[2].kind,
        ident("a;b", true),
        "a quoted record key"
    );
}

// F6 "Regex literals exist".
#[test]
fn a_slash_is_a_regex_in_expression_position_and_a_divide_after_a_value() {
    assert_eq!(texts("RETURN /a;b/"), ["RETURN", "/a;b/"]);
    assert_eq!(
        texts("RETURN 'x' = /it's/; RETURN 2"),
        ["RETURN", "'x'", "=", "/it's/", ";", "RETURN", "2"]
    );
    assert_eq!(
        texts(r"RETURN 'x' = /a\/;b/; RETURN 2"),
        ["RETURN", "'x'", "=", r"/a\/;b/", ";", "RETURN", "2"]
    );
    assert_eq!(
        texts("RETURN 4 / 2; RETURN 3 / 1"),
        ["RETURN", "4", "/", "2", ";", "RETURN", "3", "/", "1"]
    );
    assert_eq!(
        texts("string::matches('a;b', /a;b/)"),
        ["string", "::", "matches", "(", "'a;b'", ",", "/a;b/", ")"]
    );
    assert_eq!(texts("SELECT a / 2 FROM t")[2], "/");
    assert_eq!(texts("RETURN (1) / 2")[4], "/");
    assert_eq!(toks("RETURN $a / 2")[2].kind, Kind::Punct('/'));
}

#[test]
fn a_word_used_as_a_name_leaves_a_slash_a_divide() {
    let all = toks("UPDATE vt SET b = value / 2; CREATE h1:1; RETURN 4 / 2");
    assert_eq!(all.iter().filter(|t| t.is_punct(';')).count(), 2);
    assert!(all.iter().any(|t| t.is_word("create")));
    for src in [
        "a.value / 2",
        "$o.set / 2",
        "$o.from / 2",
        "a.* / 2",
        "SELECT in / 2 FROM t",
        "a = set / 2",
    ] {
        let all = toks(src);
        assert!(all.iter().any(|t| t.is_punct('/')), "{src}");
        assert!(all.iter().all(|t| t.kind != Kind::Regex), "{src}");
    }
}

#[test]
fn a_slash_after_an_infix_clause_or_opening_word_is_a_regex() {
    for head in [
        "a IN",
        "a IS",
        "a IS NOT",
        "a NOT IN",
        "a CONTAINSNOT",
        "WHERE a =",
        "RETURN",
        "THROW",
        "SELECT * FROM t LIMIT",
    ] {
        let src = format!("{head} /it's;b/; RETURN 2");
        let all = toks(&src);
        let regex: Vec<&str> = all
            .iter()
            .filter(|t| t.kind == Kind::Regex)
            .map(|t| &src[t.range.clone()])
            .collect();
        assert_eq!(regex, ["/it's;b/"], "{src}");
        assert_eq!(all.iter().filter(|t| t.is_punct(';')).count(), 1, "{src}");
    }
}

// Whether either reading of the text's doubtful slashes shows `word`.
fn either_shows(src: &str, word: &str) -> bool {
    let (primary, _) = lex(src);
    let second = alternate(src).unwrap_or_default();
    primary.iter().chain(&second).any(|t| t.is_word(word))
}

#[test]
fn a_statement_behind_a_doubtful_slash_shows_in_one_reading() {
    for (src, verb) in [
        (
            "SELECT * FROM vt ORDER value / 2; CREATE o1:1; RETURN 4 / 2",
            "create",
        ),
        (
            "SELECT VALUE /it's/ FROM vt; DELETE x; SELECT VALUE /it's/ FROM vt",
            "delete",
        ),
        ("SELECT * FROM t START /x/; DELETE x", "delete"),
        // The primary guesses wrong here in each direction; only the flip shows the verb.
        ("SELECT value / 2 FROM t; DELETE x; RETURN 4 / 2", "delete"),
        ("RETURN 1 NEWCLAUSE /it's/; DELETE x", "delete"),
    ] {
        assert!(either_shows(src, verb), "{src}");
    }
}

#[test]
fn a_double_star_is_an_operator_so_a_regex_may_follow() {
    let all = toks("RETURN 2 ** /a;b/");
    assert_eq!(all.last().map(|t| &t.kind), Some(&Kind::Regex));
    assert!(all.iter().all(|t| !t.is_punct(';')));
}

#[test]
fn arithmetic_with_one_slash_has_no_alternate() {
    assert_eq!(alternate("RETURN total / 2"), None);
    assert_eq!(alternate("RETURN 1 / 2"), None);
    assert_eq!(alternate("RETURN 'a'"), None);
    assert_eq!(alternate("a.value /it's/"), None, "a field is no doubt");
}

#[test]
fn a_flip_across_two_divides_invents_no_statement() {
    let src = "SELECT a / 2, b / 3 FROM t";
    let (primary, _) = lex(src);
    let second = alternate(src).unwrap_or_default();
    for tok in primary.iter().chain(&second) {
        assert!(!tok.is_punct(';'), "{tok:?}");
        for verb in ["create", "delete", "update", "upsert", "insert"] {
            assert!(!tok.is_word(verb), "{tok:?}");
        }
    }
}

#[test]
fn alternate_survives_text_that_stops_short() {
    for src in [
        "/",
        "a /",
        "a /\\",
        "RETURN a / 'open",
        "RETURN a /open",
        "a / `b",
        "a / ⟨b",
        "a /*",
        "a /é",
        "é / é /",
        "é/ é/é",
        "⟨a / b",
        "function() { /",
    ] {
        let _ = alternate(src);
    }
}

#[test]
fn a_field_after_order_by_or_limit_by_divides() {
    for src in [
        "SELECT * FROM vt ORDER BY value / 2",
        "SELECT * FROM t LIMIT BY value / 2",
    ] {
        let all = toks(src);
        assert!(all.iter().any(|t| t.is_punct('/')), "{src}");
        assert!(all.iter().all(|t| t.kind != Kind::Regex), "{src}");
    }
    assert_eq!(
        texts("SELECT * FROM vt ORDER BY value")[4..],
        ["ORDER", "BY", "value"]
    );
}

#[test]
fn dots_and_paths_are_told_apart() {
    assert_eq!(
        toks("a.b")[1].kind,
        Kind::Punct('.'),
        "a single dot is a field access"
    );
    assert_eq!(texts("g:1..3"), ["g", ":", "1", "..", "3"]);
    assert_eq!(texts("g:1..=2"), ["g", ":", "1", "..", "=", "2"]);
    assert_eq!(texts("1..delete vt"), ["1", "..", "delete", "vt"]);
    assert_eq!(texts("seed:1.delete"), ["seed", ":", "1", ".", "delete"]);
    assert_eq!(texts("1.5dec"), ["1.5dec"]);
    assert_eq!(texts("300ms"), ["300ms"]);
    for src in [
        "http::post",
        "http :: post",
        "`http` :: post",
        "http\n::\npost",
    ] {
        let all = toks(src);
        assert_eq!(
            (all[0].name(), &all[1].kind, all[2].name()),
            (Some("http"), &Kind::PathSep, Some("post")),
            "{src}"
        );
    }
    assert_eq!(one("$create"), Kind::Param);
    assert_eq!(texts("USE DB d-2"), ["USE", "DB", "d", "-", "2"]);
    assert_eq!(texts("é"), ["é"]);
}

// F6 "Scripts": the body is one token, whatever braces its strings and comments hold.
#[test]
fn a_script_body_is_one_token() {
    for body in [
        "{ let a = 'x;y'; return a; }",
        "{ return '}'; }",
        "{ return '{'; }",
        "{ // }\n return 1; }",
        "{ // it's\n return 1; }",
        "{ return `a${1}};b`; }",
        "{ const f = () => { return 1; }; return f(); }",
        "{ let a = 1; a--; return a; }",
        "{ return 1 # 2; }",
    ] {
        let src = format!("RETURN function() {body}; RETURN 2");
        let all = toks(&src);
        let at = all.iter().position(|t| t.kind == Kind::Script);
        let at = at.unwrap_or_else(|| panic!("no script in {src}"));
        assert_eq!(&src[all[at].range.clone()], body);
        assert_eq!(all.len(), at + 4, "{src}");
    }
    let src = "RETURN function(1, 'a;b') { return 1; }";
    assert_eq!(
        texts(src),
        [
            "RETURN",
            "function",
            "(",
            "1",
            ",",
            "'a;b'",
            ")",
            "{ return 1; }"
        ]
    );
    // Only a `{` right after the argument list is a body.
    assert!(
        toks("RETURN function")
            .iter()
            .all(|t| t.kind != Kind::Script)
    );
    assert!(
        toks("RETURN `function`() { 1 }")
            .iter()
            .all(|t| t.kind != Kind::Script)
    );
    assert!(toks("{ a: 1 }").iter().all(|t| t.kind != Kind::Script));
    // A path or field named `function` is followed by an ordinary block, whose verbs must show.
    for src in [
        "IF fn::function() { DELETE t }",
        "IF $a.function() { DELETE t }",
    ] {
        assert!(toks(src).iter().any(|t| t.is_word("delete")), "{src}");
    }
}

#[test]
fn an_unterminated_token_stops_the_lexer_where_it_opened() {
    for (src, at) in [
        ("RETURN 'open", 7),
        ("RETURN \"open", 7),
        ("RETURN `open", 7),
        ("RETURN ⟨open", 7),
        ("RETURN 1 /* open", 9),
        ("RETURN r'open", 7),
        ("RETURN /open", 7),
        ("RETURN function() { return 1;", 18),
    ] {
        let (toks, error) = lex(src);
        assert_eq!(error.map(|e| e.at), Some(at), "{src}");
        assert!(!toks.is_empty(), "{src}");
    }
}

#[test]
fn quoting_round_trips_through_the_lexer() {
    for name in ["plain", "a b", "a`b", r"a\b", "a⟩b"] {
        assert_eq!(one(&quote_ident(name)), ident(name, true), "{name}");
    }
    assert_eq!(quote_string(r"it's a \"), r"'it\'s a \\'");
    assert_eq!(one(&quote_string(r"it's a \")), Kind::Str { prefix: None });
}
