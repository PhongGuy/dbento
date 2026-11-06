//! `mssql_classification`: what a parsed batch writes, what needs the no-WHERE confirmation,
//! and transaction control found from tokens.
use super::{mssql_changes_settings, mssql_classification};
use crate::session::{Unguarded, Writes};

fn writes(sql: &str) -> Writes {
    mssql_classification(sql).writes
}
fn unguarded(sql: &str) -> Vec<Unguarded> {
    mssql_classification(sql).unguarded
}
fn tx(sql: &str) -> bool {
    mssql_classification(sql).transaction_control
}

#[test]
fn reads_and_session_statements_are_no() {
    for sql in [
        "SELECT 1",
        "SELECT TOP 10 * FROM t WITH (NOLOCK)",
        "WITH c AS (SELECT 1 AS a) SELECT * FROM c",
        "SELECT 1 UNION SELECT 2",
        "SELECT a FROM t FOR JSON PATH",
        "SELECT @@TRANCOUNT",
        "DECLARE @v INT = 1",
        "DECLARE c CURSOR FOR SELECT a FROM t",
        "DECLARE @v INT; SET @v = 1; SELECT @v",
        "SET NOCOUNT ON",
        "SET TRANSACTION ISOLATION LEVEL SNAPSHOT",
        "USE master",
        "PRINT 'hello'",
        "WAITFOR DELAY '00:00:01'",
        "IF @v = 1 BEGIN SELECT 1 END ELSE BEGIN SELECT 2 END",
        "WHILE @v < 10 BEGIN SELECT 1; END",
        "BEGIN SELECT 1; END",
    ] {
        assert_eq!(writes(sql), Writes::No, "{sql}");
    }
}

#[test]
fn everything_else_that_parses_is_yes() {
    for sql in [
        "SELECT * INTO #tmp FROM t",
        "INSERT INTO t (a) OUTPUT inserted.id VALUES (1)",
        "UPDATE t SET a = 1 WHERE id = 2",
        "DELETE FROM t WHERE id = 2",
        "MERGE t USING s ON t.id = s.id WHEN MATCHED THEN DELETE",
        "WITH c AS (SELECT id FROM t) DELETE FROM t WHERE id IN (SELECT id FROM c)",
        "EXEC dbo.p @a = 1",
        "EXEC sp_executesql N'SELECT 1'",
        "EXECUTE ('SELECT 1')",
        "CREATE TABLE #t (id INT)",
        "CREATE VIEW dbo.v AS SELECT a FROM t",
        "ALTER TABLE t ADD c INT NULL",
        "GRANT SELECT ON t TO reader",
        "KILL 55",
        "RAISERROR ('boom', 16, 1)",
        "BEGIN TRAN",
        "IF @v = 1 BEGIN UPDATE t SET a = 1; END",
        "IF @v = 1 SELECT 1 ELSE DELETE FROM t",
        "BEGIN DELETE FROM t; END",
        "SELECT 1; INSERT INTO t VALUES (1)",
    ] {
        assert_eq!(writes(sql), Writes::Yes, "{sql}");
    }
}

#[test]
fn a_batch_the_parser_rejects_is_unknown() {
    for sql in [
        "BEGIN TRY SELECT 1/0 END TRY BEGIN CATCH SELECT ERROR_MESSAGE() END CATCH",
        "DBCC CHECKDB",
        "BACKUP DATABASE d TO DISK = 'x.bak'",
        "BULK INSERT t FROM 'f.csv'",
        "SELECT 1 SELECT 2",
        "CREATE PROCEDURE dbo.p @a INT AS BEGIN SELECT @a END",
        "sp_who",
    ] {
        assert_eq!(writes(sql), Writes::Unknown, "{sql}");
        assert!(unguarded(sql).is_empty(), "{sql}");
    }
}

#[test]
fn updates_and_deletes_without_where_are_unguarded_in_every_form() {
    let upd = Unguarded::UpdateWithoutWhere;
    let del = Unguarded::DeleteWithoutWhere;
    assert_eq!(unguarded("UPDATE t SET a = 1"), [upd]);
    assert_eq!(
        unguarded("UPDATE t SET a = u.a FROM t JOIN u ON t.id = u.id"),
        [upd]
    );
    assert_eq!(unguarded("UPDATE TOP (5) t SET a = 1"), [upd]);
    assert_eq!(unguarded("DELETE FROM t"), [del]);
    assert_eq!(
        unguarded("DELETE x FROM t AS x JOIN u ON x.id = u.id"),
        [del]
    );
    assert_eq!(
        unguarded("WITH c AS (SELECT TOP 5 * FROM t) DELETE FROM c"),
        [del]
    );
    assert_eq!(
        unguarded("WITH c AS (SELECT * FROM t) UPDATE c SET a = 1"),
        [upd]
    );
    assert_eq!(
        unguarded("UPDATE t SET a = 1; DELETE FROM t; UPDATE u SET b = 2"),
        [upd, del, upd]
    );
    for sql in [
        "UPDATE t SET a = 1 WHERE id = 2",
        "UPDATE t SET a = u.a FROM t JOIN u ON t.id = u.id WHERE u.b = 1",
        "DELETE x FROM t AS x JOIN u ON x.id = u.id WHERE u.b = 1",
        "WITH c AS (SELECT id FROM t) DELETE FROM t WHERE id IN (SELECT id FROM c)",
        "MERGE t USING s ON t.id = s.id WHEN MATCHED THEN DELETE",
        "CREATE PROCEDURE dbo.p AS BEGIN UPDATE t SET a = 1; END",
    ] {
        assert!(unguarded(sql).is_empty(), "{sql}");
    }
}

#[test]
fn drop_and_truncate_are_unguarded() {
    for sql in [
        "DROP TABLE IF EXISTS t",
        "DROP PROCEDURE dbo.p",
        "DROP VIEW dbo.v",
        "DROP FUNCTION dbo.f",
        "DROP TRIGGER trg",
        "DROP INDEX ix ON t",
        "DROP SCHEMA s",
        "DROP DATABASE d",
    ] {
        assert_eq!(unguarded(sql), [Unguarded::Drop], "{sql}");
    }
    assert_eq!(unguarded("TRUNCATE TABLE t"), [Unguarded::Truncate]);
}

#[test]
fn the_check_looks_inside_if_while_and_begin_end_bodies() {
    assert_eq!(
        unguarded("IF @v = 1 BEGIN UPDATE t SET a = 1; END"),
        [Unguarded::UpdateWithoutWhere]
    );
    assert_eq!(
        unguarded("IF @v = 1 SELECT 1 ELSE DELETE FROM t"),
        [Unguarded::DeleteWithoutWhere]
    );
    assert_eq!(
        unguarded("WHILE @v < 10 BEGIN DELETE FROM t; END"),
        [Unguarded::DeleteWithoutWhere]
    );
    assert_eq!(
        unguarded("IF EXISTS (SELECT 1 FROM sys.tables WHERE name = 't') DROP TABLE t"),
        [Unguarded::Drop]
    );
    assert_eq!(
        unguarded("BEGIN DELETE FROM t; END"),
        [Unguarded::DeleteWithoutWhere]
    );
}

#[test]
fn transaction_control_is_found_by_tokens_whether_or_not_the_batch_parses() {
    for sql in [
        "BEGIN TRAN",
        "BEGIN TRANSACTION",
        "begin transaction t1",
        "BEGIN DISTRIBUTED TRANSACTION",
        "COMMIT",
        "COMMIT TRANSACTION t1",
        "ROLLBACK",
        "ROLLBACK TRAN sp1",
        "SAVE TRAN sp1",
        "SAVE TRANSACTION sp1",
        "SET IMPLICIT_TRANSACTIONS ON",
        "SELECT 1; COMMIT",
        "IF @@TRANCOUNT > 0 ROLLBACK",
        "BEGIN TRY SELECT 1 END TRY BEGIN CATCH ROLLBACK END CATCH",
        "-- c\nCOMMIT",
        "/* a /* b */ c */ COMMIT",
    ] {
        assert!(tx(sql), "{sql}");
    }
    for sql in [
        "SELECT 1",
        "BEGIN SELECT 1; END",
        "IF 1 = 1 BEGIN SELECT 1 END",
        "BEGIN TRY SELECT 1 END TRY BEGIN CATCH SELECT 2 END CATCH",
        "SELECT 'COMMIT'",
        "SELECT [commit] FROM t",
        "SELECT \"rollback\" FROM t",
        "SELECT 1 -- ROLLBACK",
        "/* COMMIT /* nested */ ROLLBACK */ SELECT 1",
        "SET XACT_ABORT ON",
        "SET TRANSACTION ISOLATION LEVEL SNAPSHOT",
        "SELECT @commit",
        "DECLARE @savepoint INT",
    ] {
        assert!(!tx(sql), "{sql}");
    }
}

#[test]
fn an_unparsed_batch_still_flags_updates_and_deletes_without_where() {
    let upd = Unguarded::UpdateWithoutWhere;
    let del = Unguarded::DeleteWithoutWhere;
    for (sql, want) in [
        (
            "BEGIN TRY DELETE FROM t END TRY BEGIN CATCH SELECT 1 END CATCH",
            vec![del],
        ),
        (
            "SELECT 1 UPDATE t SET a = (SELECT MAX(b) FROM u WHERE u.id = 1)",
            vec![upd],
        ),
        (
            "SELECT 1 DELETE FROM t SELECT 2 FROM u WHERE x = 1",
            vec![del],
        ),
        ("DELETE TOP (10) FROM t", vec![del]),
        ("DELETE FROM t FROM t JOIN u ON t.id = u.id", vec![del]),
        ("SELECT 1 UPDATE t SET a = 1; DELETE FROM u", vec![upd, del]),
    ] {
        assert_eq!(writes(sql), Writes::Unknown, "{sql}");
        assert_eq!(unguarded(sql), want, "{sql}");
    }
    for sql in [
        "BEGIN TRY UPDATE t SET a = 1 WHERE id = 1 END TRY BEGIN CATCH SELECT 1 END CATCH",
        "BEGIN TRY UPDATE t SET a = CASE WHEN b = 1 THEN 1 ELSE 2 END WHERE id = 1 END TRY BEGIN CATCH SELECT 1 END CATCH",
        "BEGIN TRY DELETE t FROM t JOIN u ON t.id = CASE WHEN u.b = 1 THEN u.a ELSE u.c END WHERE t.x = 1 END TRY BEGIN CATCH SELECT 1 END CATCH",
        "DELETE t WHERE id = 1",
        "SELECT 'DELETE FROM t' SELECT 2",
        "SELECT 1 /* DELETE FROM t */ SELECT 2",
        "SELECT [delete] FROM t SELECT 2",
        "SELECT 1 GRANT UPDATE, DELETE ON t TO r",
        "SELECT 1 UPDATE STATISTICS t",
        "SELECT 1 MERGE t USING s ON t.id = s.id WHEN MATCHED THEN DELETE",
        "SELECT 1 MERGE t USING s ON t.id = s.id WHEN MATCHED THEN UPDATE SET a = 1",
        "ALTER TABLE t ADD CONSTRAINT f FOREIGN KEY (a) REFERENCES u (id) ON DELETE CASCADE SELECT 1",
    ] {
        assert_eq!(writes(sql), Writes::Unknown, "{sql}");
        assert!(unguarded(sql).is_empty(), "{sql}");
    }
}

#[test]
fn a_routine_definition_batch_is_not_scanned_for_its_body() {
    for sql in [
        "CREATE PROCEDURE dbo.p @a INT AS BEGIN DELETE FROM t END",
        "CREATE OR ALTER PROCEDURE dbo.p AS UPDATE t SET a = 1",
        "ALTER PROCEDURE dbo.p @a INT AS DELETE FROM t",
        "create proc dbo.p as drop table t",
        "CREATE TRIGGER trg ON t AFTER UPDATE, DELETE AS BEGIN DELETE FROM u END",
        "CREATE FUNCTION dbo.f () RETURNS INT AS BEGIN RETURN 1 END",
    ] {
        assert_eq!(writes(sql), Writes::Unknown, "{sql}");
        assert!(unguarded(sql).is_empty(), "{sql}");
    }
}

#[test]
fn an_unparsed_batch_still_flags_drop_and_truncate() {
    assert_eq!(
        unguarded("BEGIN TRY DROP TABLE t END TRY BEGIN CATCH SELECT 1 END CATCH"),
        [Unguarded::Drop]
    );
    assert_eq!(
        unguarded("SELECT 1 DROP PROCEDURE dbo.p"),
        [Unguarded::Drop]
    );
    assert_eq!(
        unguarded("SELECT 1 TRUNCATE TABLE t"),
        [Unguarded::Truncate]
    );
    assert_eq!(
        unguarded("SELECT 1 DELETE FROM t DROP TABLE u"),
        [Unguarded::DeleteWithoutWhere, Unguarded::Drop]
    );
    assert!(unguarded("SELECT 1 ALTER TABLE t DROP COLUMN c").is_empty());
    assert!(unguarded("SELECT 1 ALTER TABLE t DROP CONSTRAINT ck").is_empty());
    assert!(unguarded("SELECT 'DROP TABLE t' SELECT 2").is_empty());
}

#[test]
fn only_one_parsed_query_is_a_single_query_batch() {
    for (batch, single) in [
        ("SELECT 1 AS a", true),
        ("WITH c AS (SELECT 1 AS x) SELECT x FROM c", true),
        ("SELECT 1 AS a; SELECT 2 AS b", false),
        ("DELETE FROM t WHERE a = 1", false),
        (
            "BEGIN TRY SELECT 1 END TRY BEGIN CATCH SELECT 2 END CATCH",
            false,
        ),
        ("", false),
    ] {
        assert_eq!(super::mssql_single_query(batch), single, "{batch}");
    }
}

#[test]
fn showplan_and_statistics_xml_count_as_transaction_control() {
    for sql in [
        "SET SHOWPLAN_XML ON",
        "set showplan_text on",
        "SET SHOWPLAN_ALL ON",
        "SET STATISTICS XML ON",
        "SELECT 1\nSET SHOWPLAN_XML OFF",
    ] {
        assert!(tx(sql), "{sql}");
    }
    for sql in [
        "SET STATISTICS IO ON",
        "SET NOCOUNT ON",
        "SELECT 'SET SHOWPLAN_XML ON'",
    ] {
        assert!(!tx(sql), "{sql}");
    }
}

// A SET list that names a fence-breaking option counts, whatever else it lists.
#[test]
fn set_lists_and_ansi_defaults_count_as_transaction_control() {
    for sql in [
        "SET ANSI_DEFAULTS OFF",
        "set ansi_defaults on",
        "SET NOCOUNT, IMPLICIT_TRANSACTIONS OFF",
        "SET NOCOUNT,/* c */ANSI_NULLS, IMPLICIT_TRANSACTIONS OFF",
        "SET NOCOUNT, ANSI_DEFAULTS OFF",
        "SET STATISTICS IO, XML ON",
        "SET STATISTICS TIME, PROFILE, XML ON",
        "SELECT 1\nSET NOCOUNT, IMPLICIT_TRANSACTIONS OFF",
    ] {
        assert!(tx(sql), "{sql}");
    }
    for sql in [
        "SET STATISTICS IO, TIME ON",
        "SET NOCOUNT, ANSI_NULLS ON",
        "SELECT ansi_defaults FROM sys.dm_exec_sessions",
        "UPDATE t SET a = 1, implicit_transactions = 2 WHERE id = 1",
        "SELECT 'SET ANSI_DEFAULTS OFF'",
    ] {
        assert!(!tx(sql), "{sql}");
    }
}

// Each keyword below runs on the server, so the classifier must see it too.
#[test]
fn transaction_control_is_found_where_the_server_reads_a_token_break() {
    for sql in [
        "SET\u{A0}IMPLICIT_TRANSACTIONS OFF",
        "SET\u{3000}IMPLICIT_TRANSACTIONS OFF",
        "SET\u{200B}ANSI_DEFAULTS OFF",
        "SET\u{1}IMPLICIT_TRANSACTIONS OFF",
        "SET\u{0}IMPLICIT_TRANSACTIONS OFF",
        "SELECT 1\u{85}COMMIT",
        "--c\rSET IMPLICIT_TRANSACTIONS OFF",
        "SELECT 1SET IMPLICIT_TRANSACTIONS OFF",
        "SELECT 1COMMIT",
        "SELECT 1eCOMMIT",
        "SELECT 1E5COMMIT",
        "SELECT 1e-COMMIT",
        "SELECT 1.5COMMIT",
        "SELECT .5e5COMMIT",
        "SELECT 0xFFSAVE TRAN s",
        "SELECT 0XSAVE TRAN s",
        "SELECT $1COMMIT",
        "SELECT $.5COMMIT",
        "SELECT \u{A3}COMMIT",
        "SELECT \u{20AC}1COMMIT",
        "SELECT 1\u{A0}COMMIT\u{A0}COMMIT\u{A0}BEGIN\u{A0}TRAN",
    ] {
        assert!(tx(sql), "{sql:?}");
    }
    for sql in [
        "SELECT 0xCOMMIT",
        "SELECT 1\u{A0}AS one",
        "SELECT 'a\u{A0}COMMIT'",
        "--c COMMIT\nSELECT 1",
        "SELECT $action",
        "SELECT caf\u{E9}COMMIT",
        "SELECT 1 AS [a\u{A0}COMMIT]",
    ] {
        assert!(!tx(sql), "{sql:?}");
    }
}

// Layer 3 refuses a hidden commit in a batch the parser reads; only a parse failure lets one by.
#[test]
fn a_hidden_commit_is_a_write_unless_the_batch_fails_to_parse() {
    for sql in [
        "EXEC('COMMIT COMMIT DROP DATABASE victim')",
        "EXEC('COMMIT COMMIT KILL 999')",
    ] {
        assert_eq!(writes(sql), Writes::Yes, "{sql}");
    }
    for sql in [
        "SELECT 1 AS one EXEC('COMMIT COMMIT DROP DATABASE victim')",
        "SELECT 1 AS one EXEC('COMMIT COMMIT KILL 999')",
        "EXEC('COMMIT COMMIT') SHUTDOWN WITH NOWAIT",
    ] {
        assert_eq!(writes(sql), Writes::Unknown, "{sql}");
    }
}

// The MCP best-effort description states these forms get through; if one starts being refused,
// or stops parsing as it does, that text is no longer true.
#[test]
fn the_forms_the_best_effort_description_quotes_are_not_refused() {
    for sql in [
        "SELECT 1 AS one EXEC('COMMIT COMMIT DROP DATABASE victim')",
        "SELECT 1 AS one EXEC('COMMIT COMMIT KILL 999')",
        "EXEC('COMMIT COMMIT') SHUTDOWN WITH NOWAIT",
    ] {
        let c = mssql_classification(sql);
        assert_eq!(c.writes, Writes::Unknown, "{sql}");
        assert!(!c.transaction_control, "{sql}");
    }
    assert_eq!(
        mssql_classification("EXEC('COMMIT COMMIT')").writes,
        Writes::Yes
    );
}

#[test]
fn a_use_anywhere_in_the_batch_changes_settings_but_text_and_set_do_not() {
    for sql in [
        "USE master",
        "  -- c\n use [db]",
        "/* x */USE db",
        "/* a /* nested */ b */ USE db",
        "SELECT 1; USE master",
    ] {
        assert!(mssql_changes_settings(sql), "{sql}");
    }
    for sql in [
        "SELECT 1",
        "EXEC('USE master')",
        "UPDATE t SET a = 1",
        "SET NOCOUNT ON",
        "SELECT [USE] FROM t -- USE master",
        "SELECT a FROM t OPTION (USE HINT('DISABLE_OPTIMIZED_NESTED_LOOP'))",
    ] {
        assert!(!mssql_changes_settings(sql), "{sql}");
    }
}
