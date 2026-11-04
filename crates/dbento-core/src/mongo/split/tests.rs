use super::{split, statement_ranges};

#[test]
fn statement_ranges_follow_semicolons_line_breaks_and_brackets() {
    let chained = "db.a.find()\n  .sort({x: 1})\n  .limit(2)";
    let chain_src = format!("{chained}\ndb.b.find()");
    let multi = "db.a.insertOne({\n  a: 1,\n  b: ';'\n})";
    let (multi_semi, multi_src) = (format!("{multi};"), format!("{multi};db.b.drop()"));
    let hidden = "db.b.find({re: /a;b/, s: \"x;\\\"y\", t: 'it\\'s;'})";
    let comments = format!("// lead; c\ndb.a.find() // tail; c\n/* ; */ {hidden}");
    for (src, want) in [
        (
            "db.a.find(); db.b.find()",
            vec!["db.a.find();", "db.b.find()"],
        ),
        (
            "db.a.find()\ndb.b.find()\n",
            vec!["db.a.find()", "db.b.find()"],
        ),
        (
            "db.a.find()\r\ndb.b.find()",
            vec!["db.a.find()", "db.b.find()"],
        ),
        ("use shop\ndb.a.find()", vec!["use shop", "db.a.find()"]),
        // Same line, no `;`: one piece, which the parser refuses.
        ("db.a.find() db.b.find()", vec!["db.a.find() db.b.find()"]),
        (chain_src.as_str(), vec![chained, "db.b.find()"]),
        ("db.a.\nfind()", vec!["db.a.\nfind()"]),
        (multi_src.as_str(), vec![multi_semi.as_str(), "db.b.drop()"]),
        (
            "db.a.find()\n;db.b.find()",
            vec!["db.a.find()", "db.b.find()"],
        ),
        (comments.as_str(), vec!["db.a.find()", hidden]),
        ("db.a.find({p: /[/;]x/i})", vec!["db.a.find({p: /[/;]x/i})"]),
        ("", vec![]),
        (";;  ;\n", vec![]),
        ("  // only a comment\n/* and another */", vec![]),
        (";db.a.find();;", vec!["db.a.find();"]),
        (
            "db.a.find(); db.b.find({s: 'open; db.c.find()\n",
            vec!["db.a.find();", "db.b.find({s: 'open; db.c.find()"],
        ),
        ("db.a.find()\n/* open", vec!["db.a.find()", "/* open"]),
        ("db.a.find({\ndb.b.find()", vec!["db.a.find({\ndb.b.find()"]),
        (
            "db.a.find())\ndb.b.find()",
            vec!["db.a.find())", "db.b.find()"],
        ),
    ] {
        let got: Vec<&str> = statement_ranges(src).into_iter().map(|r| &src[r]).collect();
        assert_eq!(got, want, "{src:?}");
    }
}

#[test]
fn pieces_carry_their_tokens_without_the_semicolon_and_their_lex_error() {
    let src = "db.a.drop();\ndb.b.find({s: 'x";
    let (toks, pieces) = split(src);
    assert_eq!(pieces.len(), 2);
    let first: Vec<&str> = toks[pieces[0].toks.clone()]
        .iter()
        .map(|t| &src[t.range.clone()])
        .collect();
    assert_eq!(
        (first, &pieces[0].error),
        (vec!["db", ".", "a", ".", "drop", "(", ")"], &None)
    );
    let err = pieces[1].error.as_ref().unwrap();
    assert_eq!(
        (err.message, err.at),
        ("unterminated string", src.find('\'').unwrap())
    );
    assert_eq!(&src[pieces[1].range.clone()], "db.b.find({s: 'x");
}
