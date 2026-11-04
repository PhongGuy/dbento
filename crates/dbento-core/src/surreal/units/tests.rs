use super::{Unit, UnitKind, UseTarget, statement_count, statement_ranges, units};

fn texts(src: &str) -> Vec<&str> {
    statement_ranges(src).into_iter().map(|r| &src[r]).collect()
}

fn only(src: &str) -> Unit {
    let mut all = units(src);
    assert_eq!(all.len(), 1, "{src}");
    all.remove(0)
}

fn used(namespace: Option<&str>, database: Option<&str>) -> UnitKind {
    UnitKind::Use(UseTarget {
        namespace: namespace.map(str::to_owned),
        database: database.map(str::to_owned),
    })
}

// F6 "Separators": `n` is the server's `statements` count.
#[test]
fn the_statement_count_matches_the_servers_begin_frame() {
    for (src, n) in [
        ("RETURN 1", 1),
        ("RETURN 1;", 1),
        ("RETURN 1; RETURN 2;", 2),
        ("RETURN 1;; RETURN 2", 2),
        ("; RETURN 1", 1),
        (";;", 0),
        (";", 0),
        ("", 0),
        ("   \n ", 0),
        ("/* a;b */", 0),
        ("-- a;b", 0),
        ("BEGIN; RETURN 1; COMMIT", 3),
        ("RETURN 'a;b'", 1),
        ("RETURN 'a\nb;c'", 1),
        ("SELECT `a;b` FROM seed", 1),
        ("RETURN `a;b`:1", 1),
        ("SELECT ⟨a;b⟩ FROM seed", 1),
        ("RETURN seed:⟨a;b⟩", 1),
        ("RETURN 1 -- x;y\n; RETURN 2", 2),
        ("RETURN 1 /* x;y */; RETURN 2", 2),
        ("{ RETURN 1; RETURN 2 }", 1),
        ("{ { RETURN 1; }; RETURN 2; }", 1),
        ("IF true { RETURN 1; RETURN 2 } ELSE { RETURN 3; }", 1),
        ("RETURN function() { let a = 'x;y'; return a; }", 1),
        ("RETURN s'a;b'", 1),
        ("RETURN 'a;b' = /a;b/", 1),
        ("string::matches('a;b', /a;b/)", 1),
        ("RETURN { a: 'x;y' }", 1),
        ("{a:1, b:'x;y'}; RETURN 2", 2),
        ("RETURN 'x' = /it's/; RETURN 2", 2),
        ("RETURN 'x' = /a\\/;b/; RETURN 2", 2),
        ("RETURN 4 / 2; RETURN 3 / 1", 2),
        ("RETURN 1 -- it's\n; RETURN 2", 2),
        ("RETURN 1 /* it's */; RETURN 2", 2),
        ("\"it's;\"; 'a\"b;'", 2),
    ] {
        assert_eq!(statement_count(src), n, "{src}");
    }
}

// A missing `;` is the server's parse error, for the whole call: one statement here.
#[test]
fn a_statement_without_a_separator_stays_one_unit() {
    for src in [
        "RETURN 1 RETURN 2",
        "RETURN 1\nRETURN 2",
        "{ RETURN 1 } RETURN 2",
        "IF true { RETURN 1; } RETURN 2",
        "FOR $i IN [1] { } RETURN 2",
        "DEFINE FUNCTION fn::x3() { RETURN 1; } RETURN 2",
    ] {
        assert_eq!(texts(src), [src], "{src}");
    }
}

#[test]
fn a_unit_starts_at_its_first_token_and_keeps_its_semicolon() {
    assert_eq!(
        texts("-- head\nSELECT 1 FROM t; /* mid */ RETURN 2 -- tail\n"),
        ["SELECT 1 FROM t;", "RETURN 2"]
    );
    assert!(texts("-- only a comment\n").is_empty());
}

#[test]
fn a_transaction_is_one_unit_through_its_commit_or_cancel() {
    assert_eq!(
        texts("RETURN 1; BEGIN; CREATE a:1; COMMIT; RETURN 2"),
        ["RETURN 1;", "BEGIN; CREATE a:1; COMMIT;", "RETURN 2"]
    );
    assert_eq!(
        texts("BEGIN TRANSACTION; CREATE a:1; CANCEL TRANSACTION; RETURN 2"),
        [
            "BEGIN TRANSACTION; CREATE a:1; CANCEL TRANSACTION;",
            "RETURN 2"
        ]
    );
    assert_eq!(
        texts("begin; create a:1; commit;"),
        ["begin; create a:1; commit;"]
    );
    assert_eq!(only("BEGIN; CREATE a:1; COMMIT").statements, 3);
    // Nested text does not close the span; a lone COMMIT is its own statement.
    assert_eq!(
        texts("BEGIN; { COMMIT; }; RETURN 1; COMMIT; RETURN 2"),
        ["BEGIN; { COMMIT; }; RETURN 1; COMMIT;", "RETURN 2"]
    );
    assert_eq!(texts("COMMIT; RETURN 2"), ["COMMIT;", "RETURN 2"]);
}

#[test]
fn an_unterminated_begin_runs_to_the_end_of_the_text() {
    let src = "RETURN 1; BEGIN; CREATE a:1; RETURN 2";
    assert_eq!(texts(src), ["RETURN 1;", "BEGIN; CREATE a:1; RETURN 2"]);
    assert_eq!(units(src)[1].statements, 3);
}

#[test]
fn a_let_runs_to_the_end_of_the_text() {
    let src = "RETURN 0; LET $a = 1; RETURN $a; DELETE t WHERE a = $a;";
    assert_eq!(
        texts(src),
        ["RETURN 0;", "LET $a = 1; RETURN $a; DELETE t WHERE a = $a;"]
    );
    assert_eq!(units(src)[1].statements, 3);
    assert_eq!(texts("let $a = 1; RETURN $a"), ["let $a = 1; RETURN $a"]);
}

#[test]
fn a_let_inside_a_transaction_starts_the_unit_at_that_begin() {
    let src = "RETURN 0; BEGIN; LET $a = 1; CREATE t SET a = $a; COMMIT; RETURN $a";
    assert_eq!(
        texts(src),
        [
            "RETURN 0;",
            "BEGIN; LET $a = 1; CREATE t SET a = $a; COMMIT; RETURN $a"
        ]
    );
    // A LET after the COMMIT is outside the span and starts its own unit.
    assert_eq!(
        texts("BEGIN; RETURN 1; COMMIT; LET $a = 1; RETURN $a"),
        ["BEGIN; RETURN 1; COMMIT;", "LET $a = 1; RETURN $a"]
    );
}

// The whole statement must be the USE grammar, or `USE DB d-2` would target `d`.
#[test]
fn a_literal_use_is_a_unit_of_its_own_with_no_text_to_send() {
    for (src, want) in [
        ("USE NS a DB b", used(Some("a"), Some("b"))),
        ("USE NS a DB b;", used(Some("a"), Some("b"))),
        ("use ns a db b", used(Some("a"), Some("b"))),
        ("USE NS a", used(Some("a"), None)),
        ("USE DB b", used(None, Some("b"))),
        ("USE NAMESPACE a DATABASE b", used(Some("a"), Some("b"))),
        ("USE DATABASE b", used(None, Some("b"))),
        ("USE NS `a b` DB ⟨c;d⟩", used(Some("a b"), Some("c;d"))),
        ("USE NS 'a' DB \"b\"", used(Some("a"), Some("b"))),
        (r"USE DB 'it\'s'", used(None, Some("it's"))),
        ("USE /* c */ DB -- d\n b", used(None, Some("b"))),
    ] {
        let u = only(src);
        assert_eq!((u.kind, u.refused_use), (want, false), "{src}");
    }
    assert_eq!(
        texts("USE NS a; USE DB b; RETURN 1"),
        ["USE NS a;", "USE DB b;", "RETURN 1"]
    );
}

#[test]
fn a_use_not_wholly_consumed_is_a_refused_text_unit() {
    for src in [
        "USE DB d-2",
        "USE DB (d2)",
        "USE DB d.e",
        "USE DB $d",
        "USE DB 1",
        "USE DB r'a'",
        "USE DB d e",
        "USE NS a NS b",
        "USE DB b NS a",
        "USE",
        "USE NS",
        "USE x",
        "USE DB é",
    ] {
        let u = only(src);
        assert_eq!((u.kind, u.refused_use), (UnitKind::Text, true), "{src}");
    }
}

#[test]
fn a_use_outside_its_own_unit_is_refused() {
    for src in [
        "BEGIN; USE DB d2; CREATE a1:1; COMMIT;",
        "LET $a = 1; USE DB d2; RETURN $a",
        "{ USE DB d2; RETURN 1 }",
        "IF true { USE NS a DB b }",
        "RETURN (USE DB d2)",
        "BEGIN; USE NS a",
    ] {
        let u = only(src);
        assert!(u.refused_use, "{src}");
        assert_eq!(u.kind, UnitKind::Text, "{src}");
    }
}

#[test]
fn a_name_that_only_looks_like_use_is_not_refused() {
    for src in [
        "SELECT use FROM t",
        "RETURN use",
        "SELECT `use` FROM t",
        "RETURN $use",
        "RETURN a.use",
        "{ a: 'USE DB d' }",
        "RETURN 'USE DB d'",
    ] {
        assert!(!only(src).refused_use, "{src}");
    }
}

#[test]
fn an_unterminated_token_makes_the_rest_one_unit() {
    let src = "RETURN 1; RETURN 'open; RETURN 3";
    assert_eq!(texts(src), ["RETURN 1;", "RETURN 'open; RETURN 3"]);
    let all = units(src);
    assert_eq!((all[0].lex_error, all[1].lex_error), (false, true));
    assert_eq!(statement_count(src), 2);
    for src in ["'open", "/* open", "`open", "⟨open", "RETURN /open"] {
        let u = only(src);
        assert_eq!((u.range, u.lex_error), (0..src.len(), true), "{src}");
    }
    // Inside a span the error still ends the text, and a USE cannot be literal.
    assert_eq!(only("BEGIN; RETURN 'open").range.end, 19);
    assert_eq!(only("USE DB 'open").kind, UnitKind::Text);
    assert!(only("USE DB 'open").refused_use);
}

// Spec Verification: random text never panics and ranges stay on char boundaries.
#[test]
fn random_text_never_panics_and_ranges_stay_ordered_on_char_boundaries() {
    const PIECES: [&str; 36] = [
        "'", "\"", "`", "⟨", "⟩", "/", "/*", "*/", "--", "#", "\n", ";", "{", "}", "(", ")", "[",
        "]", "::", "..", ".", "$a", "\\", " ", "1", "x", "é", "r'", "BEGIN", "COMMIT", "CANCEL",
        "LET", "USE", "DB", "function", "RETURN",
    ];
    // A small multiplicative generator keeps the cases the same on every run.
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = |bound: usize| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        ((state >> 33) as usize) % bound
    };
    for _ in 0..5000 {
        let src: String = (0..next(40)).map(|_| PIECES[next(PIECES.len())]).collect();
        let mut end = 0;
        for r in statement_ranges(&src) {
            assert!(r.start >= end && r.end >= r.start, "{src:?}");
            assert!(
                src.is_char_boundary(r.start) && src.is_char_boundary(r.end),
                "{src:?}"
            );
            end = r.end;
        }
        assert!(
            statement_count(&src) >= statement_ranges(&src).len(),
            "{src:?}"
        );
    }
}

// The server's own escapes are not all decoded here, so a name holding one could target the
// wrong database. F6 "Escapes" lists what the quoted forms accept.
#[test]
fn a_name_with_an_escape_beyond_its_own_mark_is_a_refused_text_unit() {
    for src in [
        r"USE DB 'a\nb'",
        r"USE DB 'a\u{e9}'",
        r"USE DB `a\nb`",
        r"USE DB `a\qb`",
        r#"USE DB 'a\"b'"#,
        r"USE DB ⟨a\nb⟩",
        r"USE DB ⟨a\\b⟩",
        r"USE DB ⟨a\u{e9}⟩",
        r"USE DB ⟨a\⟩",
    ] {
        let u = only(src);
        assert_eq!((u.kind, u.refused_use), (UnitKind::Text, true), "{src}");
    }
    let u = only(r"USE NS `a\`b` DB 'c\\d'");
    assert_eq!(u.kind, used(Some("a`b"), Some(r"c\d")));
}

// F6 "Names that collide with verbs": `{use: 1}` parses, and `use:1` is no USE statement.
#[test]
fn use_as_an_object_key_or_record_table_is_not_refused() {
    for src in [
        "{ use: 1 }",
        "RETURN { use: 1 }",
        "{ a: 1, use: 2 }",
        "CREATE use:1",
    ] {
        assert!(!only(src).refused_use, "{src}");
    }
}

// Both cases hold a `/` that the two lexer readings disagree on, so each reading cuts elsewhere.
#[test]
fn a_cut_the_two_readings_disagree_on_runs_its_unit_to_the_end() {
    for (src, n) in [
        (
            "SELECT * FROM vt ORDER value / 2; CREATE o1:1; RETURN 4 / 2",
            3,
        ),
        (
            "SELECT VALUE /it's/ FROM vt; DELETE x; SELECT VALUE /it's/ FROM vt",
            3,
        ),
    ] {
        assert_eq!(texts(src), [src], "{src}");
        assert_eq!(statement_count(src), n, "{src}");
    }
}

#[test]
fn units_before_the_disagreement_stay_cut() {
    let tail = "SELECT * FROM vt ORDER value / 2; CREATE o1:1; RETURN 4 / 2";
    let src = format!("RETURN 1; RETURN 2; {tail}");
    assert_eq!(texts(&src), ["RETURN 1;", "RETURN 2;", tail]);
    assert_eq!(units(&src)[2].statements, 3);
}

#[test]
fn a_disagreement_inside_a_transaction_takes_the_span_to_the_end() {
    let src = "BEGIN; SELECT a / 2 FROM t; COMMIT; RETURN 3 / 1";
    assert_eq!(texts(src), [src]);
}

#[test]
fn a_second_reading_with_the_same_cuts_splits_as_usual() {
    let src = "SELECT a / 2 / 3 FROM t; RETURN 1";
    assert_eq!(texts(src), ["SELECT a / 2 / 3 FROM t;", "RETURN 1"]);
}

// The primary reading swallows the `USE` in a regex, or stops at one left open; the second
// reading shows it.
#[test]
fn a_use_only_the_second_reading_shows_is_refused() {
    for src in [
        "SELECT VALUE / 2; USE DB zz; SELECT VALUE / 2",
        "SELECT VALUE / 2; USE DB zz",
    ] {
        let u = only(src);
        assert!(u.refused_use, "{src}");
        assert_eq!(u.kind, UnitKind::Text, "{src}");
    }
}

#[test]
fn the_statement_count_is_the_higher_of_the_two_readings() {
    for (src, n) in [
        ("SELECT VALUE / 2; DELETE x; SELECT VALUE / 2", 3),
        (
            "SELECT * FROM vt ORDER value / 2; CREATE o1:1; RETURN 4 / 2",
            3,
        ),
    ] {
        assert_eq!(statement_count(src), n, "{src}");
    }
}

// A literal USE ahead of the doubt keeps its RPC; one behind it is text the server would run.
#[test]
fn a_use_after_the_disagreement_is_refused() {
    let src = "USE DB a; SELECT VALUE /;/ FROM t; USE DB b";
    let all = units(src);
    assert_eq!(all.len(), 2);
    assert_eq!(
        (all[0].kind.clone(), all[0].refused_use),
        (used(None, Some("a")), false)
    );
    assert_eq!(
        (all[1].kind.clone(), all[1].refused_use),
        (UnitKind::Text, true)
    );
    assert_eq!(all[1].range.end, src.len());
}

// The fragment after a closed statement belongs to any span still open.
#[test]
fn a_lex_error_after_a_closed_statement_stays_in_its_span() {
    assert_eq!(only("BEGIN; 'open").range, 0..12);
    assert_eq!(only("LET $a = 1; 'open").statements, 2);
    assert_eq!(texts("BEGIN; COMMIT; 'open"), ["BEGIN; COMMIT;", "'open"]);
    assert!(only("BEGIN; 'open").lex_error);
}

#[test]
fn a_unit_knows_whether_it_opens_a_transaction() {
    let flags = |src: &str| -> Vec<bool> { units(src).iter().map(|u| u.transaction).collect() };
    assert_eq!(flags("BEGIN; CREATE a:1; COMMIT; RETURN 1"), [true, false]);
    assert_eq!(flags("begin transaction; RETURN 1;"), [true]);
    assert_eq!(flags("LET $a = 1; BEGIN; COMMIT"), [false]);
}
