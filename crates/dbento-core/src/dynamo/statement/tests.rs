use super::{Kind, PartiqlStatement, Target, parse_script};

fn one(text: &str) -> PartiqlStatement {
    let mut all = parse_script(text);
    assert_eq!(all.len(), 1, "{text:?}");
    all.remove(0).unwrap()
}

fn target(table: &str, index: Option<&str>) -> Option<Target> {
    Some(Target {
        table: table.to_owned(),
        index: index.map(str::to_owned),
    })
}

#[test]
fn the_first_keyword_names_the_kind_in_any_case() {
    for (text, kind) in [
        ("SELECT * FROM t", Kind::Select),
        ("select * from t", Kind::Select),
        ("-- c\n Insert INTO t VALUE {'a': 1}", Kind::Insert),
        ("update t SET a = 1 WHERE pk = 'x'", Kind::Update),
        ("DELETE FROM t WHERE pk = 'x'", Kind::Delete),
        ("EXISTS(SELECT * FROM t WHERE pk = 'x')", Kind::Other),
        ("SELEC * FROM t", Kind::Other),
        ("(SELECT * FROM t)", Kind::Other),
    ] {
        assert_eq!(one(text).kind, kind, "{text}");
    }
}

#[test]
fn the_target_is_the_table_and_any_index_after_from_into_or_update() {
    assert_eq!(
        one("SELECT * FROM \"orders\"").target,
        target("orders", None)
    );
    assert_eq!(
        one("SELECT * FROM orders.by_status WHERE status = 'open'").target,
        target("orders", Some("by_status"))
    );
    assert_eq!(
        one("SELECT * FROM \"orders\".\"by status\"").target,
        target("orders", Some("by status"))
    );
    assert_eq!(one("SELECT * FROM \"a\"\"b\"").target, target("a\"b", None));
    assert_eq!(
        one("INSERT INTO h VALUE {'id': 'a'}").target,
        target("h", None)
    );
    assert_eq!(
        one("UPDATE \"h\" SET v = '2' WHERE id = 'a'").target,
        target("h", None)
    );
    assert_eq!(
        one("DELETE FROM h WHERE id = 'a'").target,
        target("h", None)
    );
    assert_eq!(
        one("SELECT * FROM t WHERE a IN (SELECT b FROM u)").target,
        target("t", None)
    );
    assert_eq!(one("EXISTS(SELECT * FROM t)").target, None);
    assert_eq!(one("SELECT 1").target, None);
    assert_eq!(one("INSERT t VALUE {}").target, None);
}

#[test]
fn a_projection_is_kept_only_when_every_item_is_a_plain_name() {
    let p = |text: &str| one(text).projection;
    let names = |n: &[&str]| Some(n.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>());
    assert_eq!(p("SELECT status FROM orders"), names(&["status"]));
    assert_eq!(
        p("SELECT pk, \"sk\", \"a\"\"b\" FROM orders"),
        names(&["pk", "sk", "a\"b"])
    );
    for text in [
        "SELECT * FROM orders",
        "SELECT a.b FROM orders",
        "SELECT size(a) FROM orders",
        "SELECT a AS x FROM orders",
        "SELECT a, * FROM orders",
        "SELECT 'a' FROM orders",
        "SELECT FROM orders",
        "SELECT a",
        "UPDATE t SET a = 1",
    ] {
        assert_eq!(p(text), None, "{text}");
    }
}

#[test]
fn a_syntax_error_passes_through_with_its_range() {
    let all = parse_script("SELECT 1; SELECT 'x");
    assert_eq!(all[0].as_ref().map(|s| s.range.clone()), Ok(0..8));
    let e = all[1].as_ref().unwrap_err();
    assert_eq!((e.range.clone(), e.offset), (10..19, 7));
}
