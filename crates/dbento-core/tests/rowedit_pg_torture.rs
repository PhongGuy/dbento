mod support;

use dbento_core::{Change, EditBatch, EditValue, apply, render_sql};
use support::{Cell, PgFixture, edit_value, pairs, pg_fixture, row_key, val};

async fn scalar(fx: &PgFixture, sql: &str) -> String {
    fx.db.scalar(sql).await.unwrap()
}

const TORTURE: &str = r#"
CREATE EXTENSION IF NOT EXISTS citext;
CREATE SCHEMA "My S";
CREATE TYPE "My S"."Mood" AS ENUM ('ok', 'sad');
CREATE TYPE pair AS (a int, b text);
CREATE DOMAIN posarr AS int[] CHECK (cardinality(VALUE) > 0);
CREATE TABLE tt(id int PRIMARY KEY,
 i2 smallint, i8 bigint, nu numeric, nf numeric, r4 real, f8 float8, f8s float8, mo money,
 dt date, dbc date, ts timestamp, tz timestamptz, tt timetz, iv interval,
 tx text, ch char(5), cit citext, by bytea, bt bit(3), vb varbit,
 jb jsonb, js json, xm xml, ar int[], ar2 int[], arl int[], cmp pair, cmpa pair[],
 en "My S"."Mood", ma "My S"."Mood"[], da posarr,
 rg int4range, er int4range, nm nummultirange, ip inet, ci cidr, mac macaddr, m8 macaddr8, uu uuid,
 pt point, ln line, ls lseg, bx box, pa path, pg polygon, cr circle, tv tsvector, tq tsquery,
 lsn pg_lsn, o oid, rc regclass,
 em text, npi numeric, nni numeric, rn real, n38 numeric(38,10), tm time,
 ub int4range, i8r int8range, nr numrange, dr daterange, tzr tstzrange,
 rt regtype, sn pg_snapshot, "Sel ect é" text);
INSERT INTO tt VALUES (1,
 -32768, 9223372036854775807, repeat('9', 1000)::numeric, 'NaN', '-0', '-Infinity', '4.9e-324', 12.34,
 'infinity', '0044-03-15 BC', '2024-01-02 03:04:05.123456', '2024-01-02 03:04:05+05:30', '03:04:05-08',
 '1 year 2 mons -3 days 04:05:06.789',
 'x 👩‍👩‍👧 مرحبا ''q'' \', 'ab', 'Hi', '\x00ff', '101', '1',
 '{"b":1,"a":[1,2]}', '{"a":1,"a":2}', '<a>x</a>', '{1,NULL}', '{{1,2},{3,4}}', '[0:2]={1,2,3}',
 '(1,"x y")', ARRAY['(1,a)'::pair], 'sad', '{sad,ok}', '{1}',
 '[1,5)', 'empty', '{[1,2], (3,4)}', '10.0.0.1/8', '10.0.0.0/8', '08:00:2b:01:02:03',
 '08:00:2b:01:02:03:04:05', 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11',
 '(1,2)', '{1,2,3}', '[(1,2),(3,4)]', '(3,4),(1,2)', '[(1,2),(3,4)]', '((1,2),(3,4),(5,6))',
 '<(1,2),3>', 'a:1 b:2', 'a & !b', '16/B374D848', 4294967295, 'tt',
 '', 'Infinity', '-Infinity', 'NaN', 123.4567890123, '24:00:00',
 '[1,)', '(,)', 'empty', '[2024-01-01,infinity)', '[2024-01-01 00:00+00,infinity)',
 'int[]', '10:20:10,14,15', 'é combined');
INSERT INTO tt(id) VALUES (2);
"#;

#[tokio::test]
async fn the_type_torture_row_round_trips_through_update_insert_null_default_and_delete() {
    let fx = pg_fixture("").await;
    fx.db.admin().await.batch_execute(TORTURE).await.unwrap();
    let target = fx.target("public", "tt").await;
    let names: Vec<String> = target
        .columns()
        .iter()
        .map(|c| c.name().to_owned())
        .collect();
    let source = fx.row("SELECT * FROM tt WHERE id = 1").await;
    assert_eq!(source.len(), names.len());
    let copy: Vec<(String, EditValue)> = names
        .iter()
        .zip(&source)
        .skip(1)
        .map(|(n, c)| (n.clone(), edit_value(c)))
        .collect();
    let mut insert = copy.clone();
    insert.insert(0, ("id".to_owned(), val("3")));
    let applied = fx
        .apply_changes(
            &target,
            vec![
                Change::Update {
                    key: row_key(&[("id", val("2"))]),
                    set: copy,
                },
                Change::Insert { values: insert },
            ],
        )
        .await
        .unwrap();
    applied.commit().await.unwrap();
    assert_eq!(
        &fx.row("SELECT * FROM tt WHERE id = 2").await[1..],
        &source[1..]
    );
    assert_eq!(
        &fx.row("SELECT * FROM tt WHERE id = 3").await[1..],
        &source[1..]
    );

    // No column has a default, so DEFAULT clears a value just as NULL does.
    let nulls = names[1..]
        .iter()
        .map(|n| (n.clone(), EditValue::Null))
        .collect();
    let defaults = names[1..]
        .iter()
        .map(|n| (n.clone(), EditValue::Default))
        .collect();
    let applied = fx
        .apply_changes(
            &target,
            vec![
                Change::Update {
                    key: row_key(&[("id", val("2"))]),
                    set: nulls,
                },
                Change::Update {
                    key: row_key(&[("id", val("1"))]),
                    set: defaults,
                },
                Change::Delete {
                    key: row_key(&[("id", val("3"))]),
                },
            ],
        )
        .await
        .unwrap();
    applied.commit().await.unwrap();
    assert_eq!(scalar(&fx, "SELECT count(*) FROM tt").await, "2");
    for id in [1, 2] {
        let row = fx.row(&format!("SELECT * FROM tt WHERE id = {id}")).await;
        assert!(
            row[1..].iter().all(|c| *c == Cell::Null),
            "row {id}: {row:?}"
        );
    }
}

const EQ: &str = "DROP TABLE IF EXISTS eq; \
CREATE TABLE eq(id int PRIMARY KEY, v varchar(3), j jsonb, by bytea, ar text[], d text DEFAULT 'd'); \
INSERT INTO eq VALUES (1, 'a', '{}', NULL, NULL, 'x'), (2, 'b', NULL, NULL, NULL, 'y'), (4, 'c', NULL, NULL, NULL, 'z');";

#[tokio::test]
async fn render_sql_run_directly_matches_apply() {
    let fx = pg_fixture("").await;
    let admin = fx.db.admin().await;
    admin.batch_execute(EQ).await.unwrap();
    let target = fx.target("public", "eq").await;
    let changes = vec![
        Change::Update {
            key: row_key(&[("id", val("1"))]),
            set: pairs(&[
                ("v", val("a'b")),
                ("j", val(r#"{"k": "it's \\ ok"}"#)),
                ("by", EditValue::Bytes(vec![0, 255])),
                ("ar", val(r#"{"a,b",NULL}"#)),
            ]),
        },
        Change::Update {
            key: row_key(&[("id", val("2"))]),
            set: pairs(&[("d", EditValue::Default), ("v", EditValue::Null)]),
        },
        Change::Insert {
            values: pairs(&[("id", val("3")), ("v", val("é"))]),
        },
        Change::Delete {
            key: row_key(&[("id", val("4"))]),
        },
    ];
    let batch = EditBatch::new(target, changes).unwrap();
    let script = render_sql(&batch);
    let snapshot = "SELECT string_agg(eq::text, ' ' ORDER BY id) FROM eq";
    let applied = apply(&fx.ws, fx.conn(), Some(fx.secret()), batch)
        .await
        .unwrap();
    applied.commit().await.unwrap();
    let via_apply = scalar(&fx, snapshot).await;
    admin.batch_execute(EQ).await.unwrap();
    // With escapes read in plain literals, only the E'' form keeps a backslash intact.
    admin
        .batch_execute("SET standard_conforming_strings = off")
        .await
        .unwrap();
    admin.batch_execute(&script).await.unwrap();
    assert_eq!(scalar(&fx, snapshot).await, via_apply);
    assert!(
        via_apply.contains("a'b") && via_apply.contains("\\\\x00ff"),
        "{via_apply}"
    );
}

const LIT: &str = "DROP TABLE IF EXISTS lit; \
CREATE TABLE lit(id int8 PRIMARY KEY, a int2, n numeric, f float4, ok bool, s text); \
INSERT INTO lit VALUES (1, 1, 1.5, 0.5, false, 'x'), (2, 2, 2.5, 1.5, true, 'y');";

#[tokio::test]
async fn typed_literals_run_directly_match_apply() {
    let fx = pg_fixture("").await;
    let admin = fx.db.admin().await;
    admin.batch_execute(LIT).await.unwrap();
    let target = fx.target("public", "lit").await;
    let changes = vec![
        Change::Update {
            key: row_key(&[("id", val("1"))]),
            set: pairs(&[
                ("a", val("-7")),
                ("n", val("10.25")),
                ("f", val("0.1")),
                ("ok", val("t")),
                ("s", val("4")),
            ]),
        },
        Change::Update {
            key: row_key(&[("id", val("2"))]),
            set: pairs(&[("ok", val("FALSE"))]),
        },
        Change::Insert {
            values: pairs(&[("id", val("3")), ("ok", val("yes"))]),
        },
    ];
    let batch = EditBatch::new(target, changes).unwrap();
    let script = render_sql(&batch);
    assert!(
        script.contains(
            "SET a = -7, n = 10.25, f = '0.1'::pg_catalog.float4, ok = true, s = '4'::pg_catalog.text WHERE id = 1;"
        ),
        "{script}"
    );
    assert!(script.contains("SET ok = false WHERE id = 2;"), "{script}");
    assert!(script.contains("(id, ok) VALUES (3, true);"), "{script}");
    let snapshot = "SELECT string_agg(lit::text, ' ' ORDER BY id) FROM lit";
    let applied = apply(&fx.ws, fx.conn(), Some(fx.secret()), batch)
        .await
        .unwrap();
    applied.commit().await.unwrap();
    let via_apply = scalar(&fx, snapshot).await;
    admin.batch_execute(LIT).await.unwrap();
    admin.batch_execute(&script).await.unwrap();
    assert_eq!(scalar(&fx, snapshot).await, via_apply);
    assert_eq!(
        via_apply,
        "(1,-7,10.25,0.1,t,4) (2,2,2.5,1.5,f,y) (3,,,,t,)"
    );
}
