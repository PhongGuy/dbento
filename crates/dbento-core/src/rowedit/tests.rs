use super::sql::{BoundStatement, Dialect, Param, bound};
use super::target::mysql_identity;
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

fn ms() -> EditTarget {
    EditTarget::new(
        RelationRef::new("dbo", "t"),
        Engine::Mssql,
        vec![col("id", "int", true, None)],
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
        engine: Engine::Mssql,
    };
    assert_eq!(EditBatch::new(ms(), insert()).unwrap_err(), refused);
    assert_eq!(
        EditBatch::new(ms(), vec![]).unwrap_err(),
        refused,
        "the engine is checked before anything else"
    );
    assert_eq!(
        refused.to_string(),
        "editing rows is not available on mssql yet"
    );
    for target in [pg(), lite(), my(), ms()] {
        let engine = target.engine();
        assert_eq!(
            EditBatch::new(target, insert()).is_ok(),
            engine.capabilities().edit_rows,
            "{engine:?}"
        );
    }
    assert!(Engine::Mysql.capabilities().edit_rows, "MySQL edits rows");
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
        reread: None,
        guard: None,
        expected: vec![],
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
            expected: vec![],
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
            expected: vec![],
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
                expected: vec![],
            },
            Change::Delete {
                key: k(),
                expected: vec![],
            },
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
                expected: vec![],
            },
            Change::Insert {
                values: pairs(&[("title", EditValue::Default), ("n", t("it's"))]),
            },
            Change::Insert { values: vec![] },
            Change::Delete {
                key: key(&[("id", t("3"))]),
                expected: vec![],
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
            expected: vec![],
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
            expected: vec![],
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
            expected: vec![],
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
            expected: vec![],
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
            expected: vec![],
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
        expected: vec![],
    };
    let cases: Vec<(EditTarget, Vec<Change>, BatchError)> = vec![
        // Nothing to apply would still open a transaction that `Applied` leaves waiting.
        (pg(), vec![], BatchError::Empty),
        (
            pg_no_identity(),
            vec![Change::Delete {
                key: key(&[("a", t("1"))]),
                expected: vec![],
            }],
            BatchError::NoIdentity { change: 0 },
        ),
        (
            pg(),
            vec![Change::Update {
                key: key(&[("title", t("x"))]),
                set: pairs(&[("title", t("y"))]),
                expected: vec![],
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
                expected: vec![],
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
                expected: vec![],
            }],
            BatchError::DefaultInKey { change: 0 },
        ),
        (
            pg(),
            vec![Change::Update {
                key: key(&[("id", t("1"))]),
                set: pairs(&[("nope", t("x"))]),
                expected: vec![],
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
                expected: vec![],
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
                    expected: vec![],
                },
            ],
            BatchError::EmptyUpdate { change: 1 },
        ),
        (
            pg(),
            vec![Change::Update {
                key: key(&[("id", t("1"))]),
                set: vec![],
                expected: vec![],
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
                expected: vec![],
            },
            Change::Update {
                key: key(&[("id", t("2"))]),
                set: pairs(&[("a", t("1.5")), ("n", t("1e3")), ("ok", t("No"))]),
                expected: vec![],
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
            expected: vec![],
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
            expected: vec![],
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
            expected: vec![],
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
            expected: vec![],
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
            expected: vec![],
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
            expected: vec![],
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
            expected: vec![],
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

fn my_full() -> EditTarget {
    EditTarget::new(
        RelationRef::new("shop", "t`x"),
        Engine::Mysql,
        vec![
            col("id", "int", true, None),
            col("title", "varchar(10)", true, None),
            col("pic", "varbinary(16)", true, None),
            col("g", "int", false, None),
        ],
        strings(&["id"]),
        vec![],
        false,
    )
}

fn my_pair() -> EditTarget {
    EditTarget::new(
        RelationRef::new("shop", "p"),
        Engine::Mysql,
        vec![
            col("a", "int", true, None),
            col("b", "varbinary(4)", true, None),
            col("n", "int", true, None),
        ],
        strings(&["a", "b"]),
        vec![],
        false,
    )
}

/// A MySQL batch built directly, so these generator tests do not depend on the capability flag.
fn my_batch(target: EditTarget, changes: Vec<Change>) -> EditBatch {
    EditBatch {
        target,
        changes,
        dialect: Dialect::Mysql,
    }
}

#[test]
fn mysql_binds_question_marks_with_bytes_through_unhex_and_renders_literals() {
    let b = my_batch(
        my_full(),
        vec![Change::Update {
            key: key(&[("id", t("7"))]),
            set: pairs(&[
                ("title", t("it's \\ ok")),
                ("pic", EditValue::Bytes(vec![0x00, 0xff])),
            ]),
            expected: vec![],
        }],
    );
    assert_eq!(
        bound(&b),
        vec![stmt(
            "UPDATE `shop`.`t``x` SET `title` = ?, `pic` = UNHEX(?) WHERE `id` = ?",
            vec![p("it's \\ ok"), p("00ff"), p("7")],
        )]
    );
    assert_eq!(
        render_sql(&b),
        "UPDATE `shop`.`t``x` SET `title` = CONVERT(X'69742773205c206f6b' USING utf8mb4), `pic` = X'00ff' WHERE `id` = '7';"
    );
}

#[test]
fn mysql_inserts_name_given_columns_and_an_all_default_row_is_two_empty_lists() {
    let b = my_batch(
        my_full(),
        vec![
            Change::Insert {
                values: pairs(&[
                    ("id", t("1")),
                    ("title", EditValue::Default),
                    ("pic", EditValue::Null),
                ]),
            },
            Change::Insert {
                values: pairs(&[("title", EditValue::Default)]),
            },
            Change::Update {
                key: key(&[("id", t("3"))]),
                set: pairs(&[("title", EditValue::Default)]),
                expected: vec![],
            },
            Change::Delete {
                key: key(&[("id", t("2"))]),
                expected: vec![],
            },
        ],
    );
    assert_eq!(
        bound(&b),
        vec![
            stmt(
                "INSERT INTO `shop`.`t``x` (`id`, `pic`) VALUES (?, NULL)",
                vec![p("1")]
            ),
            stmt("INSERT INTO `shop`.`t``x` () VALUES ()", vec![]),
            stmt(
                "UPDATE `shop`.`t``x` SET `title` = DEFAULT WHERE `id` = ?",
                vec![p("3")]
            ),
            stmt("DELETE FROM `shop`.`t``x` WHERE `id` = ?", vec![p("2")]),
        ]
    );
    assert_eq!(
        render_sql(&b),
        "INSERT INTO `shop`.`t``x` (`id`, `pic`) VALUES ('1', NULL);\n\
         INSERT INTO `shop`.`t``x` () VALUES ();\n\
         UPDATE `shop`.`t``x` SET `title` = DEFAULT WHERE `id` = '3';\n\
         DELETE FROM `shop`.`t``x` WHERE `id` = '2';"
    );
}

#[test]
fn a_mysql_key_update_re_reads_the_new_key_as_text_and_bytes() {
    let b = my_batch(
        my_pair(),
        vec![
            Change::Update {
                key: key(&[("a", t("1")), ("b", EditValue::Bytes(vec![1]))]),
                set: pairs(&[("a", t("010"))]),
                expected: vec![],
            },
            Change::Update {
                key: key(&[("a", t("2")), ("b", EditValue::Bytes(vec![2]))]),
                set: pairs(&[("n", t("5"))]),
                expected: vec![],
            },
            Change::Update {
                key: key(&[("a", t("3")), ("b", EditValue::Bytes(vec![3]))]),
                set: pairs(&[("a", EditValue::Default)]),
                expected: vec![],
            },
        ],
    );
    let s = bound(&b);
    assert_eq!(
        s[0].sql,
        "UPDATE `shop`.`p` SET `a` = ? WHERE `a` = ? AND `b` = UNHEX(?)"
    );
    assert_eq!(s[0].params, vec![p("010"), p("1"), p("01")]);
    assert!(!s[0].returning, "MySQL has no RETURNING");
    let reread = BoundStatement {
        sql: "SELECT CAST(`a` AS CHAR), `b` FROM `shop`.`p` WHERE `a` = ? AND `b` = UNHEX(?)"
            .to_owned(),
        params: vec![p("010"), p("01")],
        returning: true,
        reread: None,
        guard: None,
        expected: vec![],
    };
    assert_eq!(s[0].reread.as_deref(), Some(&reread));
    assert_eq!(s[1].reread, None, "the key was not written");
    assert_eq!(s[2].reread, None, "a DEFAULT key cannot be matched");
}

#[test]
fn a_mysql_browse_names_its_locators_before_the_qualified_star() {
    let located = EditTarget::new(
        RelationRef::new("shop", "g"),
        Engine::Mysql,
        vec![col("a", "int", true, None)],
        strings(&["my_row_id"]),
        strings(&["my_row_id"]),
        false,
    );
    assert_eq!(
        browse_sql(&located, &Browse::default()).unwrap(),
        "SELECT `my_row_id`, `shop`.`g`.*\nFROM `shop`.`g`\nORDER BY `my_row_id` ASC"
    );
    assert_eq!(
        browse_sql(&my_full(), &Browse::default()).unwrap(),
        "SELECT *\nFROM `shop`.`t``x`\nORDER BY `id` ASC"
    );
}

fn meta(name: &str, ty: &str, nullable: bool) -> ColumnMeta {
    ColumnMeta::new(name, ty, nullable, None, None)
}

fn ix(name: &str, unique: bool, parts: &[Option<&str>]) -> (String, bool, Vec<Option<String>>) {
    (
        name.to_owned(),
        unique,
        parts.iter().map(|p| p.map(str::to_owned)).collect(),
    )
}

#[test]
fn mysql_identity_takes_the_primary_key_then_the_first_usable_unique_key() {
    let cols = [
        meta("id", "int", false),
        meta("f", "float", false),
        meta("b", "bit(8)", false),
        meta("u", "int", false),
        meta("v", "varchar(5)", false),
        meta("n", "int", true),
        meta("d", "double", false),
    ];
    let pk = ix("PRIMARY", true, &[Some("id")]);
    assert_eq!(
        mysql_identity(&cols, &[ix("a", true, &[Some("u")]), pk]),
        ["id"]
    );
    let skipped = vec![
        ix("a_plain", false, &[Some("u")]),
        ix("b_null", true, &[Some("n")]),
        ix("c_fn", true, &[None]),
        ix("d_float", true, &[Some("u"), Some("f")]),
        ix("e_bit", true, &[Some("b")]),
    ];
    assert!(
        mysql_identity(&cols, &skipped).is_empty(),
        "every candidate is unusable"
    );
    let mut with_last = skipped.clone();
    with_last.push(ix("f_ok", true, &[Some("v"), Some("u")]));
    assert_eq!(mysql_identity(&cols, &with_last), ["v", "u"]);
    let float_pk = [
        ix("PRIMARY", true, &[Some("f")]),
        ix("z", true, &[Some("d")]),
    ];
    assert_eq!(
        mysql_identity(&cols, &float_pk),
        ["d"],
        "a FLOAT key falls through; DOUBLE is kept"
    );
    assert!(mysql_identity(&cols, &[]).is_empty(), "no index");
}

#[test]
fn not_editable_reasons_read_as_the_tail_of_a_sentence() {
    let says = |r: NotEditable| r.to_string();
    assert_eq!(
        says(NotEditable::NoKey),
        "the table has no primary key or NOT NULL unique key that can identify a row"
    );
    assert_eq!(says(NotEditable::View), "views are not edited");
    assert_eq!(says(NotEditable::Sequence), "sequences are not edited");
    assert_eq!(
        says(NotEditable::NonTransactional {
            engine: "MyISAM".to_owned()
        }),
        "MyISAM tables cannot roll back a change"
    );
    assert_eq!(
        says(NotEditable::SystemVersioned),
        "system-versioned tables are not edited"
    );
    assert_eq!(
        pg().not_editable(),
        None,
        "only MySQL targets carry a reason"
    );
}

#[test]
fn a_target_with_a_not_editable_reason_refuses_every_change() {
    let mut refused = my();
    refused.not_editable = Some(NotEditable::NonTransactional {
        engine: "MyISAM".to_owned(),
    });
    let insert = Change::Insert {
        values: pairs(&[("id", t("1"))]),
    };
    let update = Change::Update {
        key: key(&[("id", t("1"))]),
        set: pairs(&[("id", t("2"))]),
        expected: vec![],
    };
    let expected = BatchError::NotEditable {
        reason: NotEditable::NonTransactional {
            engine: "MyISAM".to_owned(),
        },
    };
    for change in [insert, update] {
        assert_eq!(
            EditBatch::new(refused.clone(), vec![change]).unwrap_err(),
            expected,
            "an all-default insert must not reach a table that cannot roll back"
        );
    }
    assert_eq!(
        expected.to_string(),
        "the table cannot be edited: MyISAM tables cannot roll back a change"
    );
}

#[test]
fn the_apply_session_is_always_strict() {
    use super::apply::strict_sql_mode;
    assert_eq!(strict_sql_mode(""), "STRICT_TRANS_TABLES");
    assert_eq!(
        strict_sql_mode("ANSI_QUOTES"),
        "ANSI_QUOTES,STRICT_TRANS_TABLES"
    );
    assert_eq!(
        strict_sql_mode("STRICT_ALL_TABLES"),
        "STRICT_ALL_TABLES,STRICT_TRANS_TABLES"
    );
    assert_eq!(
        strict_sql_mode("XSTRICT_TRANS_TABLES"),
        "XSTRICT_TRANS_TABLES,STRICT_TRANS_TABLES",
        "whole modes only"
    );
    let mysql = "ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES,NO_ZERO_DATE";
    assert_eq!(strict_sql_mode(mysql), mysql, "already strict");
}

#[test]
fn a_warning_fails_a_change_before_its_rows_count_and_a_note_does_not() {
    use super::apply::change_outcome;
    let w = |level: &str, message: &str| (level.to_owned(), message.to_owned());
    assert_eq!(change_outcome(1, &[]), ChangeOutcome::Applied(1));
    assert_eq!(change_outcome(0, &[]), ChangeOutcome::Conflict);
    assert_eq!(change_outcome(2, &[]), ChangeOutcome::Ambiguous(2));
    let note = [w("Note", "Data truncated for column 'dc' at row 1")];
    assert_eq!(
        change_outcome(1, &note),
        ChangeOutcome::Applied(1),
        "a note is not a failure"
    );
    let mixed = [w("Warning", "a"), w("Note", "n"), w("Error", "b")];
    assert_eq!(
        change_outcome(1, &mixed),
        ChangeOutcome::Failed("a; b".to_owned()),
        "no re-read follows"
    );
    assert_eq!(
        change_outcome(0, &[w("Warning", "a")]),
        ChangeOutcome::Failed("a".to_owned()),
        "a warning outranks a miss"
    );
}

#[test]
fn expected_values_are_validated_and_kept_for_every_column() {
    let update = |expected: Vec<(String, EditValue)>| Change::Update {
        key: key(&[("id", t("1"))]),
        set: pairs(&[("title", t("new"))]),
        expected,
    };
    let delete = |expected: Vec<(String, EditValue)>| Change::Delete {
        key: key(&[("id", t("1"))]),
        expected,
    };
    // A generated column is dropped from `set` but kept in `expected`.
    let b = batch(
        pg(),
        vec![
            update(pairs(&[("title", t("old")), ("g", t("2"))])),
            delete(pairs(&[
                ("id", t("1")),
                ("title", EditValue::Null),
                ("pic", EditValue::Bytes(vec![1])),
                ("g", t("2")),
            ])),
            update(vec![]),
        ],
    );
    assert_eq!(
        b.changes()[0],
        Change::Update {
            key: key(&[("id", t("1"))]),
            set: pairs(&[("title", t("new"))]),
            expected: pairs(&[("title", t("old")), ("g", t("2"))]),
        }
    );
    match &b.changes()[1] {
        Change::Delete { expected, .. } => assert_eq!(expected.len(), 4),
        other @ (Change::Insert { .. } | Change::Update { .. }) => panic!("{other:?}"),
    }
    assert_eq!(
        EditBatch::new(pg(), vec![update(pairs(&[("nope", t("x"))]))]).unwrap_err(),
        BatchError::UnknownColumn {
            change: 0,
            column: "nope".to_owned()
        }
    );
    let located = Change::Delete {
        key: key(&[("ctid", t("(0,1)")), ("xmin", t("750"))]),
        expected: pairs(&[("ctid", t("(0,1)"))]),
    };
    assert_eq!(
        EditBatch::new(pg_locator(), vec![located]).unwrap_err(),
        BatchError::UnknownColumn {
            change: 0,
            column: "ctid".to_owned()
        }
    );
    assert_eq!(
        EditBatch::new(
            pg(),
            vec![
                update(vec![]),
                delete(pairs(&[("title", t("a")), ("title", t("b"))]))
            ]
        )
        .unwrap_err(),
        BatchError::DuplicateColumn {
            change: 1,
            column: "title".to_owned()
        }
    );
    assert_eq!(
        EditBatch::new(pg(), vec![update(pairs(&[("title", EditValue::Default)]))]).unwrap_err(),
        BatchError::DefaultExpected { change: 0 }
    );
    assert_eq!(
        BatchError::DefaultExpected { change: 2 }.to_string(),
        "change 2: an expected value cannot be DEFAULT"
    );
}

#[test]
fn a_guard_passes_only_one_row_holding_exactly_the_expected_values() {
    use super::apply::guard_outcome;
    let exp = vec![t("a"), EditValue::Null, EditValue::Bytes(vec![0xff])];
    assert_eq!(guard_outcome(&[], &exp), Some(ChangeOutcome::Conflict));
    assert_eq!(
        guard_outcome(&[exp.clone(), exp.clone()], &exp),
        Some(ChangeOutcome::Ambiguous(2))
    );
    assert_eq!(guard_outcome(std::slice::from_ref(&exp), &exp), None);
    let as_bytes = vec![
        EditValue::Bytes(b"a".to_vec()),
        EditValue::Null,
        EditValue::Bytes(vec![0xff]),
    ];
    assert_eq!(
        guard_outcome(&[as_bytes], &exp),
        None,
        "text compares as its bytes"
    );
    for changed in [
        vec![t("A"), EditValue::Null, EditValue::Bytes(vec![0xff])],
        vec![t("a "), EditValue::Null, EditValue::Bytes(vec![0xff])],
        vec![t("a"), t(""), EditValue::Bytes(vec![0xff])],
        vec![
            EditValue::Null,
            EditValue::Null,
            EditValue::Bytes(vec![0xff]),
        ],
        vec![t("a"), EditValue::Null, EditValue::Bytes(vec![0xfe])],
    ] {
        assert_eq!(
            guard_outcome(std::slice::from_ref(&changed), &exp),
            Some(ChangeOutcome::Conflict),
            "{changed:?}"
        );
    }
}

#[test]
fn a_sqlite_guard_reads_the_expected_columns_by_the_writes_key() {
    let b = batch(
        lite(),
        vec![
            Change::Update {
                key: key(&[("id", t("3"))]),
                set: pairs(&[("title", t("new"))]),
                expected: pairs(&[("title", t("old")), ("pic", EditValue::Bytes(vec![1]))]),
            },
            Change::Delete {
                key: key(&[("id", t("4"))]),
                expected: pairs(&[("n", EditValue::Null)]),
            },
            Change::Update {
                key: key(&[("id", t("5"))]),
                set: pairs(&[("n", t("1"))]),
                expected: vec![],
            },
        ],
    );
    let s = bound(&b);
    let read = |sql: &str, params: Vec<Param>| {
        Some(Box::new(BoundStatement {
            sql: sql.to_owned(),
            params,
            returning: false,
            reread: None,
            guard: None,
            expected: vec![],
        }))
    };
    assert_eq!(
        s[0].guard,
        read(
            "SELECT title, pic FROM main.\"order items\" WHERE id = ?1",
            vec![p("3")]
        )
    );
    assert_eq!(s[0].expected, vec![t("old"), EditValue::Bytes(vec![1])]);
    assert_eq!(
        s[0].sql,
        "UPDATE main.\"order items\" SET title = ?1 WHERE id = ?2"
    );
    assert_eq!(
        s[1].guard,
        read(
            "SELECT n FROM main.\"order items\" WHERE id = ?1",
            vec![p("4")]
        )
    );
    assert_eq!(s[1].expected, vec![EditValue::Null]);
    assert_eq!(s[2].guard, None);
    assert!(s[2].expected.is_empty());
    assert!(
        !render_sql(&b).contains("SELECT"),
        "the preview shows the writes only"
    );
}

#[test]
fn a_postgres_update_guard_locks_the_row_and_reads_output_text() {
    let fmt = |c: &str| {
        format!(
            "CASE WHEN pg_catalog.num_nonnulls({c}) = 0 THEN NULL ELSE pg_catalog.format('%s', {c}) END"
        )
    };
    let b = batch(
        pg(),
        vec![
            Change::Update {
                key: key(&[("id", t("7"))]),
                set: pairs(&[("title", t("new"))]),
                expected: pairs(&[("title", t("old")), ("pic", t("\\x01"))]),
            },
            Change::Update {
                key: key(&[("id", t("7"))]),
                set: pairs(&[("id", t("8"))]),
                expected: pairs(&[("id", t("7"))]),
            },
        ],
    );
    let s = bound(&b);
    let g0 = s[0].guard.as_deref().unwrap();
    assert_eq!(
        g0.sql,
        format!(
            "SELECT {}, {} FROM app.\"My T\" WHERE id = $1::text::pg_catalog.int4 FOR NO KEY UPDATE",
            fmt("title"),
            fmt("pic")
        )
    );
    assert_eq!(g0.params, vec![p("7")]);
    assert_eq!(s[0].expected, vec![t("old"), t("\\x01")]);
    assert_eq!(
        s[1].guard.as_deref().unwrap().sql,
        format!(
            "SELECT {} FROM app.\"My T\" WHERE id = $1::text::pg_catalog.int4 FOR UPDATE",
            fmt("id")
        )
    );
    let located = batch(
        pg_locator(),
        vec![Change::Update {
            key: key(&[("ctid", t("(0,1)")), ("xmin", t("750"))]),
            set: pairs(&[("a", t("5"))]),
            expected: pairs(&[("a", t("4"))]),
        }],
    );
    assert_eq!(
        bound(&located)[0].guard.as_deref().unwrap().sql,
        format!(
            "SELECT {} FROM ONLY public.t WHERE ctid = $1::text::pg_catalog.tid AND xmin = $2::text::pg_catalog.xid FOR NO KEY UPDATE",
            fmt("a")
        )
    );
}

#[test]
fn a_postgres_delete_returns_its_guarded_columns_instead_of_locking_first() {
    let fmt = |c: &str| {
        format!(
            "CASE WHEN pg_catalog.num_nonnulls({c}) = 0 THEN NULL ELSE pg_catalog.format('%s', {c}) END"
        )
    };
    let b = batch(
        pg(),
        vec![
            Change::Delete {
                key: key(&[("id", t("7"))]),
                expected: pairs(&[("id", t("7")), ("g", EditValue::Null)]),
            },
            Change::Delete {
                key: key(&[("id", t("8"))]),
                expected: vec![],
            },
        ],
    );
    let s = bound(&b);
    assert_eq!(
        s[0].sql,
        format!(
            "DELETE FROM app.\"My T\" WHERE id = $1::text::pg_catalog.int4 RETURNING {}, {}",
            fmt("id"),
            fmt("g")
        )
    );
    assert_eq!(s[0].guard, None);
    assert!(!s[0].returning, "no written key is read back");
    assert_eq!(s[0].expected, vec![t("7"), EditValue::Null]);
    assert_eq!(
        s[1].sql,
        "DELETE FROM app.\"My T\" WHERE id = $1::text::pg_catalog.int4"
    );
    assert!(s[1].expected.is_empty());
    assert_eq!(
        render_sql(&b),
        "DELETE FROM app.\"My T\" WHERE id = 7;\nDELETE FROM app.\"My T\" WHERE id = 8;"
    );
}

#[test]
fn a_mysql_guard_casts_text_columns_keeps_byte_columns_raw_and_locks() {
    let b = my_batch(
        my_full(),
        vec![
            Change::Update {
                key: key(&[("id", t("7"))]),
                set: pairs(&[("title", t("new"))]),
                expected: pairs(&[
                    ("title", t("old")),
                    ("pic", EditValue::Bytes(vec![1])),
                    ("g", EditValue::Null),
                ]),
            },
            Change::Delete {
                key: key(&[("id", t("7"))]),
                expected: pairs(&[("id", t("7"))]),
            },
        ],
    );
    let s = bound(&b);
    let g0 = s[0].guard.as_deref().unwrap();
    assert_eq!(
        g0.sql,
        "SELECT CAST(`title` AS CHAR), `pic`, CAST(`g` AS CHAR) FROM `shop`.`t``x` WHERE `id` = ? FOR UPDATE"
    );
    assert_eq!(g0.params, vec![p("7")]);
    assert_eq!(
        s[1].guard.as_deref().unwrap().sql,
        "SELECT CAST(`id` AS CHAR) FROM `shop`.`t``x` WHERE `id` = ? FOR UPDATE"
    );
    assert_eq!(s[1].sql, "DELETE FROM `shop`.`t``x` WHERE `id` = ?");
}

#[test]
fn mysql_byte_typed_columns_are_read_raw_and_the_rest_as_char() {
    let raw = [
        "bit(8)",
        "geometry",
        "point",
        "linestring",
        "polygon",
        "multipoint",
        "multilinestring",
        "multipolygon",
        "geometrycollection",
        "geomcollection",
        "vector(3)",
        "binary(4)",
        "varbinary(4)",
        "tinyblob",
        "blob",
        "mediumblob",
        "longblob",
    ];
    let cast = [
        "varchar(3)",
        "char(3)",
        "text",
        "json",
        "float",
        "double",
        "decimal(5,2)",
        "timestamp(6)",
        "year",
        "enum('a','b')",
        "set('a')",
        "int unsigned",
    ];
    let cases = raw
        .iter()
        .map(|ty| (ty, true))
        .chain(cast.iter().map(|ty| (ty, false)));
    for (ty, want_raw) in cases {
        let target = EditTarget::new(
            RelationRef::new("s", "t"),
            Engine::Mysql,
            vec![col("id", "int", true, None), col("c", ty, true, None)],
            strings(&["id"]),
            vec![],
            false,
        );
        let b = my_batch(
            target,
            vec![Change::Delete {
                key: key(&[("id", t("1"))]),
                expected: pairs(&[("c", EditValue::Null)]),
            }],
        );
        let sql = bound(&b)[0].guard.as_deref().unwrap().sql.clone();
        let want = if want_raw {
            "SELECT `c` FROM"
        } else {
            "SELECT CAST(`c` AS CHAR) FROM"
        };
        assert!(sql.starts_with(want), "{ty}: {sql}");
    }
}

#[tokio::test]
async fn reread_refuses_an_engine_that_cannot_edit_rows_before_connecting() {
    // The database file does not exist, so reaching connect would give a different error.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"missing.db\"\n",
    )
    .unwrap();
    let ws = crate::workspace::Workspace::open(dir.path()).unwrap();
    let conn = ws.declared("lite").unwrap();
    let err = reread(
        &ws,
        conn,
        None,
        crate::session::SessionOptions::default(),
        &ms(),
        &[key(&[("id", t("1"))])],
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            err,
            RereadError::UnsupportedEngine {
                engine: Engine::Mssql
            }
        ),
        "{err:?}"
    );
}
