use dbento_core::Engine::{self, Mysql, Postgres, Sqlite};
use dbento_core::{FormatOptions, KeywordCase, format_sql, line_comment_starts, splits_token};

fn fmt(engine: Engine, sql: &str) -> String {
    format_sql(engine, sql, &FormatOptions::default())
}
fn both(sql: &str, expected: &str) {
    for e in [Postgres, Sqlite] {
        assert_eq!(fmt(e, sql), expected, "{e:?}");
        assert_eq!(fmt(e, expected), expected, "{e:?} idempotent");
    }
}

#[test]
fn target_style() {
    both(
        "select c.country, count(*) as orders, sum(o.total) as revenue from orders as o join customers as c on c.id = o.customer_id where o.status in ('delivered', 'shipped') group by c.country order by revenue desc;",
        "SELECT c.country,\n       count(*) AS orders,\n       sum(o.total) AS revenue\nFROM orders AS o\nJOIN customers AS c ON c.id = o.customer_id\nWHERE o.status IN ('delivered', 'shipped')\nGROUP BY c.country\nORDER BY revenue DESC;",
    );
}

#[test]
fn postgres_casts() {
    assert_eq!(
        fmt(
            Postgres,
            "select id::text, '2024-01-01'::date as d, x::numeric(10,2) from t where a->>'k' = 'v'"
        ),
        "SELECT id::text,\n       '2024-01-01'::date AS d,\n       x::numeric(10, 2)\nFROM t\nWHERE a->>'k' = 'v'"
    );
}

#[test]
fn postgres_dollar_quotes_are_untouched() {
    assert_eq!(
        fmt(
            Postgres,
            "select $$it's select from$$ as a, $tag$ x $$ y $tag$ as b from t"
        ),
        "SELECT $$it's select from$$ AS a,\n       $tag$ x $$ y $tag$ AS b\nFROM t"
    );
}

#[test]
fn postgres_prefix_strings_stay_whole() {
    assert_eq!(
        fmt(Postgres, r"select e'it\'s', b'101', x'ff' from t"),
        "SELECT e'it\\'s',\n       b'101',\n       x'ff'\nFROM t"
    );
}

#[test]
fn ctes_align_inside_their_parens() {
    assert_eq!(
        fmt(
            Postgres,
            "with recent as (select * from orders where created_at > now() - interval '7 days'), top as (select customer_id, sum(total) s from recent group by 1) select * from top order by s desc limit 10"
        ),
        "WITH recent AS (SELECT *\n                FROM orders\n                WHERE created_at > now() - INTERVAL '7 days'),\n     top AS (SELECT customer_id,\n                    sum(total) s\n             FROM recent\n             GROUP BY 1)\nSELECT *\nFROM top\nORDER BY s DESC\nLIMIT 10"
    );
}

#[test]
fn line_and_block_comments_are_kept_in_place() {
    both(
        "-- leading\nselect a, /* inline */ b -- trailing\nfrom t /* multi\n line */ where x = 1",
        "-- leading\nSELECT a,\n       /* inline */ b -- trailing\nFROM t /* multi\n line */\nWHERE x = 1",
    );
    both(
        "select a, -- first\n  b -- second\nfrom t",
        "SELECT a, -- first\n       b -- second\nFROM t",
    );
}

#[test]
fn quoted_identifiers_and_strings_keep_their_case() {
    both(
        "select \"Select\", \"from x\" from \"My Table\" where name = 'select from where'",
        "SELECT \"Select\",\n       \"from x\"\nFROM \"My Table\"\nWHERE name = 'select from where'",
    );
}

#[test]
fn sqlite_quirks() {
    assert_eq!(
        fmt(
            Sqlite,
            "select `a b`, [c d], x from `t` where x = ? and y = ?1 and z = :name and w = @v and q = $p"
        ),
        "SELECT `a b`,\n       [c d],\n       x\nFROM `t`\nWHERE x = ?\n      AND y = ?1\n      AND z = :name\n      AND w = @v\n      AND q = $p"
    );
}

#[test]
fn subqueries_joins_and_between() {
    both(
        "select a from t left join u on u.id = t.id where t.id in (select id from v where w = 1)",
        "SELECT a\nFROM t\nLEFT JOIN u ON u.id = t.id\nWHERE t.id IN (SELECT id\n               FROM v\n               WHERE w = 1)",
    );
    both(
        "select * from t where a between 1 and 2 and b = 3",
        "SELECT *\nFROM t\nWHERE a BETWEEN 1 AND 2\n      AND b = 3",
    );
    both(
        "select distinct a, b from t",
        "SELECT DISTINCT a,\n                b\nFROM t",
    );
}

#[test]
fn functions_keep_their_case_and_inline_parens_stay_inline() {
    both(
        "select left(name, 3), Count(*) from t",
        "SELECT left(name, 3),\n       Count(*)\nFROM t",
    );
    both(
        "select row_number() over (partition by a order by b) from t",
        "SELECT row_number() OVER (PARTITION BY a ORDER BY b)\nFROM t",
    );
    both(
        "select coalesce(a,\n  b) from t",
        "SELECT coalesce(a, b)\nFROM t",
    );
}

#[test]
fn spacing_only_changes_around_commas_and_parens() {
    both("select a , b from t", "SELECT a,\n       b\nFROM t");
    both(
        "select a+b, c-1 from t where x=1",
        "SELECT a+b,\n       c-1\nFROM t\nWHERE x=1",
    );
    both("select ( a ) from t", "SELECT (a)\nFROM t");
}

#[test]
fn string_continuation_keeps_its_line_break() {
    both(
        "select 'a'\n'b' as s from t",
        "SELECT 'a'\n'b' AS s\nFROM t",
    );
    both("select 'a' 'b' from t", "SELECT 'a' 'b'\nFROM t");
}

#[test]
fn string_continuation_gap_is_kept_verbatim() {
    // Postgres lexes `'a'<gap>'b'` as one literal spanning the gap, so the gap is not
    // re-indented.
    both(
        "select x, 'a' \r\n\t  'b' from t where y = 'e'\n  'f' and z = 1",
        "SELECT x,\n       'a' \r\n\t  'b'\nFROM t\nWHERE y = 'e'\n  'f'\n      AND z = 1",
    );
}

#[test]
fn line_comments_inside_a_string_continuation_are_kept_verbatim() {
    // `--` comments between the pieces belong to the one literal, so neither they nor
    // their whitespace move.
    assert_eq!(
        fmt(
            Postgres,
            "select 'c' -- note\n'd' -- more\n\n  'e', x from t"
        ),
        "SELECT 'c' -- note\n'd' -- more\n\n  'e',\n       x\nFROM t"
    );
    assert_eq!(
        fmt(Postgres, "select x, 'c'\n   -- note\n 'd' from t"),
        "SELECT x,\n       'c'\n   -- note\n 'd'\nFROM t"
    );
}

#[test]
fn is_distinct_from_stays_in_its_condition() {
    both(
        "select * from t where a is distinct from b and c is not distinct from d or e = 1",
        "SELECT *\nFROM t\nWHERE a IS DISTINCT FROM b\n      AND c IS NOT DISTINCT FROM d\n      OR e = 1",
    );
}

#[test]
fn dml_statements() {
    both(
        "update t set a = 1, b = 2 where id = 3",
        "UPDATE t\nSET a = 1,\n    b = 2\nWHERE id = 3",
    );
    both(
        "insert into t (a, b) values (1, 2), (3, 4) returning a",
        "INSERT INTO t (a, b)\nVALUES (1, 2),\n       (3, 4)\nRETURNING a",
    );
    both("delete from t where id = 1", "DELETE FROM t\nWHERE id = 1");
    both(
        "insert or replace into t (a) values (1) on conflict (a) do update set a = 2",
        "INSERT OR REPLACE INTO t (a)\nVALUES (1)\nON CONFLICT (a) DO UPDATE\nSET a = 2",
    );
    both(
        "select a from t1 union all select a from t2",
        "SELECT a\nFROM t1\nUNION ALL\nSELECT a\nFROM t2",
    );
}

#[test]
fn statements_are_separated_by_one_blank_line() {
    both("select 1; select 2", "SELECT 1;\n\nSELECT 2");
    both(
        "select 1;\n-- next\nselect 2;",
        "SELECT 1;\n\n-- next\nSELECT 2;",
    );
}

#[test]
fn outer_whitespace_is_kept() {
    both("\n  select 1;\n", "\n  SELECT 1;\n");
    both("\nselect b, c from t;", "\nSELECT b,\n       c\nFROM t;");
}

#[test]
fn other_statements_get_keyword_case_only() {
    both(
        "create table t (id int primary key, name text not null)",
        "CREATE TABLE t (id int PRIMARY KEY, name text NOT NULL)",
    );
    assert_eq!(
        fmt(
            Postgres,
            "create function f() returns int as $$ select 1 from t $$ language sql;"
        ),
        "CREATE FUNCTION f() RETURNS int AS $$ select 1 from t $$ LANGUAGE sql;"
    );
    both("pragma  table_info(t)", "pragma  table_info(t)");
}

#[test]
fn keyword_case_options() {
    let lower = FormatOptions::default().with_keyword_case(KeywordCase::Lower);
    let keep = FormatOptions::default().with_keyword_case(KeywordCase::Preserve);
    for e in [Postgres, Sqlite] {
        assert_eq!(format_sql(e, "SELECT A FROM T", &lower), "select A\nfrom T");
        assert_eq!(format_sql(e, "Select a From t", &keep), "Select a\nFrom t");
    }
}

#[test]
fn input_that_does_not_lex_is_returned_unchanged() {
    for (e, sql) in [
        (Postgres, "select 'abc from t"),
        (Postgres, "select $$ abc"),
        (Sqlite, "select /* x from t"),
        (Sqlite, "select [a from t"),
    ] {
        assert_eq!(fmt(e, sql), sql);
    }
    both("", "");
    both("  \n", "  \n");
}

#[test]
fn sqlite_trigger_body_is_one_statement() {
    assert_eq!(
        fmt(
            Sqlite,
            "create trigger tr after insert on t begin\n  update u set a = case when new.a then 1 end;\n  insert into v values (1);\nend;\nselect a, b from t"
        ),
        "CREATE TRIGGER tr AFTER INSERT ON t BEGIN\n  UPDATE u SET a = CASE WHEN new.a THEN 1 END;\n  INSERT INTO v VALUES (1);\nEND;\nSELECT a,\n       b\nFROM t"
    );
}

#[test]
fn source_whitespace_is_kept_next_to_other_statements() {
    both(
        "select 1;\ncreate table t (a int);  select 2",
        "SELECT 1;\nCREATE TABLE t (a int);  SELECT 2",
    );
}

#[test]
fn for_and_within_are_keywords() {
    both("select a from t for update", "SELECT a\nFROM t FOR UPDATE");
    assert_eq!(
        fmt(
            Postgres,
            "select percentile_cont(0.5) within group (order by a) from t"
        ),
        "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY a)\nFROM t"
    );
}

#[test]
fn and_inside_case_stays_inline() {
    both(
        "select * from t where case when a and b then 1 else 0 end = 1 and c = 2",
        "SELECT *\nFROM t\nWHERE CASE WHEN a AND b THEN 1 ELSE 0 END = 1\n      AND c = 2",
    );
}

#[test]
fn splits_token_is_an_end_inside_a_literal_quote_or_comment() {
    let sql = "select 'a select b'  , \"x y\" -- c d\n/* e f */ $q$ g $q$, \u{e6}\u{f8}";
    let inner = |needle: &str| {
        let at = sql.find(needle).unwrap();
        at..at + needle.len()
    };
    for e in [Postgres, Sqlite] {
        assert!(!splits_token(e, sql, 0..sql.len()), "{e:?} whole");
        assert!(
            !splits_token(e, sql, inner("'a select b'  ,")),
            "{e:?} token and space ends"
        );
        assert!(
            !splits_token(e, sql, 0..sql.find("  ,").unwrap() + 1),
            "{e:?} end in space"
        );
        for cut in ["select b", "x y", "c d", "e f"] {
            assert!(splits_token(e, sql, inner(cut)), "{e:?} {cut}");
        }
        assert!(
            splits_token(e, "select 'open", 0..12),
            "{e:?} unlexable range"
        );
    }
    assert!(splits_token(Postgres, sql, inner(" g ")));
}

#[test]
fn index_and_window_frame_words_are_keywords() {
    both(
        "create index if not exists ix on t (a)",
        "CREATE INDEX IF NOT EXISTS ix ON t (a)",
    );
    both(
        "select sum(a) over (order by b rows between unbounded preceding and current row) from t",
        "SELECT sum(a) OVER (ORDER BY b ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)\nFROM t",
    );
    assert_eq!(
        fmt(
            Postgres,
            "select a from t where b like 'x!%' escape '!' offset 1 rows fetch first 5 rows only"
        ),
        "SELECT a\nFROM t\nWHERE b LIKE 'x!%' ESCAPE '!'\nOFFSET 1 ROWS FETCH FIRST 5 ROWS ONLY"
    );
}

#[test]
fn qualified_names_keep_keyword_words_as_written() {
    both(
        "select t.index, t.rows, t.row from t",
        "SELECT t.index,\n       t.rows,\n       t.row\nFROM t",
    );
}

#[test]
fn insert_default_values_stays_on_one_line() {
    both(
        "insert into t default values",
        "INSERT INTO t DEFAULT VALUES",
    );
}

#[test]
fn format_options_compare_by_value() {
    let lower = FormatOptions::default().with_keyword_case(KeywordCase::Lower);
    assert_eq!(FormatOptions::default(), FormatOptions::default());
    assert_ne!(FormatOptions::default(), lower);
}

#[test]
fn a_line_comment_on_its_own_line_stays_on_its_own_line() {
    both(
        "select a\nfrom t\nwhere x = 1\n-- c\nand y = 2",
        "SELECT a\nFROM t\nWHERE x = 1\n      -- c\n      AND y = 2",
    );
    both("select a\n-- c\nfrom t", "SELECT a\n-- c\nFROM t");
    both(
        "update t set a = 1\n-- c\nwhere b = 2",
        "UPDATE t\nSET a = 1\n-- c\nWHERE b = 2",
    );
    // Between continued strings the comment is inside one literal, so it is not re-indented.
    assert_eq!(
        fmt(Postgres, "select 'a'\n-- c\n'b' from t"),
        "SELECT 'a'\n-- c\n'b'\nFROM t"
    );
}

#[test]
fn comments_next_to_separators_and_clause_keywords() {
    both("select a from t -- c\n;", "SELECT a\nFROM t -- c\n;");
    both(
        "select a -- c\n, b from t",
        "SELECT a -- c\n       , b\nFROM t",
    );
    both(
        "select -- c\n a, b from t",
        "SELECT -- c\n       a,\n       b\nFROM t",
    );
    both(
        "select a from t left /*c*/ join u on u.id = t.id",
        "SELECT a\nFROM t\nLEFT /*c*/ JOIN u ON u.id = t.id",
    );
}

#[test]
fn distinct_on_aligns_the_list_after_its_parens() {
    assert_eq!(
        fmt(Postgres, "select distinct on (a, b) a, b from t"),
        "SELECT DISTINCT ON (a, b) a,\n                          b\nFROM t"
    );
}

#[test]
fn locking_and_trigger_words_are_keywords_and_if_and_row_calls_are_not() {
    both(
        "select a from t for update skip locked",
        "SELECT a\nFROM t FOR UPDATE SKIP LOCKED",
    );
    both(
        "create trigger tr after insert on t for each row execute function f()",
        "CREATE TRIGGER tr AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION f()",
    );
    both(
        "select if(a, 1, 2), row(1, 2) from t",
        "SELECT if(a, 1, 2),\n       row(1, 2)\nFROM t",
    );
}

#[test]
fn alignment_counts_display_width() {
    let want = format!(
        "WITH \u{6f22}\u{5b57} AS (SELECT a,\n{}b\n{}FROM t)\nSELECT 1",
        " ".repeat(21),
        " ".repeat(14)
    );
    both(
        "with \u{6f22}\u{5b57} as (select a, b from t) select 1",
        &want,
    );
}

#[test]
fn begin_as_a_name_opens_no_block() {
    both(
        "create table begin (a int);\nselect a, b from t",
        "CREATE TABLE BEGIN (a int);\nSELECT a,\n       b\nFROM t",
    );
}

#[test]
fn a_multi_line_statement_after_another_on_its_line_starts_a_new_line() {
    both(
        "BEGIN; select a, b from t",
        "BEGIN;\nSELECT a,\n       b\nFROM t",
    );
    both("BEGIN; select 1; COMMIT;", "BEGIN; SELECT 1; COMMIT;");
    both(
        "BEGIN; -- start\nselect a, b from t",
        "BEGIN; -- start\nSELECT a,\n       b\nFROM t",
    );
}

#[test]
fn a_half_typed_quote_after_the_range_does_not_split_it() {
    let sql = "select a, b from t; select 'x";
    for e in [Postgres, Sqlite] {
        assert!(!splits_token(e, sql, 0..18), "{e:?} before the quote");
        assert!(splits_token(e, sql, 0..sql.len() - 1), "{e:?} inside it");
    }
}

#[test]
fn line_comment_starts_skips_dashes_in_strings_and_dollar_quotes() {
    let sql = "select '--a', $q$ -- b $q$ -- real\nselect 1";
    let real = sql.find("-- real").unwrap();
    assert_eq!(line_comment_starts(Postgres, sql), Some(vec![real]));
    let sql = "select '--a' -- real\nselect 1";
    let real = sql.find("-- real").unwrap();
    for e in [Postgres, Sqlite] {
        assert_eq!(line_comment_starts(e, sql), Some(vec![real]), "{e:?}");
    }
    assert_eq!(line_comment_starts(Postgres, "select 'open"), None);
}

#[test]
fn mysql_layout_keeps_its_quotes_comments_and_variables() {
    assert_eq!(
        fmt(
            Mysql,
            "select `a b`, count(*) as n # first\nfrom `t` where s = \"x\" and y = ? and z = @@sql_mode"
        ),
        "SELECT `a b`,\n       count(*) AS n # first\nFROM `t`\nWHERE s = \"x\"\n      AND y = ?\n      AND z = @@sql_mode"
    );
    let versioned = fmt(Mysql, "/*!40101 set names utf8mb4 */;\nselect 1");
    assert!(
        versioned.starts_with("/*!40101 set names utf8mb4 */") && versioned.ends_with("SELECT 1"),
        "{versioned}"
    );
    for unchanged in [
        "select 'a\\'b' from t",
        "DELIMITER //\ncreate procedure p() select 1 //\nDELIMITER ;",
    ] {
        assert_eq!(fmt(Mysql, unchanged), unchanged);
    }
    assert_eq!(
        line_comment_starts(Mysql, "select 1 # a\n-- b\n--c\n'#'"),
        Some(vec![9, 13])
    );
    assert!(splits_token(Mysql, "select `a b` from t", 9..10));
    assert!(!splits_token(Mysql, "select `a b` from t", 6..7));
}
