//! The browse state: its SQL in step with filter and sort, and why a result is not editable.
use crate::support::{edit_target, fixture};
use dbento_app::browse::{BrowseState, EditBlock, edit_block, next_sort};
use dbento_core::{Browse, BrowseError, Sort, browse_sql};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
                 INSERT INTO t VALUES (1, 'b'), (2, 'a'), (3, 'c'); \
                 CREATE TABLE shadow(rowid, _rowid_, oid);";

#[test]
fn sort_cycles_ascending_descending_none_and_restarts_on_another_column() {
    let asc = next_sort(None, "id");
    assert_eq!(asc, Some(Sort::new("id", false)));
    let desc = next_sort(asc.as_ref(), "id");
    assert_eq!(desc, Some(Sort::new("id", true)));
    assert_eq!(next_sort(desc.as_ref(), "id"), None);
    assert_eq!(
        next_sort(desc.as_ref(), "name"),
        Some(Sort::new("name", false))
    );
}

#[test]
fn a_state_keeps_its_sql_in_step_with_filter_and_sort() {
    let fx = fixture(T, "");
    let t = edit_target(&fx, "t");
    let s = BrowseState::new(t.clone(), Browse::default()).unwrap();
    assert_eq!(s.sql(), "SELECT *\nFROM main.t\nORDER BY id ASC");
    let s = s.with_filter("id > 1").unwrap();
    assert_eq!(
        s.sql(),
        "SELECT *\nFROM main.t\nWHERE (\nid > 1\n)\nORDER BY id ASC"
    );
    let s = s.with_next_sort("name").unwrap();
    assert_eq!(
        s.sql(),
        "SELECT *\nFROM main.t\nWHERE (\nid > 1\n)\nORDER BY name ASC, id ASC"
    );
    assert_eq!((s.sort_of("name"), s.sort_of("id")), (Some(false), None));
    let s = s.with_filter("   ").unwrap();
    assert_eq!(s.browse().filter, None);
    assert_eq!(s.sql(), "SELECT *\nFROM main.t\nORDER BY name ASC, id ASC");
    assert_eq!(s.sql(), browse_sql(&t, s.browse()).unwrap());
    assert_eq!(
        s.with_filter("1=1; DELETE FROM t"),
        Err(BrowseError::MultipleStatements)
    );
    assert_eq!(s.with_filter("id > 1) /*"), Err(BrowseError::Unterminated));
}

#[test]
fn edit_blocks_name_the_first_reason_in_order() {
    let fx = fixture(T, "");
    let s = BrowseState::new(edit_target(&fx, "t"), Browse::default()).unwrap();
    let ran = Some(s.sql());
    assert_eq!(edit_block(&s, false, ran), None);
    assert_eq!(
        edit_block(&s, false, Some("SELECT *\nFROM main.t\nORDER BY id ASC\n")),
        None
    );
    assert_eq!(
        edit_block(&s, true, Some("SELECT 1")),
        Some(EditBlock::ReadOnly)
    );
    assert_eq!(
        edit_block(&s, false, Some("SELECT 1")),
        Some(EditBlock::NotBrowseRun)
    );
    assert_eq!(edit_block(&s, false, None), Some(EditBlock::NotBrowseRun));
    let shadow = BrowseState::new(edit_target(&fx, "shadow"), Browse::default()).unwrap();
    assert_eq!(
        edit_block(&shadow, false, Some("SELECT 1")),
        Some(EditBlock::NoIdentity)
    );
    assert_eq!(EditBlock::ReadOnly.reason(), "read-only connection");
    assert_eq!(EditBlock::NoIdentity.reason(), "no row identity");
    assert_eq!(
        EditBlock::NotBrowseRun.reason(),
        "run the table query to edit"
    );
}
