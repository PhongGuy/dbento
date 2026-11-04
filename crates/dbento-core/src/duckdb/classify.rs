//! Write detection for DuckDB: the Postgres parser first, DuckDB's own keywords after.

use pg_query::protobuf::Token;

use crate::classify::{leading_keyword, pg_classification};
use crate::session::{Classification, Unguarded, Writes};

/// pg_query when it parses the text; else the leading-keyword rules. Never a refusal.
pub(crate) fn duckdb_classification(sql: &str) -> Classification {
    let keyword = leading_keyword(sql);
    let is = |words: &[&str]| words.iter().any(|w| w.eq_ignore_ascii_case(keyword));
    // pg_classification answers Unknown only when the parser fails; DuckDB syntax lands there.
    let mut class = pg_classification(sql).classification;
    if class.writes != Writes::Unknown {
        class.transaction_control |= is(&["BEGIN", "COMMIT", "END", "ROLLBACK", "ABORT"]);
        return class;
    }
    // The fallback reads lexer tokens, so a keyword inside a string or comment never counts.
    let Ok(scanned) = pg_query::scan(sql) else {
        return class;
    };
    let has = |wanted: &[Token]| {
        scanned
            .tokens
            .iter()
            .any(|t| Token::try_from(t.token).is_ok_and(|k| wanted.contains(&k)))
    };
    class.writes = if is(&[
        "SELECT",
        "FROM",
        "VALUES",
        "TABLE",
        "SHOW",
        "DESCRIBE",
        "SUMMARIZE",
        "PIVOT",
        "UNPIVOT",
    ]) || (is(&["EXPLAIN"]) && !has(&[Token::Analyze, Token::Analyse]))
    {
        Writes::No
    } else if is(&["WITH"]) {
        let dml = [
            Token::Insert,
            Token::Update,
            Token::DeleteP,
            Token::Merge,
            Token::Copy,
        ];
        if has(&dml) { Writes::Yes } else { Writes::No }
    } else if is(&[
        "INSERT",
        "UPDATE",
        "DELETE",
        "MERGE",
        "CREATE",
        "ALTER",
        "DROP",
        "TRUNCATE",
        "COPY",
        "EXPORT",
        "IMPORT",
        "ATTACH",
        "DETACH",
        "INSTALL",
        "LOAD",
        "SET",
        "RESET",
        "CHECKPOINT",
        "VACUUM",
        "CALL",
    ]) {
        Writes::Yes
    } else {
        Writes::Unknown
    };
    if is(&["DROP"]) {
        class.unguarded.push(Unguarded::Drop);
    } else if is(&["TRUNCATE"]) {
        class.unguarded.push(Unguarded::Truncate);
    } else if let Some(found) = dml_without_where(&scanned.tokens, is(&["WITH"])) {
        class.unguarded.push(found);
    }
    class
}

/// The main UPDATE or DELETE with no WHERE after it at paren depth 0: the first code token,
/// or under a leading WITH the first such token outside the CTEs' parentheses.
fn dml_without_where(tokens: &[pg_query::protobuf::ScanToken], with: bool) -> Option<Unguarded> {
    let mut depth = 0usize;
    let mut code = tokens.iter().filter_map(|t| {
        let token = Token::try_from(t.token).ok()?;
        let at = depth;
        match token {
            Token::SqlComment | Token::CComment => return None,
            Token::Ascii40 => depth += 1,
            Token::Ascii41 => depth = depth.saturating_sub(1),
            _ => {}
        }
        Some((token, at))
    });
    let verb = if with {
        code.find(|&(t, d)| d == 0 && matches!(t, Token::Update | Token::DeleteP))?
    } else {
        code.next()?
    };
    let found = match verb.0 {
        Token::Update => Unguarded::UpdateWithoutWhere,
        Token::DeleteP => Unguarded::DeleteWithoutWhere,
        _ => return None,
    };
    (!code.any(|(t, d)| d == 0 && t == Token::Where)).then_some(found)
}

/// Whether the text has a RETURNING clause, read from lexer tokens so strings and comments
/// never count; such a statement returns rows whatever its columns are named.
pub(crate) fn duckdb_has_returning(sql: &str) -> bool {
    pg_query::scan(sql).is_ok_and(|s| {
        s.tokens
            .iter()
            .any(|t| Token::try_from(t.token) == Ok(Token::Returning))
    })
}

/// SET, RESET or USE as the leading keyword.
pub(crate) fn duckdb_sets_session(sql: &str) -> bool {
    let keyword = leading_keyword(sql);
    ["SET", "RESET", "USE"]
        .iter()
        .any(|w| w.eq_ignore_ascii_case(keyword))
}

#[cfg(test)]
mod tests {
    use super::{duckdb_classification, duckdb_sets_session};
    use crate::session::{Unguarded, Writes};

    fn writes(sql: &str) -> Writes {
        duckdb_classification(sql).writes
    }

    #[test]
    fn reads_are_no_through_either_path() {
        for sql in [
            "SELECT 1",
            "WITH a AS (SELECT 1) SELECT * FROM a",
            "VALUES (1)",
            "TABLE t",
            "SHOW TABLES",
            "EXPLAIN SELECT 1",
            "FROM t",
            "FROM t SELECT a",
            "FROM t WHERE a > 1",
            "SELECT * EXCLUDE (a) FROM t",
            "DESCRIBE t",
            "SUMMARIZE t",
            "PIVOT t ON a USING sum(b)",
            "UNPIVOT t ON a, b INTO NAME k VALUE v",
            "EXPLAIN FROM t",
            "WITH a AS (SELECT 'INSERT') FROM a",
            "-- c\nselect 1",
            "/* insert */ from t",
        ] {
            assert_eq!(writes(sql), Writes::No, "{sql}");
        }
    }

    #[test]
    fn writes_are_yes_through_either_path() {
        for sql in [
            "INSERT INTO t VALUES (1)",
            "UPDATE t SET a = 1 WHERE a = 2",
            "DELETE FROM t WHERE a = 1",
            "CREATE TABLE u(a INTEGER)",
            "COPY t TO 'x.csv'",
            "SET threads = 2",
            "CHECKPOINT",
            "VACUUM",
            "INSERT INTO t BY NAME SELECT 1 AS a",
            "INSERT OR REPLACE INTO t VALUES (1)",
            "ATTACH 'x.duckdb' AS x",
            "DETACH x",
            "INSTALL httpfs",
            "LOAD httpfs",
            "EXPORT DATABASE 'dir'",
            "SET VARIABLE x = 1",
            "CALL checkpoint()",
            "CREATE MACRO m(a) AS a + 1",
            "WITH a AS (SELECT 1) INSERT INTO t FROM a",
        ] {
            assert_eq!(writes(sql), Writes::Yes, "{sql}");
        }
    }

    #[test]
    fn what_neither_path_knows_is_unknown_and_never_refused() {
        for sql in [
            "PRAGMA table_info('t')",
            "FORCE CHECKPOINT",
            "EXPLAIN ANALYZE FROM t",
            "frobnicate",
        ] {
            let c = duckdb_classification(sql);
            assert_eq!(c.writes, Writes::Unknown, "{sql}");
            assert!(c.unguarded.is_empty() && !c.transaction_control, "{sql}");
        }
    }

    #[test]
    fn unguarded_statements_are_found_on_both_paths() {
        let unguarded = |sql: &str| duckdb_classification(sql).unguarded;
        assert_eq!(
            unguarded("UPDATE t SET a = 1"),
            [Unguarded::UpdateWithoutWhere]
        );
        assert_eq!(unguarded("DELETE FROM t"), [Unguarded::DeleteWithoutWhere]);
        assert_eq!(unguarded("DROP TABLE t"), [Unguarded::Drop]);
        assert_eq!(unguarded("TRUNCATE t"), [Unguarded::Truncate]);
        assert_eq!(unguarded("DROP MACRO m"), [Unguarded::Drop]);
        assert_eq!(writes("DROP MACRO m"), Writes::Yes);
        assert!(unguarded("DELETE FROM t WHERE a = 1").is_empty());
        // DuckDB list and struct literals send these down the keyword path.
        assert_eq!(
            unguarded("UPDATE t SET l = [1, 2]"),
            [Unguarded::UpdateWithoutWhere]
        );
        assert_eq!(
            unguarded("UPDATE t SET s = {'k': 1}"),
            [Unguarded::UpdateWithoutWhere]
        );
        assert_eq!(
            unguarded("WITH a AS (SELECT [1]) DELETE FROM t"),
            [Unguarded::DeleteWithoutWhere]
        );
        assert!(unguarded("UPDATE t SET l = [1] WHERE id = 1").is_empty());
    }

    #[test]
    fn transaction_control_and_session_settings() {
        for sql in [
            "BEGIN",
            "BEGIN TRANSACTION READ ONLY",
            "COMMIT",
            "END",
            "ROLLBACK",
            "ABORT",
        ] {
            assert!(duckdb_classification(sql).transaction_control, "{sql}");
        }
        assert!(!duckdb_classification("SELECT 1").transaction_control);
        for sql in [
            "SET threads = 2",
            "RESET threads",
            "USE memory",
            "set search_path = 'main'",
        ] {
            assert!(duckdb_sets_session(sql), "{sql}");
        }
        assert!(!duckdb_sets_session("SELECT 1") && !duckdb_sets_session("-- SET\nSELECT 1"));
    }
}
