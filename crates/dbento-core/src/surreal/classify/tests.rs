use super::surreal_classification;
use crate::session::Unguarded::{self, DeleteWithoutWhere, Drop, UpdateWithoutWhere};
use crate::session::Writes;

fn writes(src: &str) -> Writes {
    surreal_classification(src).writes
}

fn unguarded(src: &str) -> Vec<Unguarded> {
    surreal_classification(src).unguarded
}

#[test]
fn reads_and_session_statements_do_not_write() {
    for src in [
        "SELECT * FROM t",
        "SELECT a, b FROM t WHERE a = 1 ORDER BY b LIMIT 3",
        "RETURN 1",
        "INFO FOR DB",
        "INFO FOR TABLE t STRUCTURE",
        "SHOW CHANGES FOR TABLE t SINCE 1 LIMIT 1",
        "SLEEP 1s",
        "THROW 'x'",
        "LET $a = 1; RETURN $a",
        "USE NS a DB b",
        "RETURN string::len('a')",
        "RETURN STRING::LEN('a')",
        "RETURN string::is::email('a@b.c')",
        "RETURN type::table('t')",
        "RETURN array::len([1])",
        "RETURN math::mean([1, 2])",
        "RETURN time::now()",
        "RETURN crypto::argon2::generate('p')",
        "RETURN vector::add([1], [2])",
        "RETURN count([1])",
        "RETURN sleep(1s)",
    ] {
        assert_eq!(writes(src), Writes::No, "{src}");
    }
}

// F6 "Names that collide with verbs".
#[test]
fn a_verb_used_as_a_name_is_not_a_write() {
    for src in [
        "SELECT a.update FROM seed",
        "RETURN $d.delete",
        "RETURN $create",
        "RETURN seed:1.delete",
        "SELECT `update`, `create` FROM seed",
        "SELECT * FROM `update`",
        "SELECT a FROM t ORDER BY `update`",
        "RETURN { delete: 1 }",
        "RETURN { a: 1, create: 2 }",
        "SELECT x AS delete FROM t",
        "SELECT x AS update, y AS remove FROM t",
        "SELECT a FROM t ORDER BY delete",
        "SELECT a FROM t GROUP BY update",
        "RETURN array::remove([1], 0)",
        "RETURN array::insert([1], 2)",
        "RETURN array::update([1])",
        "RETURN string::replace('a', 'b', 'c')",
        // These words are names everywhere but a statement's start.
        "SELECT kill FROM t",
        "SELECT live, access FROM t",
        "RETURN live",
        "RETURN kill",
        "SELECT * FROM access",
        "SELECT a FROM t WHERE live = 1",
        "RETURN { kill: 1 }",
    ] {
        assert_eq!(writes(src), Writes::No, "{src}");
    }
}

// The verbs in F6's first row, each at a statement's start.
#[test]
fn every_write_verb_is_a_write_in_any_case() {
    for src in [
        "CREATE t SET a = 1",
        "create t set a = 1",
        "UPDATE t SET a = 1 WHERE a = 2",
        "UPSERT t:1 SET a = 1",
        "DELETE t WHERE a = 1",
        "INSERT INTO t { a: 1 }",
        "RELATE a:1->r->b:2",
        "DEFINE TABLE t",
        "REMOVE FIELD a ON t",
        "ALTER TABLE t COMMENT 'x'",
        "ALTER SYSTEM QUERY_TIMEOUT 77s",
        "REBUILD INDEX ix ON t",
        "LIVE SELECT * FROM t",
        "KILL 'id'",
        "ACCESS ka ON DATABASE GRANT FOR USER dv",
        "ACCESS ka ON DATABASE REVOKE ALL",
        "{ LIVE SELECT * FROM t }",
        "RETURN 1; KILL 'id'",
    ] {
        assert_eq!(writes(src), Writes::Yes, "{src}");
    }
}

// F6 "Writes with no parentheses": each of these wrote on the server.
#[test]
fn a_write_in_an_expression_position_is_a_write() {
    for src in [
        "SELECT delete vt FROM seed",
        "SELECT update vt set x=9 FROM seed",
        "RETURN delete vt",
        "RETURN create newx:1",
        "LET $a = delete vt",
        "[delete vt]",
        "RETURN {a: create newx:1}",
        "RETURN !delete vt",
        "SELECT * FROM create newx:1",
        "SELECT * FROM delete vt RETURN BEFORE",
        "IF delete vt { 1 }",
        "FOR $i IN delete vt RETURN BEFORE { }",
        "RETURN array::len(update vt set x=9)",
        "RETURN define table zt",
        "RETURN remove table vt",
        "SELECT * FROM vt WHERE (delete vt:1 RETURN BEFORE)",
        "SELECT * FROM vt LIMIT (CREATE newx:1).len()",
        "RETURN 1..delete vt",
        "RETURN (UPDATE g SET x=9)",
        "LET $a = (DELETE g)",
        "IF true { DELETE g }",
    ] {
        assert_eq!(writes(src), Writes::Yes, "{src}");
    }
}

// F1's escapes the text can show, with the quoted and spaced spellings.
#[test]
fn the_visible_escapes_are_writes() {
    for src in [
        "RETURN http::post('http://h')",
        "RETURN http::get('http://h')",
        "RETURN HTTP::GET('http://h')",
        "RETURN `http`::post('http://h')",
        "RETURN ⟨http⟩::get('http://h')",
        "RETURN http :: post('http://h')",
        "RETURN `http` :: post('http://h')",
        "RETURN file::put('bk:/a', 'x')",
        "RETURN sequence::nextval('sq')",
        "RETURN `sequence`::nextval('sq')",
        "RETURN ⟨sequence⟩::nextval('sq')",
        "RETURN sequence :: nextval('sq')",
        "RETURN SEQUENCE::NEXTVAL('sq')",
        "RETURN f'bk:/a'.put('x')",
        "RETURN f\"bk:/a\".put('x')",
        "RETURN f'bk:/a'.delete()",
        "RETURN f'bk:/a'.copy('b')",
        "RETURN f'bk:/a'.rename('b')",
        "RETURN f'bk:/a'.put_if_not_exists('x')",
        "RETURN f'bk:/a'.copy_if_not_exists('b')",
        "RETURN f'bk:/a'.rename_if_not_exists('b')",
        "RETURN $f.put ('x')",
    ] {
        assert_eq!(writes(src), Writes::Yes, "{src}");
    }
    // A file read and a field that shares a method's name stay reads.
    for src in [
        "RETURN f'bk:/a'.get()",
        "RETURN $a.put",
        "RETURN $a.delete",
        "RETURN a.putty()",
    ] {
        assert_eq!(writes(src), Writes::No, "{src}");
    }
}

#[test]
fn what_the_text_cannot_judge_is_unknown() {
    for src in [
        "RETURN fn::w()",
        "RETURN fn :: w()",
        "RETURN `fn`::w()",
        "RETURN fn::create()",
        "RETURN mod::x::y()",
        "RETURN ml::m<1.0.0>([1])",
        "RETURN api::invoke('/wr')",
        "RETURN `api`::invoke('/wr')",
        "RETURN api :: invoke('/wr')",
        "RETURN eval::surql('DELETE t')",
        "RETURN sequence::other('sq')",
        "RETURN brand_new::thing()",
        "RETURN function() { return 1; }",
        "SELECT * FROM t WHERE x = function() { return 1; }",
        "RETURN 'open",
        "/* open",
        "RETURN `open",
    ] {
        assert_eq!(writes(src), Writes::Unknown, "{src}");
    }
    // A visible write outranks an unknown call.
    assert_eq!(writes("CREATE t SET a = fn::x()"), Writes::Yes);
    assert_eq!(writes("DEFINE FUNCTION fn::x() { RETURN 1; }"), Writes::Yes);
}

// F7, row by row: what stays unguarded and what does not.
#[test]
fn an_update_or_delete_of_a_whole_table_is_unguarded() {
    for src in [
        "UPDATE g SET x=9",
        "UPDATE g",
        "UPDATE g CONTENT { x: 9 }",
        "UPDATE g MERGE { x: 9 }",
        "UPDATE g REPLACE { x: 9 }",
        "UPDATE g PATCH []",
        "UPDATE ONLY g SET x=9",
        "UPDATE $x SET x=9",
        "UPDATE type::table('g') SET x=9",
        "UPDATE (SELECT * FROM g WHERE x=1) SET x=9",
        "UPDATE g, h SET x=9",
        "UPDATE g:1, h SET x=9",
        "UPDATE [g:1, h] SET x=9",
        "update g set x=9",
        "UPDATE g SET x = (SELECT VALUE a FROM h WHERE b = 1)",
        "RETURN (UPDATE g SET x=9)",
        "RETURN update g set x=9",
    ] {
        assert_eq!(unguarded(src), [UpdateWithoutWhere], "{src}");
    }
    for src in [
        "DELETE g",
        "DELETE FROM g",
        "DELETE ONLY g",
        "DELETE $x",
        "DELETE type::table('g')",
        "DELETE (SELECT * FROM g WHERE x=1)",
        "DELETE edge",
        "DELETE g EXPLAIN FULL",
        "EXPLAIN DELETE g",
        "LET $a = (DELETE g)",
        "IF true { DELETE g }",
        "RETURN delete g",
    ] {
        assert_eq!(unguarded(src), [DeleteWithoutWhere], "{src}");
    }
}

#[test]
fn a_where_or_a_named_record_guards_a_write() {
    for src in [
        "UPDATE g SET x=9 WHERE true",
        "UPDATE g SET x=9 WHERE x>1",
        "update g set x=9 where x>1",
        "UPDATE g:1 SET x=9",
        "UPDATE ONLY g:1 SET x=9",
        "UPDATE type::record('g', 1) SET x=9",
        "UPDATE g:1..3 SET x=9",
        "UPDATE g:1..=2 SET x=9",
        "UPDATE [g:1, g:2] SET x=9",
        "UPDATE g:1, g:2 SET x=9",
        "UPDATE g:99 SET x=9",
        "UPDATE g:1->edge SET w=9",
        "UPDATE r'g:1' SET x=9",
        "DELETE g:1",
        "DELETE ONLY g:1",
        "DELETE FROM ONLY g:1",
        "DELETE g WHERE x>1",
        "DELETE g:1..3",
        "DELETE [g:1, g:2]",
        "DELETE g:1, g:2",
        "DELETE g:1->edge",
        "DELETE g:1->edge WHERE w=1",
        "DELETE `g h`:1",
        "DELETE ⟨g h⟩:⟨a;b⟩",
        "SELECT * FROM vt WHERE (delete vt:1 RETURN BEFORE)",
        // UPSERT never reaches every row, and the other verbs have no such form.
        "UPSERT g SET x=9",
        "UPSERT g CONTENT { x: 9 }",
        "CREATE g SET x=9",
        "INSERT INTO g { x: 1 }",
    ] {
        assert_eq!(unguarded(src), [], "{src}");
    }
}

// A word that reads as a verb but is a clause: no false "without WHERE".
#[test]
fn a_clause_word_is_not_an_unguarded_statement() {
    for src in [
        "INSERT INTO g {id:1, x:5} ON DUPLICATE KEY UPDATE x=9",
        "INSERT INTO g [{id:1,x:5},{id:9,x:5}] ON DUPLICATE KEY UPDATE x=9",
        "INSERT INTO g (a) VALUES (1) on duplicate key update a=2",
        "DEFINE TABLE t PERMISSIONS FOR delete NONE",
        "DEFINE TABLE t PERMISSIONS FOR select, create, update, delete WHERE a = 1",
        "DEFINE FIELD f ON t PERMISSIONS FOR update WHERE a = 1, FOR select FULL",
        "DEFINE FUNCTION fn::x() { DELETE g; UPDATE g SET x=1; }",
        "DEFINE EVENT e ON t WHEN true THEN (DELETE g)",
        "SELECT x AS delete FROM t",
    ] {
        assert_eq!(unguarded(src), [], "{src}");
    }
    assert_eq!(
        unguarded("DEFINE TABLE t PERMISSIONS FOR delete NONE; DELETE g"),
        [DeleteWithoutWhere],
        "a DEFINE ends at its `;`"
    );
    assert_eq!(
        writes("DEFINE TABLE t PERMISSIONS FOR delete NONE"),
        Writes::Yes
    );
    assert_eq!(writes("FOR $i IN [1] { RETURN $i }"), Writes::No);
}

#[test]
fn removing_a_table_database_or_namespace_is_a_drop() {
    for src in [
        "REMOVE TABLE t",
        "REMOVE TABLE IF EXISTS t",
        "remove table t",
        "REMOVE DATABASE d",
        "REMOVE DB d",
        "REMOVE NAMESPACE n",
        "REMOVE NS n",
        "RETURN remove table vt",
    ] {
        assert_eq!(unguarded(src), [Drop], "{src}");
    }
    for src in [
        "REMOVE FIELD a ON t",
        "REMOVE INDEX i ON t",
        "REMOVE USER u ON ROOT",
    ] {
        assert_eq!(unguarded(src), [], "{src}");
    }
}

#[test]
fn a_unit_reports_every_unguarded_write_in_order() {
    assert_eq!(
        unguarded("BEGIN; UPDATE a SET x=1; DELETE b; REMOVE TABLE c; DELETE d:1; COMMIT"),
        [UpdateWithoutWhere, DeleteWithoutWhere, Drop]
    );
}

#[test]
fn a_kind_is_reported_once_however_many_writes_share_it() {
    assert_eq!(
        unguarded("LET $a = 1; DELETE a; DELETE b"),
        [DeleteWithoutWhere]
    );
}

#[test]
fn transaction_control_is_a_top_level_statement_start() {
    for src in [
        "BEGIN",
        "begin; CREATE a:1; commit;",
        "BEGIN TRANSACTION; RETURN 1; COMMIT TRANSACTION",
        "COMMIT",
        "CANCEL",
        "RETURN 1; COMMIT",
    ] {
        assert!(surreal_classification(src).transaction_control, "{src}");
    }
    for src in [
        "{ COMMIT; }",
        "IF true { COMMIT; }",
        "FOR $i IN [1] { COMMIT; }",
        "RETURN (COMMIT)",
        "LET $x = COMMIT",
        "SELECT begin, commit, cancel FROM t",
        "RETURN cancel",
        "SELECT `begin` FROM t",
    ] {
        assert!(!surreal_classification(src).transaction_control, "{src}");
    }
    assert_eq!(writes("BEGIN; RETURN 1; COMMIT"), Writes::No);
}

#[test]
fn random_text_never_panics_the_classifier() {
    const PIECES: [&str; 34] = [
        "'", "`", "⟨", "⟩", "/", "/*", "*/", "--", "\n", ";", "{", "}", "(", ")", "[", "]", ":",
        "::", "..", ".", ",", "$a", " ", "1", "x", "é", "UPDATE", "DELETE", "REMOVE", "TABLE",
        "FOR", "WHERE", "function", "type",
    ];
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = |bound: usize| {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        ((state >> 33) as usize) % bound
    };
    for _ in 0..5000 {
        let src: String = (0..next(30)).map(|_| PIECES[next(PIECES.len())]).collect();
        surreal_classification(&src);
    }
}

// A statement around the verb owns its own clauses, so its WHERE guards nothing.
#[test]
fn a_where_belongs_to_the_nearest_statement() {
    for src in [
        "SELECT delete g FROM t WHERE y",
        "SELECT a, delete g FROM t WHERE y",
        "SELECT delete g AS d FROM t WHERE y",
        "[delete g, delete h WHERE x]",
        "{ DELETE g; DELETE h WHERE x }",
        "DELETE g RETURN a.where",
    ] {
        assert_eq!(unguarded(src), [DeleteWithoutWhere], "{src}");
    }
    for src in [
        "SELECT update g set x=9 FROM seed WHERE y",
        "UPDATE g SET x = delete h WHERE y",
        "UPDATE g SET x = SELECT VALUE a FROM h WHERE b",
        "UPDATE g SET x = a.where",
    ] {
        assert_eq!(unguarded(src), [UpdateWithoutWhere], "{src}");
    }
    assert_eq!(unguarded("SELECT * FROM delete g WHERE y"), []);
}

// The spec's "outside DEFINE" holds wherever the DEFINE stands, and ends with it.
#[test]
fn a_nested_define_adds_no_unguarded_kind() {
    for src in [
        "RETURN define event e on t when true then (delete g)",
        "LET $a = (DEFINE EVENT e ON t THEN (REMOVE TABLE g))",
        "IF true { DEFINE FUNCTION fn::x() { DELETE g; }; }",
        "RETURN define table t permissions for delete none",
    ] {
        assert_eq!(unguarded(src), [], "{src}");
    }
    assert_eq!(
        unguarded("IF true { DEFINE FUNCTION fn::x() { DELETE g; }; DELETE h }"),
        [DeleteWithoutWhere]
    );
    assert_eq!(
        unguarded("(DEFINE TABLE t PERMISSIONS FOR delete NONE) + (DELETE g)"),
        [DeleteWithoutWhere]
    );
}

// With no lower bound and no upper one, a range is the whole table by another name.
#[test]
fn a_range_from_the_first_record_is_not_a_named_record() {
    assert_eq!(unguarded("DELETE g:.."), [DeleteWithoutWhere]);
    assert_eq!(unguarded("UPDATE g:.. SET x=9"), [UpdateWithoutWhere]);
    assert_eq!(unguarded("DELETE g:1.."), []);
}

// A doubtful `/` has two readings, and the one that shows the verb decides.
#[test]
fn both_lexer_readings_count() {
    let src = "SELECT * FROM vt ORDER value / 2; CREATE o1:1; RETURN 4 / 2";
    assert_eq!(writes(src), Writes::Yes);
    let src = "SELECT VALUE /it's/ FROM vt; DELETE x; SELECT VALUE /it's/ FROM vt";
    assert_eq!(writes(src), Writes::Yes);
    assert_eq!(unguarded(src), [DeleteWithoutWhere]);
}

// Here the primary reading takes the middle for a regex, and only the second shows it.
#[test]
fn the_second_reading_alone_can_show_a_verb() {
    let src = "SELECT VALUE / 2; DELETE g; RETURN 1 / 2";
    assert_eq!(writes(src), Writes::Yes);
    assert_eq!(unguarded(src), [DeleteWithoutWhere]);
    let src = "SELECT VALUE / 2; COMMIT; RETURN 1 / 2";
    assert!(surreal_classification(src).transaction_control);
    assert_eq!(writes("RETURN value / fn::x() / 3"), Writes::Unknown);
}

#[test]
fn readings_that_agree_are_not_forced_to_unknown() {
    assert_eq!(writes("RETURN value / 2 / 3"), Writes::No);
}

#[test]
fn a_lex_error_is_unknown_and_hides_no_verb_of_the_second_reading() {
    let src = "SELECT * FROM t ORDER value /'/; DELETE g; RETURN '/'";
    assert_eq!(writes(src), Writes::Yes);
    assert_eq!(unguarded(src), [DeleteWithoutWhere]);
    let src = "SELECT * FROM t ORDER value /'/; RETURN '/'";
    assert_eq!(writes(src), Writes::Unknown);
    // With no second reading the server refuses the whole call, so its verbs never run.
    assert_eq!(writes("DELETE g; RETURN 'open"), Writes::Unknown);
    assert_eq!(unguarded("DELETE g; RETURN 'open"), []);
}
