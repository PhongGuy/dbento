//! Formatting is idempotent and changes only whitespace and keyword case, checked against
//! our lexer and against each engine's own lexer.

use super::lex::{Kind, lex};
use super::{FormatOptions, KeywordCase, format_sql};
use crate::Engine::{self, Mysql, Postgres, Sqlite};

const CORPUS: &[(Engine, &str)] = &[
    (
        Postgres,
        "select c.country, count(*) as orders, sum(o.total) as revenue from orders as o join customers as c on c.id = o.customer_id where o.status in ('delivered', 'shipped') group by c.country order by revenue desc;",
    ),
    (
        Postgres,
        "select id::text, '2024-01-01'::date, x::numeric(10,2) from t where a->>'k' = 'v' and tags && array['a'] and arr[1:2] = '{1}'",
    ),
    (
        Postgres,
        "create function f() returns int as $$ select 1 from t $$ language sql; select $tag$ it's select $tag$, $1, e'a\\'b', B'101', U&'d\\0061'",
    ),
    (
        Postgres,
        "with recursive r(n) as (select 1 union all select n + 1 from r where n < 5) select n from r",
    ),
    (
        Postgres,
        "with recent as (select * from orders where created_at > now() - interval '7 days'), top as (select customer_id, sum(total) s from recent group by 1) select * from top order by s desc nulls last limit 10 offset 5",
    ),
    (
        Postgres,
        "-- leading\nselect a, /* inline /* nested */ */ b -- trailing\nfrom t /* multi\n line */ where x = 1 -- end",
    ),
    (
        Postgres,
        "select \"Select\", \"a\"\"b\", U&\"d\\0061\" from \"My Table\" where name = 'it''s'",
    ),
    (
        Postgres,
        "insert into t (a, b) values (1, 'x'), (2, 'y') on conflict (a) do update set b = excluded.b returning *",
    ),
    (
        Postgres,
        "update t set a = a + 1, b = default where id in (select id from u where u.x between 1 and 2) returning id",
    ),
    (
        Postgres,
        "delete from t using u where t.id = u.id and not exists (select 1 from v where v.id = t.id)",
    ),
    (
        Postgres,
        "select row_number() over (partition by a order by b desc) rn, left(name, 3), case when a then 1 else 2 end from t",
    ),
    (
        Postgres,
        "select a from t1 union select a from t2 except select a from t3 intersect select a from t4",
    ),
    (Postgres, "select 1;select 2 ;\n\n  select 3"),
    (
        Postgres,
        "select a=-1, b<>2, c!=3, d||'x', e @> f, g::int[] from t",
    ),
    (
        Postgres,
        "begin; alter table t add column c int default 0; commit;",
    ),
    (
        Postgres,
        "explain analyze select * from t t1 cross join t t2 natural full outer join t3",
    ),
    (
        Postgres,
        "select 'a'\n'b' as s, 'c' -- note\n'd', x from t where y = 'e'\n  'f'",
    ),
    (Postgres, "select 'a' /*c*/\n'b', 'c'\n/*d*/ 'e' from t"),
    (
        Postgres,
        "begin;\nupdate t set a = 1, b = 2 where id = 3;\ncommit;",
    ),
    (
        Postgres,
        "create function g() returns int language sql begin atomic select 1; select 2; end; select a, b from t",
    ),
    (
        Postgres,
        "do $$ begin perform 1; end $$; select a, b from t where c = 1",
    ),
    (Postgres, "select 0xE-1, 0x1F, 1e-3, 1.5E+2 from t"),
    (
        Postgres,
        "BEGIN; select distinct on (a) a, b from t left /*c*/ join u on u.id = t.id for update skip locked -- c\n;",
    ),
    (
        Postgres,
        "create table begin (a int); select -- c\n if(a, 1, 2), row(1, 2), '\u{6f22}\u{5b57}' from t\n-- d\nwhere x = 1",
    ),
    (
        Sqlite,
        "select `a b`, [c d], x from `t` where x = ? and y = ?1 and z = :name and w = @v and q = $p",
    ),
    (
        Sqlite,
        "select a, -- first\n  b /* second */ from t where s = 'it''s' and q = \"Q\"",
    ),
    (
        Sqlite,
        "insert or replace into t(a, b) values(1, x'ff') on conflict(a) do nothing",
    ),
    (
        Sqlite,
        "create table t (id integer primary key autoincrement, name text not null unique)",
    ),
    (
        Sqlite,
        "create trigger tr after insert on t begin update u set n = n + 1; end; select 1",
    ),
    (
        Sqlite,
        "create trigger tr2 before delete on t for each row when old.id > 0 begin delete from u where u.t_id = old.id; insert into log values (old.id, 'gone'); end;\nselect a, b from t",
    ),
    (
        Sqlite,
        "with c as (select 1 x) select x from c where x > 0 group by x having count(*) > 0 order by x limit 1",
    ),
    (Sqlite, "pragma table_info('t'); vacuum; select 1"),
    (
        Sqlite,
        "select 1.5e-3, .5, 0x1F, -1 from t where a glob 'x*' and b like '%y' escape '\\'",
    ),
    (
        Mysql,
        "select `a b`, count(*) as n from `t` where s = \"x\" and y = ? and z = @@sql_mode and w = @v group by `a b` limit 5",
    ),
    (
        Mysql,
        "select a, # first\n  b /* second */ from t where s = 'it''s' -- third\n and q = x'ff'",
    ),
    (
        Mysql,
        "insert into t (a, b) values (1, 'x'), (2, 'y') on duplicate key update b = values(b)",
    ),
    (
        Mysql,
        "/*!40101 set names utf8mb4 */; select 1; select a--b from t",
    ),
    (
        Mysql,
        "create table t (id int unsigned not null auto_increment primary key, n varchar(10) default 'x') engine = innodb",
    ),
    (
        Mysql,
        "update t set a = a + 1 where id in (select id from u where u.x between 1 and 2) order by id limit 3",
    ),
];

/// Deterministic xorshift; the corpus is fixed, so failures reproduce exactly.
fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// `sql` with each whitespace run replaced by a random run; a run holding a newline keeps one,
/// so a line comment never swallows the next token.
fn respace(engine: Engine, sql: &str, seed: u64) -> String {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut out = String::new();
    for t in lex(engine, sql).unwrap() {
        let text = &sql[t.range.clone()];
        if t.kind != Kind::Space {
            out.push_str(text);
            continue;
        }
        let with_nl = ["\n", "\n\t  ", " \r\n", "\n\n"];
        let any = [" ", "  ", "\t", "\n", "\n    "];
        let pick = next(&mut state) as usize;
        out.push_str(if text.contains('\n') {
            with_nl[pick % with_nl.len()]
        } else {
            any[pick % any.len()]
        });
    }
    out
}

fn bare_word(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}
fn norm(s: &str) -> String {
    if bare_word(s) {
        s.to_ascii_uppercase()
    } else {
        s.to_owned()
    }
}

fn ours(engine: Engine, sql: &str) -> Vec<String> {
    lex(engine, sql)
        .unwrap()
        .iter()
        .filter(|t| t.kind != Kind::Space)
        .map(|t| norm(&sql[t.range.clone()]))
        .collect()
}

fn engines(engine: Engine, sql: &str) -> Vec<String> {
    match engine {
        Postgres => pg_query::scan(sql)
            .expect("pg scans")
            .tokens
            .iter()
            .map(|t| norm(&sql[t.start as usize..t.end as usize]))
            .collect(),
        Mysql => {
            use sqlparser::dialect::MySqlDialect;
            use sqlparser::tokenizer::{Token, Tokenizer, Whitespace};
            Tokenizer::new(&MySqlDialect {}, sql)
                .tokenize()
                .expect("mysql tokenizes")
                .into_iter()
                .filter(|t| {
                    !matches!(
                        t,
                        Token::Whitespace(
                            Whitespace::Space | Whitespace::Newline | Whitespace::Tab
                        )
                    )
                })
                .map(|t| norm(&t.to_string()))
                .collect()
        }
        Sqlite
        | Engine::Mssql
        | Engine::Duckdb
        | Engine::Mongodb
        | Engine::Surrealdb
        | Engine::Redis => {
            use sqlite3_parser::lexer::{Scanner, sql::Tokenizer};
            let mut s = Scanner::new(Tokenizer::new());
            let mut out = Vec::new();
            while let (_, Some((tok, _)), _) = s.scan(sql.as_bytes()).expect("sqlite scans") {
                out.push(norm(std::str::from_utf8(tok).unwrap()));
            }
            out
        }
    }
}

#[test]
fn formatting_is_idempotent_and_changes_only_whitespace_and_keyword_case() {
    for &(engine, sql) in CORPUS {
        for seed in 0..12u64 {
            let input = if seed == 0 {
                sql.to_owned()
            } else {
                respace(engine, sql, seed)
            };
            for case in [
                KeywordCase::Upper,
                KeywordCase::Lower,
                KeywordCase::Preserve,
            ] {
                let o = FormatOptions::default().with_keyword_case(case);
                let out = format_sql(engine, &input, &o);
                assert_eq!(
                    format_sql(engine, &out, &o),
                    out,
                    "not idempotent ({case:?}):\n{input}"
                );
                assert_eq!(
                    ours(engine, &out),
                    ours(engine, &input),
                    "tokens changed ({case:?}):\n{input}\n---\n{out}"
                );
                assert_eq!(
                    engines(engine, &out),
                    engines(engine, &input),
                    "engine tokens changed ({case:?}):\n{input}\n---\n{out}"
                );
            }
        }
    }
}

#[test]
fn the_corpus_actually_gets_reformatted() {
    let changed = CORPUS
        .iter()
        .filter(|(e, s)| format_sql(*e, s, &FormatOptions::default()) != *s)
        .count();
    assert!(
        changed >= CORPUS.len() - 2,
        "only {changed} of {} entries changed",
        CORPUS.len()
    );
}

/// `sql` with `with` put at its `nth` token boundary, replacing whitespace found there;
/// None past the last boundary.
fn inject(engine: Engine, sql: &str, nth: usize, with: &str) -> Option<String> {
    let toks = lex(engine, sql).unwrap();
    let mut gaps = Vec::new();
    for (i, t) in toks.iter().enumerate().skip(1) {
        match (t.kind, toks[i - 1].kind) {
            (Kind::Space, _) => gaps.push(t.range.clone()),
            (_, Kind::Space) => {}
            _ => gaps.push(t.range.start..t.range.start),
        }
    }
    let at = gaps.get(nth)?;
    Some(format!("{}{with}{}", &sql[..at.start], &sql[at.end..]))
}

#[test]
fn comments_at_any_gap_keep_every_token_and_comment() {
    for &(engine, sql) in CORPUS {
        for with in [
            "\n-- own line\n",
            " -- trailing\n",
            "\n/* own */\n",
            " /* inline */ ",
        ] {
            for nth in 0.. {
                let Some(input) = inject(engine, sql, nth, with) else {
                    break;
                };
                let o = FormatOptions::default();
                let out = format_sql(engine, &input, &o);
                assert_eq!(
                    format_sql(engine, &out, &o),
                    out,
                    "not idempotent:\n{input}\n---\n{out}"
                );
                assert_eq!(
                    ours(engine, &out),
                    ours(engine, &input),
                    "tokens changed:\n{input}\n---\n{out}"
                );
                assert_eq!(
                    engines(engine, &out),
                    engines(engine, &input),
                    "engine tokens changed:\n{input}\n---\n{out}"
                );
            }
        }
    }
}
