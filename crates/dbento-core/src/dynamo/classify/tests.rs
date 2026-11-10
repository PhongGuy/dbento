use super::partiql_classification;
use crate::dynamo::statement::parse_script;
use crate::session::{Classification, Writes};

fn class(text: &str) -> Classification {
    let mut all = parse_script(text);
    assert_eq!(all.len(), 1, "{text}");
    partiql_classification(&all.remove(0).unwrap())
}

#[test]
fn select_reads_and_insert_update_delete_write_in_any_case() {
    for text in [
        "SELECT * FROM t",
        "select pk from \"t\".\"i\" where pk = 'a'",
        "/* c */ SeLeCt 1",
    ] {
        assert_eq!(class(text).writes, Writes::No, "{text}");
    }
    for text in [
        "INSERT INTO t VALUE {'pk': 'a'}",
        "insert into t value {'pk': 'a'}",
        "UPDATE t SET a = 1 WHERE pk = 'a'",
        "-- c\nupdate t SET a = 1 WHERE pk = 'a'",
        "DELETE FROM t WHERE pk = 'a'",
        "delete from t where pk = 'a'",
    ] {
        assert_eq!(class(text).writes, Writes::Yes, "{text}");
    }
}

#[test]
fn anything_else_is_unknown() {
    for text in [
        "EXISTS(SELECT * FROM t WHERE pk = 'a')",
        "exists (select 1)",
        "SELEC * FROM t",
        "(SELECT * FROM t)",
        "'SELECT'",
        "\"SELECT\" * FROM t",
        "BEGIN",
        "CREATE TABLE t",
    ] {
        assert_eq!(class(text).writes, Writes::Unknown, "{text}");
    }
}

#[test]
fn nothing_is_unguarded_transaction_control_or_categorised() {
    for text in ["UPDATE t SET a = 1", "DELETE FROM t", "SELECT * FROM t"] {
        let c = class(text);
        assert!(
            c.unguarded.is_empty() && !c.transaction_control && c.categories.is_empty(),
            "{text}: {c:?}"
        );
    }
}
