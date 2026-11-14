use super::sql::{BoundStatement, Param, bound};
use super::*;
use crate::{ColumnMeta, Engine, RelationRef};

fn col(name: &str, cast: &str, editable: bool, default: Option<&str>) -> EditColumn {
    let meta = ColumnMeta::new(name, cast, true, None, default.map(str::to_owned));
    EditColumn::new(meta, cast.to_owned(), editable)
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

fn pg() -> EditTarget {
    EditTarget::new(
        RelationRef::new("app", "My T"),
        Engine::Postgres,
        vec![
            col("id", "pg_catalog.int4", true, None),
            col("title", "pg_catalog.\"varchar\"", true, None),
            col("pic", "pg_catalog.bytea", true, None),
            col("g", "pg_catalog.int4", false, None),
        ],
        strings(&["id"]),
        vec![],
        false,
    )
}

fn pg_locator() -> EditTarget {
    EditTarget::new(
        RelationRef::new("public", "t"),
        Engine::Postgres,
        vec![col("a", "pg_catalog.int4", true, None)],
        strings(&["ctid", "xmin"]),
        strings(&["ctid", "xmin"]),
        true,
    )
}

fn pg_no_identity() -> EditTarget {
    EditTarget::new(
        RelationRef::new("public", "parts"),
        Engine::Postgres,
        vec![col("a", "pg_catalog.int4", true, None)],
        vec![],
        vec![],
        false,
    )
}

fn lite() -> EditTarget {
    EditTarget::new(
        RelationRef::new("main", "order items"),
        Engine::Sqlite,
        vec![
            col("id", "INTEGER", true, None),
            col("title", "TEXT", true, Some("'x''y'")),
            col("n", "INT", true, None),
            col("pic", "BLOB", true, None),
            col("g", "INT", false, None),
        ],
        strings(&["id"]),
        vec![],
        false,
    )
}

fn my() -> EditTarget {
    EditTarget::new(
        RelationRef::new("shop", "t"),
        Engine::Mysql,
        vec![col("id", "INT", true, None)],
        strings(&["id"]),
        vec![],
        false,
    )
}

#[test]
fn a_batch_needs_an_engine_that_edits_rows() {
    let insert = || {
        vec![Change::Insert {
            values: pairs(&[("id", t("1"))]),
        }]
    };
    let refused = BatchError::UnsupportedEngine {
        engine: Engine::Mysql,
    };
    assert_eq!(EditBatch::new(my(), insert()).unwrap_err(), refused);
    assert_eq!(
        EditBatch::new(my(), vec![]).unwrap_err(),
        refused,
        "the engine is checked before anything else"
    );
    assert_eq!(
        refused.to_string(),
        "editing rows is not available on mysql yet"
    );
    for target in [pg(), lite(), my()] {
        let engine = target.engine();
        assert_eq!(
            EditBatch::new(target, insert()).is_ok(),
            engine.capabilities().edit_rows,
            "{engine:?}"
        );
    }
}

fn t(s: &str) -> EditValue {
    EditValue::Text(s.to_owned())
}

fn pairs(p: &[(&str, EditValue)]) -> Vec<(String, EditValue)> {
    p.iter()
        .map(|(c, v)| ((*c).to_owned(), v.clone()))
        .collect()
}

fn key(p: &[(&str, EditValue)]) -> RowKey {
    RowKey::new(pairs(p))
}

fn p(s: &str) -> Param {
    Param::Text(s.to_owned())
}

fn stmt(sql: &str, params: Vec<Param>) -> BoundStatement {
    BoundStatement {
        sql: sql.to_owned(),
        params,
        returning: sql.contains(" RETURNING "),
    }
}

fn batch(target: EditTarget, changes: Vec<Change>) -> EditBatch {
    EditBatch::new(target, changes).unwrap()
}

#[test]
fn postgres_update_binds_text_through_the_catalog_cast_and_renders_literals() {
    let b = batch(
        pg(),
        vec![Change::Update {
            key: key(&[("id", t("7"))]),
            set: pairs(&[
                ("title", t("it's")),
                ("pic", EditValue::Bytes(vec![0x01, 0xff])),
            ]),
        }],
    );
    assert_eq!(
        bound(&b),
        vec![stmt(
            "UPDATE app.\"My T\" SET title = $1::text::pg_catalog.\"varchar\", pic = $2::text::pg_catalog.bytea WHERE id = $3::text::pg_catalog.int4",
            vec![p("it's"), p("\\x01ff"), p("7")],
        )]
    );
    assert_eq!(
        render_sql(&b),
        "UPDATE app.\"My T\" SET title = 'it''s'::pg_catalog.\"varchar\", pic = E'\\\\x01ff'::pg_catalog.bytea WHERE id = 7;"
    );
}

#[test]
fn postgres_null_and_default_are_keywords_and_non_editable_columns_are_dropped() {
    let b = batch(
        pg(),
        vec![Change::Update {
            key: key(&[("id", t("7"))]),
            set: pairs(&[
                ("title", EditValue::Default),
                ("pic", EditValue::Null),
                ("g", t("9")),
            ]),
        }],
    );
    match &b.changes()[0] {
        Change::Update { set, .. } => assert_eq!(set.len(), 2),
        other @ (Change::Insert { .. } | Change::Delete { .. }) => panic!("{other:?}"),
    }
    assert_eq!(
        bound(&b),
        vec![stmt(
            "UPDATE app.\"My T\" SET title = DEFAULT, pic = NULL WHERE id = $1::text::pg_catalog.int4",
            vec![p("7")],
        )]
    );
    assert_eq!(
        render_sql(&b),
        "UPDATE app.\"My T\" SET title = DEFAULT, pic = NULL WHERE id = 7;"
    );
}

#[test]
fn postgres_inserts_omit_defaults_and_fall_back_to_default_values() {
    let b = batch(
        pg(),
        vec![
            Change::Insert {
                values: pairs(&[
                    ("id", t("8")),
                    ("title", t("a")),
                    ("pic", EditValue::Default),
                    ("g", t("1")),
                ]),
            },
            Change::Insert {
                values: pairs(&[("title", EditValue::Default), ("g", t("1"))]),
            },
            Change::Insert {
                values: pairs(&[("pic", EditValue::Null)]),
            },
        ],
    );
    assert_eq!(
        bound(&b),
        vec![
            stmt(
                "INSERT INTO app.\"My T\" (id, title) VALUES ($1::text::pg_catalog.int4, $2::text::pg_catalog.\"varchar\")",
                vec![p("8"), p("a")],
            ),
            stmt("INSERT INTO app.\"My T\" DEFAULT VALUES", vec![]),
            stmt("INSERT INTO app.\"My T\" (pic) VALUES (NULL)", vec![]),
        ]
    );
    assert_eq!(
        render_sql(&b),
        "INSERT INTO app.\"My T\" (id, title) VALUES (8, 'a'::pg_catalog.\"varchar\");\nINSERT INTO app.\"My T\" DEFAULT VALUES;\nINSERT INTO app.\"My T\" (pic) VALUES (NULL);"
    );
}

#[test]
fn postgres_locator_keys_cast_to_tid_and_xid_under_only() {
    let k = || key(&[("ctid", t("(0,1)")), ("xmin", t("750"))]);
    let b = batch(
        pg_locator(),
        vec![
            Change::Update {
                key: k(),
                set: pairs(&[("a", t("5"))]),
            },
            Change::Delete { key: k() },
            Change::Insert {
                values: pairs(&[("a", t("1"))]),
            },
        ],
    );
    assert_eq!(
        bound(&b),
        vec![
            stmt(
                "UPDATE ONLY public.t SET a = $1::text::pg_catalog.int4 WHERE ctid = $2::text::pg_catalog.tid AND xmin = $3::text::pg_catalog.xid \
                 RETURNING CASE WHEN ctid IS NULL THEN NULL ELSE pg_catalog.format('%s', ctid) END, \
                 CASE WHEN xmin IS NULL THEN NULL ELSE pg_catalog.format('%s', xmin) END",
                vec![p("5"), p("(0,1)"), p("750")],
            ),
            stmt(
                "DELETE FROM ONLY public.t WHERE ctid = $1::text::pg_catalog.tid AND xmin = $2::text::pg_catalog.xid",
                vec![p("(0,1)"), p("750")],
            ),
            stmt(
                "INSERT INTO public.t (a) VALUES ($1::text::pg_catalog.int4)",
                vec![p("1")]
            ),
        ]
    );
}

#[test]
fn sqlite_default_is_the_declared_expression_or_null_and_blobs_stay_blobs() {
    let b = batch(
        lite(),
        vec![
            Change::Update {
                key: key(&[("id", t("3"))]),
                set: pairs(&[
                    ("title", EditValue::Default),
                    ("n", EditValue::Default),
                    ("pic", EditValue::Bytes(vec![0x0a])),
                ]),
            },
            Change::Insert {
                values: pairs(&[("title", EditValue::Default), ("n", t("it's"))]),
            },
            Change::Insert { values: vec![] },
            Change::Delete {
                key: key(&[("id", t("3"))]),
            },
        ],
    );
    assert_eq!(
        bound(&b),
        vec![
            stmt(
                "UPDATE main.\"order items\" SET title = ('x''y'), n = NULL, pic = ?1 WHERE id = ?2",
                vec![Param::Bytes(vec![0x0a]), p("3")],
            ),
            stmt(
                "INSERT INTO main.\"order items\" (n) VALUES (?1)",
                vec![p("it's")]
            ),
            stmt("INSERT INTO main.\"order items\" DEFAULT VALUES", vec![]),
            stmt(
                "DELETE FROM main.\"order items\" WHERE id = ?1",
                vec![p("3")]
            ),
        ]
    );
    assert_eq!(
        render_sql(&b),
        "UPDATE main.\"order items\" SET title = ('x''y'), n = NULL, pic = X'0a' WHERE id = 3;\nINSERT INTO main.\"order items\" (n) VALUES ('it''s');\nINSERT INTO main.\"order items\" DEFAULT VALUES;\nDELETE FROM main.\"order items\" WHERE id = 3;"
    );
}

#[test]
fn null_keys_match_with_is_null() {
    let b = batch(
        lite(),
        vec![Change::Delete {
            key: key(&[("id", EditValue::Null)]),
        }],
    );
    assert_eq!(
        bound(&b),
        vec![stmt(
            "DELETE FROM main.\"order items\" WHERE id IS NULL",
            vec![]
        )]
    );
    assert_eq!(
        render_sql(&b),
        "DELETE FROM main.\"order items\" WHERE id IS NULL;"
    );
    let b = batch(
        pg(),
        vec![Change::Update {
            key: key(&[("id", EditValue::Null)]),
            set: pairs(&[("title", t("x"))]),
        }],
    );
    assert_eq!(
        bound(&b),
        vec![stmt(
            "UPDATE app.\"My T\" SET title = $1::text::pg_catalog.\"varchar\" WHERE id IS NULL",
            vec![p("x")],
        )]
    );
}

#[test]
fn sqlite_untyped_keys_also_match_stored_numbers() {
    let np = EditTarget::new(
        RelationRef::new("main", "np"),
        Engine::Sqlite,
        vec![
            col("id", "", true, None),
            col("tag", "BLOB", true, None),
            col("x", "TEXT", true, None),
        ],
        strings(&["id", "tag"]),
        vec![],
        false,
    );
    let b = batch(
        np.clone(),
        vec![Change::Update {
            key: key(&[("id", t("5")), ("tag", t("abc"))]),
            set: pairs(&[("x", t("y"))]),
        }],
    );
    assert_eq!(
        bound(&b),
        vec![stmt(
            "UPDATE main.np SET x = ?1 WHERE (id = ?2 OR (typeof(id) IN ('integer','real') AND CAST(id AS NUMERIC) = ?2)) AND (tag = ?3 OR (typeof(tag) IN ('integer','real') AND CAST(tag AS NUMERIC) = ?3))",
            vec![p("y"), p("5"), p("abc")],
        )]
    );
    assert_eq!(
        render_sql(&b),
        "UPDATE main.np SET x = 'y' WHERE (id = '5' OR (typeof(id) IN ('integer','real') AND CAST(id AS NUMERIC) = '5')) AND (tag = 'abc' OR (typeof(tag) IN ('integer','real') AND CAST(tag AS NUMERIC) = 'abc'));"
    );
    let b = batch(
        np,
        vec![Change::Delete {
            key: key(&[
                ("id", EditValue::Bytes(vec![0x0a])),
                ("tag", EditValue::Null),
            ]),
        }],
    );
    assert_eq!(
        bound(&b),
        vec![stmt(
            "DELETE FROM main.np WHERE id = ?1 AND tag IS NULL",
            vec![Param::Bytes(vec![0x0a])]
        )]
    );
    let typed = batch(
        lite(),
        vec![Change::Delete {
            key: key(&[("id", t("3"))]),
        }],
    );
    assert_eq!(
        bound(&typed),
        vec![stmt(
            "DELETE FROM main.\"order items\" WHERE id = ?1",
            vec![p("3")]
        )]
    );
}

#[test]
fn batches_with_bad_changes_are_refused() {
    let ok_delete = || Change::Delete {
        key: key(&[("id", t("1"))]),
    };
    let cases: Vec<(EditTarget, Vec<Change>, BatchError)> = vec![
        // Nothing to apply would still open a transaction that `Applied` leaves waiting.
        (pg(), vec![], BatchError::Empty),
        (
            pg_no_identity(),
            vec![Change::Delete {
                key: key(&[("a", t("1"))]),
            }],
            BatchError::NoIdentity { change: 0 },
        ),
        (
            pg(),
            vec![Change::Update {
                key: key(&[("title", t("x"))]),
                set: pairs(&[("title", t("y"))]),
            }],
            BatchError::KeyMismatch {
                change: 0,
                expected: strings(&["id"]),
            },
        ),
        (
            pg_locator(),
            vec![Change::Delete {
                key: key(&[("xmin", t("1")), ("ctid", t("(0,1)"))]),
            }],
            BatchError::KeyMismatch {
                change: 0,
                expected: strings(&["ctid", "xmin"]),
            },
        ),
        (
            pg(),
            vec![Change::Delete {
                key: key(&[("id", EditValue::Default)]),
            }],
            BatchError::DefaultInKey { change: 0 },
        ),
        (
            pg(),
            vec![Change::Update {
                key: key(&[("id", t("1"))]),
                set: pairs(&[("nope", t("x"))]),
            }],
            BatchError::UnknownColumn {
                change: 0,
                column: "nope".to_owned(),
            },
        ),
        (
            pg_locator(),
            vec![Change::Insert {
                values: pairs(&[("ctid", t("(0,1)"))]),
            }],
            BatchError::UnknownColumn {
                change: 0,
                column: "ctid".to_owned(),
            },
        ),
        (
            pg(),
            vec![Change::Update {
                key: key(&[("id", t("1"))]),
                set: pairs(&[("title", t("a")), ("title", t("b"))]),
            }],
            BatchError::DuplicateColumn {
                change: 0,
                column: "title".to_owned(),
            },
        ),
        (
            pg(),
            vec![
                ok_delete(),
                Change::Update {
                    key: key(&[("id", t("1"))]),
                    set: pairs(&[("g", t("1"))]),
                },
            ],
            BatchError::EmptyUpdate { change: 1 },
        ),
        (
            pg(),
            vec![Change::Update {
                key: key(&[("id", t("1"))]),
                set: vec![],
            }],
            BatchError::EmptyUpdate { change: 0 },
        ),
    ];
    for (target, changes, want) in cases {
        assert_eq!(EditBatch::new(target, changes).unwrap_err(), want);
    }
    assert!(
        EditBatch::new(
            pg_no_identity(),
            vec![Change::Insert {
                values: pairs(&[("a", t("1"))])
            }]
        )
        .is_ok()
    );
}

fn typed_pg() -> EditTarget {
    EditTarget::new(
        RelationRef::new("public", "lit"),
        Engine::Postgres,
        vec![
            col("id", "pg_catalog.int8", true, None),
            col("a", "pg_catalog.int2", true, None),
            col("n", "pg_catalog.\"numeric\"", true, None),
            col("f", "pg_catalog.float8", true, None),
            col("ok", "pg_catalog.bool", true, None),
            col("s", "pg_catalog.text", true, None),
        ],
        strings(&["id"]),
        vec![],
        false,
    )
}

#[test]
fn postgres_renders_integers_numerics_and_booleans_bare_and_keeps_the_rest_cast() {
    let b = batch(
        typed_pg(),
        vec![
            Change::Update {
                key: key(&[("id", t("1"))]),
                set: pairs(&[
                    ("a", t("-7")),
                    ("n", t("10.25")),
                    ("f", t("0.1")),
                    ("ok", t("t")),
                    ("s", t("4")),
                ]),
            },
            Change::Update {
                key: key(&[("id", t("2"))]),
                set: pairs(&[("a", t("1.5")), ("n", t("1e3")), ("ok", t("No"))]),
            },
            Change::Insert {
                values: pairs(&[("id", t("3")), ("ok", t("maybe"))]),
            },
        ],
    );
    assert_eq!(
        render_sql(&b),
        "UPDATE public.lit SET a = -7, n = 10.25, f = '0.1'::pg_catalog.float8, ok = true, s = '4'::pg_catalog.text WHERE id = 1;\n\
         UPDATE public.lit SET a = '1.5'::pg_catalog.int2, n = '1e3'::pg_catalog.\"numeric\", ok = false WHERE id = 2;\n\
         INSERT INTO public.lit (id, ok) VALUES (3, 'maybe'::pg_catalog.bool);"
    );
    assert_eq!(
        bound(&b)[0].sql,
        "UPDATE public.lit SET a = $1::text::pg_catalog.int2, n = $2::text::pg_catalog.\"numeric\", f = $3::text::pg_catalog.float8, ok = $4::text::pg_catalog.bool, s = $5::text::pg_catalog.text WHERE id = $6::text::pg_catalog.int8"
    );
}

#[test]
fn postgres_keeps_an_integer_minimum_cast_so_it_compares_as_the_column_type() {
    // Postgres reads `-9223372036854775808` as minus a numeric, which defeats the index.
    let b = batch(
        typed_pg(),
        vec![Change::Update {
            key: key(&[("id", t("-9223372036854775808"))]),
            set: pairs(&[("a", t("-32768"))]),
        }],
    );
    assert_eq!(
        render_sql(&b),
        "UPDATE public.lit SET a = '-32768'::pg_catalog.int2 WHERE id = '-9223372036854775808'::pg_catalog.int8;"
    );
    let b = batch(
        typed_pg(),
        vec![Change::Update {
            key: key(&[("id", t("-9223372036854775807"))]),
            set: pairs(&[("a", t("-32767"))]),
        }],
    );
    assert_eq!(
        render_sql(&b),
        "UPDATE public.lit SET a = -32767 WHERE id = -9223372036854775807;"
    );
}

#[test]
fn sqlite_renders_numbers_bare_only_where_the_column_has_numeric_affinity() {
    let target = EditTarget::new(
        RelationRef::new("main", "lit"),
        Engine::Sqlite,
        vec![
            col("id", "INTEGER", true, None),
            col("n", "BIGINT", true, None),
            col("r", "DOUBLE PRECISION", true, None),
            col("d", "DECIMAL(10,2)", true, None),
            col("fp", "FLOATING POINT", true, None),
            col("s", "VARCHAR(5)", true, None),
            col("u", "", true, None),
        ],
        strings(&["id"]),
        vec![],
        false,
    );
    let b = batch(
        target,
        vec![Change::Update {
            key: key(&[("id", t("1"))]),
            set: pairs(&[
                ("n", t("-3")),
                ("r", t("2.5")),
                ("d", t("10.75")),
                ("fp", t("2.5")),
                ("s", t("4")),
                ("u", t("5")),
            ]),
        }],
    );
    // FLOATING POINT contains INT, so it has INTEGER affinity and 2.5 stays text.
    assert_eq!(
        render_sql(&b),
        "UPDATE main.lit SET n = -3, r = 2.5, d = 10.75, fp = '2.5', s = '4', u = '5' WHERE id = 1;"
    );
}

#[test]
fn an_update_that_writes_a_key_column_returns_the_identity_but_its_preview_does_not() {
    let pg_key = batch(
        pg(),
        vec![Change::Update {
            key: key(&[("id", t("7"))]),
            set: pairs(&[("id", t("010"))]),
        }],
    );
    assert_eq!(
        bound(&pg_key),
        vec![stmt(
            "UPDATE app.\"My T\" SET id = $1::text::pg_catalog.int4 WHERE id = $2::text::pg_catalog.int4 \
             RETURNING CASE WHEN id IS NULL THEN NULL ELSE pg_catalog.format('%s', id) END",
            vec![p("010"), p("7")],
        )]
    );
    assert_eq!(
        render_sql(&pg_key),
        "UPDATE app.\"My T\" SET id = 010 WHERE id = 7;"
    );
    let lite_key = batch(
        lite(),
        vec![Change::Update {
            key: key(&[("id", t("7"))]),
            set: pairs(&[("id", EditValue::Null)]),
        }],
    );
    assert_eq!(
        bound(&lite_key),
        vec![stmt(
            "UPDATE main.\"order items\" SET id = NULL WHERE id = ?1 RETURNING id",
            vec![p("7")],
        )]
    );
    let no_key = batch(
        lite(),
        vec![Change::Update {
            key: key(&[("id", t("7"))]),
            set: pairs(&[("title", t("x"))]),
        }],
    );
    assert!(!bound(&no_key)[0].returning);
}

// The commit prompt shortens built-in casts for reading; what runs keeps them qualified, so a
// type of the same name earlier on the search_path can never capture a value.
#[test]
fn the_executed_statement_keeps_its_qualified_casts() {
    let target = EditTarget::new(
        RelationRef::new("public", "t"),
        Engine::Postgres,
        vec![col("s", "pg_catalog.text", true, None)],
        strings(&["ctid", "xmin"]),
        strings(&["ctid", "xmin"]),
        true,
    );
    let k = key(&[("ctid", t("(0,2)")), ("xmin", t("750"))]);
    let b = batch(
        target,
        vec![Change::Update {
            key: k,
            set: pairs(&[("s", t("a"))]),
        }],
    );
    let run = &bound(&b)[0];
    assert!(
        run.sql.starts_with(
            "UPDATE ONLY public.t SET s = $1::text::pg_catalog.text \
             WHERE ctid = $2::text::pg_catalog.tid AND xmin = $3::text::pg_catalog.xid"
        ),
        "{}",
        run.sql
    );
    assert_eq!(
        render_sql(&b),
        "UPDATE ONLY public.t SET s = 'a'::pg_catalog.text \
         WHERE ctid = '(0,2)'::pg_catalog.tid AND xmin = '750'::pg_catalog.xid;"
    );
}
