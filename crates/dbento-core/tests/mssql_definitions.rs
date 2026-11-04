//! SQL Server schema objects and definitions: stored text as the server keeps it, composed text
//! marked as composed, and a refusal where the server stores none.
mod support;

use dbento_core::{
    ColumnMeta, ConstraintKind, DefinitionError, ObjectRef, RelationKind, RelationRef, RoutineKind,
    Session, SessionOptions, TypeKind,
};
use support::{MsFixture, ms_fixture, ms_off, ms_spid};

async fn setup(fx: &MsFixture, batches: &[&str]) {
    let mut c = fx.db.client().await;
    for b in batches {
        c.simple_query(*b)
            .await
            .unwrap()
            .into_results()
            .await
            .unwrap();
    }
}

const SHOP: &[&str] = &[
    "CREATE SCHEMA sales",
    "CREATE TYPE sales.phone FROM varchar(20) NOT NULL",
    "CREATE TYPE sales.tt AS TABLE (a int)",
    "CREATE TABLE sales.orders (id int PRIMARY KEY, amount decimal(10,2) NOT NULL, \
     CONSTRAINT ck_amt CHECK (amount >= 0))",
    "CREATE INDEX ix_a ON sales.orders (amount)",
    "CREATE VIEW sales.v AS SELECT id FROM sales.orders",
    "CREATE SYNONYM sales.syn FOR sales.orders",
    "CREATE TRIGGER sales.tr ON sales.orders AFTER INSERT AS RETURN",
    "CREATE FUNCTION sales.f_tax (@a decimal(10,2), @b nvarchar(max) = N'x') \
     RETURNS decimal(12,4) AS BEGIN RETURN @a * 1.25 END",
    "CREATE FUNCTION sales.f_big (@min int) RETURNS TABLE AS \
     RETURN SELECT id FROM sales.orders WHERE amount >= @min",
    "CREATE FUNCTION sales.f_multi () RETURNS @t TABLE (x int) AS BEGIN RETURN END",
    "CREATE PROCEDURE sales.p_none AS SELECT 1 AS one",
    "CREATE PROCEDURE sales.p_orders @customer int, @out int OUTPUT, @ph sales.phone = NULL, \
     @tv sales.tt READONLY AS SELECT 1 AS one",
    "CREATE PROCEDURE sales.p_secret WITH ENCRYPTION AS SELECT 1 AS one",
];

fn rel(name: &str) -> RelationRef {
    RelationRef::new("sales", name)
}

/// The reference `schema_objects` hands out for routine `name`; a reference is only ever listed.
async fn routine(s: &mut Session, name: &str) -> ObjectRef {
    let listed = s.schema_objects("sales").await.unwrap();
    let found = listed.routines().iter().find(|r| r.name() == name);
    found.unwrap().object_ref()
}

async fn check(s: &mut Session, name: &str) -> ObjectRef {
    let objects = s.relation_objects(&rel("orders")).await.unwrap();
    let found = objects
        .constraints()
        .iter()
        .find(|c| c.name() == Some(name));
    found.unwrap().object_ref()
}

#[tokio::test]
async fn routines_and_types_list_by_name_with_kinds_arguments_and_results() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(&fx, SHOP).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let objects = s.schema_objects("sales").await.unwrap();
    let types: Vec<_> = objects
        .types()
        .iter()
        .map(|t| (t.name().to_owned(), t.kind()))
        .collect();
    assert_eq!(
        types,
        [
            ("phone".to_owned(), TypeKind::Domain),
            ("tt".to_owned(), TypeKind::Composite),
        ]
    );
    let routines: Vec<_> = objects
        .routines()
        .iter()
        .map(|r| {
            (
                r.name().to_owned(),
                r.kind(),
                r.arguments().to_owned(),
                r.returns().map(str::to_owned),
            )
        })
        .collect();
    let row = |n: &str, k, a: &str, r: Option<&str>| {
        (n.to_owned(), k, a.to_owned(), r.map(str::to_owned))
    };
    assert_eq!(
        routines,
        [
            row("f_big", RoutineKind::Function, "@min INT", Some("TABLE")),
            row("f_multi", RoutineKind::Function, "", Some("TABLE")),
            row(
                "f_tax",
                RoutineKind::Function,
                "@a DECIMAL(10,2), @b NVARCHAR(MAX)",
                Some("DECIMAL(12,4)")
            ),
            row("p_none", RoutineKind::Procedure, "", None),
            row(
                "p_orders",
                RoutineKind::Procedure,
                "@customer INT, @out INT OUTPUT, @ph sales.phone, @tv sales.tt READONLY",
                None
            ),
            row("p_secret", RoutineKind::Procedure, "", None),
        ]
    );
    assert!(!objects.truncated());
    let none = s.schema_objects("nope").await.unwrap();
    assert!(none.types().is_empty() && none.routines().is_empty());
}

#[tokio::test]
async fn views_triggers_and_routines_answer_with_the_text_the_server_stores() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(&fx, SHOP).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let view = s.definition(&ObjectRef::relation(rel("v"))).await.unwrap();
    assert_eq!(
        view.text(),
        "CREATE VIEW sales.v AS SELECT id FROM sales.orders;"
    );
    assert!(!view.composed() && !view.truncated());
    let trigger = s.relation_objects(&rel("orders")).await.unwrap().triggers()[0].object_ref();
    assert_eq!(
        s.definition(&trigger).await.unwrap().text(),
        "CREATE TRIGGER sales.tr ON sales.orders AFTER INSERT AS RETURN;"
    );
    let none = routine(&mut s, "p_none").await;
    assert_eq!(
        s.definition(&none).await.unwrap().text(),
        "CREATE PROCEDURE sales.p_none AS SELECT 1 AS one;"
    );
    let tax = routine(&mut s, "f_tax").await;
    let text = s.definition(&tax).await.unwrap();
    assert!(text.text().starts_with("CREATE FUNCTION sales.f_tax"));
}

#[tokio::test]
async fn a_check_is_composed_and_says_so() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(&fx, SHOP).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let object = check(&mut s, "ck_amt").await;
    let text = s.definition(&object).await.unwrap();
    assert_eq!(
        text.text(),
        "ALTER TABLE [sales].[orders] ADD CONSTRAINT [ck_amt] CHECK ([amount]>=(0));"
    );
    assert!(text.composed());
}

#[tokio::test]
async fn what_the_server_stores_no_text_for_is_unsupported_and_a_gone_object_missing() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(&fx, SHOP).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let objects = s.relation_objects(&rel("orders")).await.unwrap();
    let index = objects
        .indexes()
        .iter()
        .find(|i| i.name() == "ix_a")
        .unwrap();
    let primary = objects
        .constraints()
        .iter()
        .find(|c| c.kind() == ConstraintKind::PrimaryKey)
        .unwrap();
    for object in [
        ObjectRef::relation(rel("orders")),
        index.object_ref(),
        primary.object_ref(),
        s.schema_objects("sales").await.unwrap().types()[0].object_ref(),
    ] {
        let e = s.definition(&object).await.unwrap_err();
        assert!(matches!(e, DefinitionError::Unsupported { .. }), "{e:?}");
    }
}

// Each reference was listed, then its object went away, as in a stale schema tree.
#[tokio::test]
async fn an_object_gone_since_it_was_listed_is_missing() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(&fx, SHOP).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let trigger = s.relation_objects(&rel("orders")).await.unwrap().triggers()[0].object_ref();
    let objects = [
        routine(&mut s, "p_none").await,
        routine(&mut s, "f_multi").await,
        check(&mut s, "ck_amt").await,
        trigger,
        ObjectRef::relation(rel("nope")),
    ];
    // `p_none` comes back as a table: an object of that name, but no routine.
    setup(
        &fx,
        &[
            "DROP PROCEDURE sales.p_none",
            "CREATE TABLE sales.p_none (a int)",
            "DROP FUNCTION sales.f_multi",
            "ALTER TABLE sales.orders DROP CONSTRAINT ck_amt",
            "DROP TRIGGER sales.tr",
        ],
    )
    .await;
    for object in objects {
        let e = s.definition(&object).await.unwrap_err();
        assert!(matches!(e, DefinitionError::Missing { .. }), "{e:?}");
    }
}

#[tokio::test]
async fn an_encrypted_routine_says_why_it_has_no_text() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(&fx, SHOP).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let secret = routine(&mut s, "p_secret").await;
    match s.definition(&secret).await.unwrap_err() {
        DefinitionError::Unexpected { message } => {
            assert!(message.contains("encrypted"), "{message}");
            assert!(message.contains("VIEW DEFINITION"), "{message}");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_definition_past_the_cap_is_cut_and_marked() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let select = format!(
        "CREATE VIEW dbo.wide AS SELECT 1 AS one {}",
        "-- padding padding padding padding padding padding padding\n".repeat(20_000)
    );
    setup(&fx, &[&select]).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let cut = s
        .definition(&ObjectRef::relation(RelationRef::new("dbo", "wide")))
        .await
        .unwrap();
    assert!(cut.truncated());
    assert_eq!(cut.text().len(), dbento_core::DEFINITION_CAP);
}

/// `FOR REPLICATION` stores a procedure as type `RF`, and a cursor parameter's catalog row says
/// `int` with only `is_cursor_ref` to tell it apart.
#[tokio::test]
async fn a_replication_procedure_is_listed_and_a_cursor_parameter_reads_as_one() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(
        &fx,
        &[
            "CREATE SCHEMA sales",
            "CREATE PROCEDURE sales.p_cur @c CURSOR VARYING OUTPUT AS SELECT 1 AS one",
            "CREATE PROCEDURE sales.p_rep FOR REPLICATION AS SELECT 1 AS one",
        ],
    )
    .await;
    let mut s = fx.session(SessionOptions::default()).await;
    let objects = s.schema_objects("sales").await.unwrap();
    let routines: Vec<_> = objects
        .routines()
        .iter()
        .map(|r| (r.name(), r.kind(), r.arguments()))
        .collect();
    assert_eq!(
        routines,
        [
            ("p_cur", RoutineKind::Procedure, "@c CURSOR OUTPUT"),
            ("p_rep", RoutineKind::Procedure, ""),
        ]
    );
    let replicated = routine(&mut s, "p_rep").await;
    assert_eq!(
        s.definition(&replicated).await.unwrap().text(),
        "CREATE PROCEDURE sales.p_rep FOR REPLICATION AS SELECT 1 AS one;"
    );
}

/// `sys` is in the schema list, so its extended procedures and system views answer too.
#[tokio::test]
async fn the_system_schema_lists_extended_procedures_and_shows_its_views() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    let objects = s.schema_objects("sys").await.unwrap();
    assert!(objects.types().is_empty() && !objects.truncated());
    let shell = objects
        .routines()
        .iter()
        .find(|r| r.name() == "xp_cmdshell");
    let shell = shell.unwrap();
    assert_eq!(shell.kind(), RoutineKind::Procedure);
    match s.definition(&shell.object_ref()).await.unwrap_err() {
        DefinitionError::Unexpected { message } => {
            assert!(message.contains("extended"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    let view = ObjectRef::relation(RelationRef::new("sys", "tables"));
    let text = s.definition(&view).await.unwrap();
    assert!(text.text().starts_with("CREATE VIEW sys.tables"));
}

/// Without rights on a user type its catalog row is hidden, though a routine the login may call
/// still lists the parameter; without VIEW DEFINITION the text columns read NULL.
#[tokio::test]
async fn a_restricted_login_lists_what_it_may_call_and_hears_why_it_sees_no_text() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_fixture(
        dbento_testkit::mssql_db().await,
        "tls = \"off\"\nread_only = true\nread_only_user = \"r\"",
    );
    setup(&fx, SHOP).await;
    setup(
        &fx,
        &[
            "CREATE FUNCTION sales.f_ph (@a sales.phone) RETURNS sales.phone \
             AS BEGIN RETURN @a END",
            "CREATE USER r WITHOUT LOGIN",
            "GRANT SELECT ON sales.orders TO r",
            "GRANT SELECT ON sales.v TO r",
            "GRANT EXECUTE ON sales.p_orders TO r",
            "GRANT EXECUTE ON sales.f_ph TO r",
        ],
    )
    .await;
    let mut s = fx.session(SessionOptions::default()).await;
    let objects = s.schema_objects("sales").await.unwrap();
    assert!(objects.types().is_empty());
    let routines: Vec<_> = objects
        .routines()
        .iter()
        .map(|r| (r.name(), r.arguments(), r.returns()))
        .collect();
    assert_eq!(
        routines,
        [
            ("f_ph", "@a VARCHAR(20)", Some("VARCHAR(20)")),
            (
                "p_orders",
                "@customer INT, @out INT OUTPUT, @ph VARCHAR(20), @tv TABLE TYPE READONLY",
                None
            ),
        ]
    );
    let trigger = s.relation_objects(&rel("orders")).await.unwrap().triggers()[0].object_ref();
    for object in [
        ObjectRef::relation(rel("v")),
        trigger,
        routine(&mut s, "p_orders").await,
        check(&mut s, "ck_amt").await,
    ] {
        match s.definition(&object).await.unwrap_err() {
            DefinitionError::Unexpected { message } => {
                assert!(message.contains("VIEW DEFINITION"), "{message}");
            }
            other => panic!("{other:?}"),
        }
    }
}

/// Bytes at most the server has sent on the session's connection: its writes hold a packet each.
async fn sent(fx: &MsFixture, spid: &str) -> usize {
    let sql = format!(
        "SELECT CAST(num_writes AS bigint) * net_packet_size FROM sys.dm_exec_connections \
         WHERE session_id = {spid} AND parent_connection_id IS NULL"
    );
    fx.db.scalar(&sql).await.unwrap().parse().unwrap()
}

/// The emoji start right at the cap, so a cut counted in UTF-16 units would split a surrogate
/// pair, and they run on far past it, so a cut made after the read shows in the bytes sent.
#[tokio::test]
async fn a_definition_far_past_the_cap_is_cut_by_the_server_with_a_character_whole() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let cap = dbento_core::DEFINITION_CAP;
    let head = "CREATE VIEW dbo.wide AS SELECT 1 AS one /*";
    let view = format!(
        "{head}{}{}*/",
        "x".repeat(cap - head.len()),
        "\u{1F600}".repeat(2 * cap)
    );
    setup(&fx, &[&view]).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let spid = ms_spid(&mut s).await;
    let before = sent(&fx, &spid).await;
    let cut = s
        .definition(&ObjectRef::relation(RelationRef::new("dbo", "wide")))
        .await
        .unwrap();
    let wire = sent(&fx, &spid).await - before;
    assert!(wire < 3 * cap, "{wire} bytes sent");
    assert!(cut.truncated());
    assert_eq!(cut.text().len(), cap);
    assert!(cut.text().ends_with('x'));
}

#[tokio::test]
async fn synonyms_list_as_other_relations_and_describe_no_columns() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(&fx, SHOP).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let list = s.relations("sales").await.unwrap();
    let kinds: Vec<_> = list
        .items()
        .iter()
        .map(|r| (r.name().to_owned(), r.kind()))
        .collect();
    assert_eq!(
        kinds,
        [
            ("orders".to_owned(), RelationKind::Table),
            ("syn".to_owned(), RelationKind::Other),
            ("v".to_owned(), RelationKind::View),
        ]
    );
    assert!(s.columns(&rel("syn")).await.unwrap().is_empty());
}

#[tokio::test]
async fn bound_defaults_read_as_their_expression_beside_constraint_defaults() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(
        &fx,
        &[
            "CREATE DEFAULT dbo.df_zero AS 0",
            "CREATE DEFAULT /* c */ dbo.df_text AS N'a AS b'  -- trailing",
            "CREATE TABLE dbo.t (a int NOT NULL, b nvarchar(20) NULL, \
             c int NOT NULL CONSTRAINT df_c DEFAULT (7), d int NULL)",
            "EXEC sp_bindefault N'dbo.df_zero', N'dbo.t.a'",
            "EXEC sp_bindefault N'dbo.df_text', N'dbo.t.b'",
        ],
    )
    .await;
    let mut s = fx.session(SessionOptions::default()).await;
    let got = s.columns(&RelationRef::new("dbo", "t")).await.unwrap();
    let col = |n: &str, ty: &str, nullable, d: Option<&str>| {
        ColumnMeta::new(n, ty, nullable, None, d.map(str::to_owned))
    };
    assert_eq!(
        got,
        [
            col("a", "INT", false, Some("0")),
            col("b", "NVARCHAR(20)", true, Some("N'a AS b'")),
            col("c", "INT", false, Some("((7))")),
            col("d", "INT", true, None),
        ]
    );
}

#[tokio::test]
async fn a_synonym_is_composed_and_says_so() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(&fx, SHOP).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let synonym = s
        .definition(&ObjectRef::relation(rel("syn")))
        .await
        .unwrap();
    assert_eq!(
        synonym.text(),
        "CREATE SYNONYM [sales].[syn] FOR [sales].[orders];"
    );
    assert!(synonym.composed());
}

/// The server keeps the base name as written, four parts and all, whether or not it exists.
#[tokio::test]
async fn a_synonym_with_a_remote_base_and_a_bracket_in_its_name_reads_back_runnable() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(&fx, &["CREATE SYNONYM dbo.[sy]]n] FOR srv.db.dbo.nothing"]).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let object = ObjectRef::relation(RelationRef::new("dbo", "sy]n"));
    assert_eq!(
        s.definition(&object).await.unwrap().text(),
        "CREATE SYNONYM [dbo].[sy]]n] FOR [srv].[db].[dbo].[nothing];"
    );
}

/// A synonym needs a grant to be seen at all, and once seen its base name reads without VIEW
/// DEFINITION. A bound default reads for anyone who sees the column; a constraint default's text
/// is withheld, which must not read as a column without a default.
#[tokio::test]
async fn a_restricted_login_sees_a_granted_synonym_and_a_bound_default() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_fixture(
        dbento_testkit::mssql_db().await,
        "tls = \"off\"\nread_only = true\nread_only_user = \"r\"",
    );
    setup(
        &fx,
        &[
            "CREATE DEFAULT dbo.df_zero AS 0",
            "CREATE TABLE dbo.t (a int NOT NULL, c int NOT NULL CONSTRAINT df_c DEFAULT (7))",
            "EXEC sp_bindefault N'dbo.df_zero', N'dbo.t.a'",
            "CREATE SYNONYM dbo.seen FOR dbo.t",
            "CREATE SYNONYM dbo.hidden FOR dbo.t",
            "CREATE USER r WITHOUT LOGIN",
            "GRANT SELECT ON dbo.t TO r",
            "GRANT SELECT ON dbo.seen TO r",
        ],
    )
    .await;
    let mut s = fx.session(SessionOptions::default()).await;
    let list = s.relations("dbo").await.unwrap();
    let kinds: Vec<_> = list.items().iter().map(|r| (r.name(), r.kind())).collect();
    assert_eq!(
        kinds,
        [("seen", RelationKind::Other), ("t", RelationKind::Table)]
    );
    let seen = ObjectRef::relation(RelationRef::new("dbo", "seen"));
    assert_eq!(
        s.definition(&seen).await.unwrap().text(),
        "CREATE SYNONYM [dbo].[seen] FOR [dbo].[t];"
    );
    let hidden = ObjectRef::relation(RelationRef::new("dbo", "hidden"));
    let e = s.definition(&hidden).await.unwrap_err();
    assert!(matches!(e, DefinitionError::Missing { .. }), "{e:?}");
    let got = s.columns(&RelationRef::new("dbo", "t")).await.unwrap();
    let defaults: Vec<_> = got.iter().map(|c| c.default()).collect();
    assert_eq!(
        defaults,
        [Some("0"), Some("/* withheld: needs VIEW DEFINITION */")]
    );
}

/// The server keeps a bound default's comments, which no limit bounds, so the text stops at the
/// server like any other stored text, and the mark says what follows the expression went unread.
#[tokio::test]
async fn a_bound_default_behind_a_long_comment_is_cut_by_the_server() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let cap = dbento_core::DEFINITION_CAP;
    let default = format!("CREATE DEFAULT dbo.df AS 0 /*{}*/", "x".repeat(8 * cap));
    setup(
        &fx,
        &[
            &default,
            "CREATE TABLE dbo.t (a int NOT NULL)",
            "EXEC sp_bindefault N'dbo.df', N'dbo.t.a'",
        ],
    )
    .await;
    let mut s = fx.session(SessionOptions::default()).await;
    let spid = ms_spid(&mut s).await;
    let before = sent(&fx, &spid).await;
    let got = s.columns(&RelationRef::new("dbo", "t")).await.unwrap();
    let wire = sent(&fx, &spid).await - before;
    assert!(wire < 3 * cap, "{wire} bytes sent");
    assert_eq!(got[0].default(), Some("/* cut */ 0"));
}

/// A constraint keeps no comments, but its expression alone can pass the cap.
#[tokio::test]
async fn a_constraint_default_past_the_cap_is_marked() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let cap = dbento_core::DEFINITION_CAP;
    let table = format!(
        "CREATE TABLE dbo.t (a nvarchar(max) NULL CONSTRAINT df_a DEFAULT (N'{}'))",
        "x".repeat(cap + 5)
    );
    setup(&fx, &[&table]).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let got = s.columns(&RelationRef::new("dbo", "t")).await.unwrap();
    let default = got[0].default().unwrap();
    assert!(default.starts_with("/* cut */ (N'xx"), "{}", &default[..20]);
    assert_eq!(default.len(), "/* cut */ ".len() + cap + 1);
}
