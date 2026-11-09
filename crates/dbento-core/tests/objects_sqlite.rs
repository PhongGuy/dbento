//! SQLite schema objects: listings from pragmas and the stored CREATE text.
use crate::support;

use crate::support::{run, sqlite_fixture};
use dbento_core::{
    ConstraintKind, DEFINITION_CAP, DefinitionError, IndexInfo, Limits, ObjectRef, RelationObjects,
    RelationRef, RoutineInfo, RoutineKind, SessionOptions, TriggerInfo, TriggerState, TypeInfo,
    TypeKind,
};

fn read_only() -> SessionOptions {
    let mut opts = SessionOptions::default();
    opts.force_read_only = true;
    opts
}

#[tokio::test]
async fn schema_objects_are_empty_on_sqlite() {
    let fx = sqlite_fixture("CREATE TABLE t(x)", "");
    let mut s = fx.session(read_only()).await;
    for schema in ["main", "nope"] {
        let o = s.schema_objects(schema).await.unwrap();
        assert!(
            o.types().is_empty() && o.routines().is_empty() && !o.truncated(),
            "{schema}"
        );
    }
    s.close().await.unwrap();
}

const SETUP: &str = r#"
CREATE TABLE p(a INT, b TEXT, PRIMARY KEY (a, b));
CREATE TABLE t(
  id INTEGER PRIMARY KEY,
  email TEXT NOT NULL CONSTRAINT email_once UNIQUE,
  qty INT CONSTRAINT qty_positive CHECK (qty > 0),
  a INT, b TEXT,
  n INT CHECK (n <> 0),
  CONSTRAINT t_ab FOREIGN KEY (a, b) REFERENCES p(a, b),
  UNIQUE (a, qty),
  CHECK (a < 100)
);
CREATE INDEX t_partial ON t(a) WHERE a > 10;
CREATE INDEX t_expr ON t(lower(email), qty);
CREATE UNIQUE INDEX t_u ON t(qty);
CREATE VIEW v AS SELECT id FROM t;
CREATE TRIGGER t_ins AFTER INSERT ON t BEGIN SELECT 1; END;
CREATE TRIGGER v_ins INSTEAD OF INSERT ON v BEGIN SELECT 1; END;
CREATE TABLE w(k TEXT, x INT, CONSTRAINT w_pk PRIMARY KEY (k, x)) WITHOUT ROWID;
"#;

fn s(v: &str) -> String {
    v.to_owned()
}
fn cols(v: &[&str]) -> Vec<String> {
    v.iter().map(|c| (*c).to_owned()).collect()
}
fn main_rel(name: &str) -> RelationRef {
    RelationRef::new("main", name)
}
async fn objects(fx: &support::Fixture, rel: RelationRef) -> RelationObjects {
    let mut session = fx.session(read_only()).await;
    let o = session.relation_objects(&rel).await.unwrap();
    session.close().await.unwrap();
    o
}
type IndexRow = (String, Vec<String>, bool, bool, bool, Option<String>);
fn indexes(o: &RelationObjects) -> Vec<IndexRow> {
    o.indexes()
        .iter()
        .map(|i| {
            (
                s(i.name()),
                i.columns().to_vec(),
                i.unique(),
                i.primary(),
                i.partial(),
                i.method().map(s),
            )
        })
        .collect()
}
type KeyRow = (
    Option<String>,
    ConstraintKind,
    Vec<String>,
    Option<RelationRef>,
);
fn keys(o: &RelationObjects) -> Vec<KeyRow> {
    o.constraints()
        .iter()
        .map(|c| {
            (
                c.name().map(s),
                c.kind(),
                c.columns().to_vec(),
                c.referenced().cloned(),
            )
        })
        .collect()
}
fn triggers(o: &RelationObjects) -> Vec<(String, bool)> {
    o.triggers()
        .iter()
        .map(|t| (s(t.name()), t.enabled()))
        .collect()
}

#[tokio::test]
async fn a_table_lists_automatic_and_explicit_indexes_keys_checks_and_triggers() {
    let fx = sqlite_fixture(SETUP, "");
    let o = objects(&fx, main_rel("t")).await;
    assert_eq!(
        indexes(&o),
        vec![
            (
                s("sqlite_autoindex_t_1"),
                cols(&["email"]),
                true,
                false,
                false,
                None
            ),
            (
                s("sqlite_autoindex_t_2"),
                cols(&["a", "qty"]),
                true,
                false,
                false,
                None
            ),
            (
                s("t_expr"),
                cols(&["(expression)", "qty"]),
                false,
                false,
                false,
                None
            ),
            (s("t_partial"), cols(&["a"]), false, false, true, None),
            (s("t_u"), cols(&["qty"]), true, false, false, None),
        ]
    );
    assert_eq!(
        keys(&o),
        vec![
            (None, ConstraintKind::PrimaryKey, cols(&["id"]), None),
            (None, ConstraintKind::Unique, cols(&["a", "qty"]), None),
            (
                Some(s("email_once")),
                ConstraintKind::Unique,
                cols(&["email"]),
                None
            ),
            (
                Some(s("t_ab")),
                ConstraintKind::ForeignKey,
                cols(&["a", "b"]),
                Some(main_rel("p"))
            ),
            (None, ConstraintKind::Check, Vec::new(), None),
            (None, ConstraintKind::Check, cols(&["n"]), None),
            (
                Some(s("qty_positive")),
                ConstraintKind::Check,
                cols(&["qty"]),
                None
            ),
        ]
    );
    assert_eq!(triggers(&o), vec![(s("t_ins"), true)]);
}

#[tokio::test]
async fn composite_and_without_rowid_keys_come_from_their_index() {
    let fx = sqlite_fixture(SETUP, "");
    let p = objects(&fx, main_rel("p")).await;
    assert_eq!(
        keys(&p),
        vec![(None, ConstraintKind::PrimaryKey, cols(&["a", "b"]), None)]
    );
    assert_eq!(
        indexes(&p),
        vec![(
            s("sqlite_autoindex_p_1"),
            cols(&["a", "b"]),
            true,
            true,
            false,
            None
        )]
    );
    let w = objects(&fx, main_rel("w")).await;
    assert_eq!(
        keys(&w),
        vec![(
            Some(s("w_pk")),
            ConstraintKind::PrimaryKey,
            cols(&["k", "x"]),
            None
        )]
    );
    assert_eq!(
        indexes(&w),
        vec![(
            s("sqlite_autoindex_w_1"),
            cols(&["k", "x"]),
            true,
            true,
            false,
            None
        )]
    );
}

#[tokio::test]
async fn a_view_lists_its_instead_of_trigger_and_names_match_any_case() {
    let fx = sqlite_fixture(SETUP, "");
    let v = objects(&fx, main_rel("V")).await;
    assert!(v.indexes().is_empty() && v.constraints().is_empty());
    assert_eq!(triggers(&v), vec![(s("v_ins"), true)]);
    assert_eq!(
        v.triggers()[0].object_ref(),
        TriggerInfo::new(main_rel("v"), "v_ins", TriggerState::Enabled).object_ref()
    );
    assert_eq!(
        objects(&fx, main_rel("T")).await,
        objects(&fx, main_rel("t")).await
    );
}

#[tokio::test]
async fn a_virtual_table_the_parser_rejects_lists_no_objects() {
    let fx = sqlite_fixture("CREATE VIRTUAL TABLE docs USING fts5(a, b);", "");
    let o = objects(&fx, main_rel("docs")).await;
    assert!(
        o.indexes().is_empty() && o.constraints().is_empty() && o.triggers().is_empty(),
        "{o:?}"
    );
}

/// A virtual table whose module this build lacks, written straight into the catalog.
const MISSING_MODULE: &str = "CREATE TABLE b(x); PRAGMA writable_schema = ON; \
    INSERT INTO sqlite_schema VALUES('table', 'v', 'v', 0, 'CREATE VIRTUAL TABLE v USING no_such_module()'); \
    PRAGMA writable_schema = OFF;";

#[tokio::test]
async fn a_virtual_table_whose_module_is_missing_lists_why_its_objects_cannot_be_read() {
    let fx = sqlite_fixture(MISSING_MODULE, "");
    let mut session = fx.session(read_only()).await;
    let o = session.relation_objects(&main_rel("v")).await.unwrap();
    assert!(
        o.indexes().is_empty() && o.constraints().is_empty() && o.triggers().is_empty(),
        "{o:?}"
    );
    assert_eq!(
        o.unreadable_reason(),
        Some("no such module: no_such_module")
    );
    let plain = session.relation_objects(&main_rel("b")).await.unwrap();
    assert_eq!(plain.unreadable_reason(), None);
    session.close().await.unwrap();
}

#[tokio::test]
async fn a_temp_trigger_is_listed_under_the_main_table_it_fires_on() {
    let fx = sqlite_fixture(SETUP, "");
    let aux = fx.dir.path().join("data/aux.db");
    rusqlite::Connection::open(&aux)
        .unwrap()
        .execute_batch("CREATE TABLE t(y)")
        .unwrap();
    let mut session = fx.session(SessionOptions::default()).await;
    let setup = format!(
        "ATTACH '{}' AS aux; \
         CREATE TEMP TRIGGER t_temp AFTER INSERT ON t BEGIN SELECT 1; END; \
         CREATE TEMP TRIGGER t_main AFTER DELETE ON main.T BEGIN SELECT 1; END; \
         CREATE TEMP TRIGGER t_aux AFTER INSERT ON aux.t BEGIN SELECT 1; END;",
        aux.display()
    );
    for r in run(&mut session, &setup, Limits::default()).await {
        r.unwrap();
    }
    let o = session.relation_objects(&main_rel("t")).await.unwrap();
    assert_eq!(
        triggers(&o),
        vec![(s("t_ins"), true), (s("t_main"), true), (s("t_temp"), true)]
    );
    let d = session
        .definition(&o.triggers()[2].object_ref())
        .await
        .unwrap();
    assert_eq!(
        d.text(),
        "CREATE TRIGGER t_temp AFTER INSERT ON t BEGIN SELECT 1; END;"
    );
    let a = session
        .relation_objects(&RelationRef::new("aux", "t"))
        .await
        .unwrap();
    assert_eq!(triggers(&a), vec![(s("t_aux"), true)]);
    session.close().await.unwrap();
}

fn index_ref(rel: RelationRef, name: &str) -> ObjectRef {
    IndexInfo::new(rel, name, Vec::new(), false, false, false, None).object_ref()
}
fn trigger_ref(rel: RelationRef, name: &str) -> ObjectRef {
    TriggerInfo::new(rel, name, TriggerState::Enabled).object_ref()
}

#[tokio::test]
async fn attached_and_mixed_case_definitions_and_objects_on_the_wrong_relation() {
    let fx = sqlite_fixture(SETUP, "");
    let aux = fx.dir.path().join("data/aux.db");
    rusqlite::Connection::open(&aux)
        .unwrap()
        .execute_batch(
            "CREATE TABLE inner_t(id INTEGER PRIMARY KEY, c INT); \
             CREATE INDEX \"Inner Idx\" ON inner_t(c); \
             CREATE TRIGGER \"Inner Trg\" AFTER INSERT ON inner_t BEGIN SELECT 1; END;",
        )
        .unwrap();
    let mut session = fx.session(SessionOptions::default()).await;
    let attach = format!("ATTACH '{}' AS \"aux schema\"", aux.display());
    for r in run(&mut session, &attach, Limits::default()).await {
        r.unwrap();
    }
    let inner = RelationRef::new("aux schema", "inner_t");
    // Names match in any case, and the attached schema's catalog is the one read.
    let cases = [
        (
            index_ref(inner.clone(), "inner idx"),
            "CREATE INDEX \"Inner Idx\" ON inner_t(c);",
        ),
        (
            trigger_ref(RelationRef::new("aux schema", "INNER_T"), "INNER TRG"),
            "CREATE TRIGGER \"Inner Trg\" AFTER INSERT ON inner_t BEGIN SELECT 1; END;",
        ),
        (
            ObjectRef::relation(inner.clone()),
            "CREATE TABLE inner_t(id INTEGER PRIMARY KEY, c INT);",
        ),
    ];
    for (object, want) in cases {
        let d = session.definition(&object).await.unwrap();
        assert_eq!(d.text(), want, "{object:?}");
    }
    // An index or trigger asked for on a relation it does not belong to is missing.
    for object in [
        index_ref(main_rel("p"), "t_partial"),
        trigger_ref(main_rel("v"), "t_ins"),
        index_ref(main_rel("t"), "Inner Idx"),
    ] {
        assert!(
            matches!(
                session.definition(&object).await,
                Err(DefinitionError::Missing { .. })
            ),
            "{object:?}"
        );
    }
    session.close().await.unwrap();
}

#[tokio::test]
async fn unknown_and_attached_schemas() {
    let fx = sqlite_fixture(SETUP, "");
    let aux = fx.dir.path().join("data/aux.db");
    rusqlite::Connection::open(&aux)
        .unwrap()
        .execute_batch("CREATE TABLE inner_t(id INTEGER PRIMARY KEY, c INT CHECK (c > 0))")
        .unwrap();
    let mut session = fx.session(SessionOptions::default()).await;
    for r in run(
        &mut session,
        &format!("ATTACH '{}' AS \"aux schema\"", aux.display()),
        Limits::default(),
    )
    .await
    {
        r.unwrap();
    }
    let inner = session
        .relation_objects(&RelationRef::new("aux schema", "inner_t"))
        .await
        .unwrap();
    assert_eq!(
        keys(&inner),
        vec![
            (None, ConstraintKind::PrimaryKey, cols(&["id"]), None),
            (None, ConstraintKind::Check, cols(&["c"]), None),
        ]
    );
    for r in [
        RelationRef::new("nope", "t"),
        main_rel("nope"),
        RelationRef::new("temp", "t"),
    ] {
        let o = session.relation_objects(&r).await.unwrap();
        assert!(
            o.indexes().is_empty() && o.constraints().is_empty() && o.triggers().is_empty(),
            "{r:?}"
        );
    }
    session.close().await.unwrap();
}

async fn definition(
    fx: &support::Fixture,
    object: &ObjectRef,
) -> Result<dbento_core::Definition, DefinitionError> {
    let mut session = fx.session(read_only()).await;
    let d = session.definition(object).await;
    session.close().await.unwrap();
    d
}
fn stored(fx: &support::Fixture, name: &str) -> String {
    format!(
        "{};",
        fx.text(&format!(
            "SELECT sql FROM sqlite_schema WHERE name = '{name}'"
        ))
    )
}

#[tokio::test]
async fn definitions_are_the_stored_create_text() {
    let fx = sqlite_fixture(SETUP, "");
    let o = objects(&fx, main_rel("t")).await;
    let partial = o
        .indexes()
        .iter()
        .find(|i| i.name() == "t_partial")
        .unwrap();
    let d = definition(&fx, &partial.object_ref()).await.unwrap();
    assert_eq!(d.text(), "CREATE INDEX t_partial ON t(a) WHERE a > 10;");
    assert!(!d.truncated() && !d.composed());
    // Automatic indexes and every constraint live only inside the CREATE TABLE.
    let auto = o
        .indexes()
        .iter()
        .find(|i| i.name() == "sqlite_autoindex_t_1")
        .unwrap();
    assert_eq!(
        definition(&fx, &auto.object_ref()).await.unwrap().text(),
        stored(&fx, "t")
    );
    for c in o.constraints() {
        assert_eq!(
            definition(&fx, &c.object_ref()).await.unwrap().text(),
            stored(&fx, "t"),
            "{c:?}"
        );
    }
    assert_eq!(
        definition(&fx, &o.triggers()[0].object_ref())
            .await
            .unwrap()
            .text(),
        stored(&fx, "t_ins")
    );
    assert_eq!(
        definition(&fx, &ObjectRef::relation(main_rel("T")))
            .await
            .unwrap()
            .text(),
        stored(&fx, "t")
    );
    assert_eq!(
        definition(&fx, &ObjectRef::relation(main_rel("v")))
            .await
            .unwrap()
            .text(),
        "CREATE VIEW v AS SELECT id FROM t;"
    );
    let v = objects(&fx, main_rel("v")).await;
    assert_eq!(
        definition(&fx, &v.triggers()[0].object_ref())
            .await
            .unwrap()
            .text(),
        stored(&fx, "v_ins")
    );
}

#[tokio::test]
async fn a_virtual_table_definition_is_its_stored_create_text() {
    let fx = sqlite_fixture("CREATE VIRTUAL TABLE docs USING fts5(a, b);", "");
    let d = definition(&fx, &ObjectRef::relation(main_rel("docs")))
        .await
        .unwrap();
    assert_eq!(d.text(), "CREATE VIRTUAL TABLE docs USING fts5(a, b);");
}

#[tokio::test]
async fn types_and_routines_have_no_definition_on_sqlite() {
    let fx = sqlite_fixture(SETUP, "");
    let ty = TypeInfo::new("main", "mood", TypeKind::Enum).object_ref();
    match definition(&fx, &ty).await {
        Err(DefinitionError::Unsupported { what }) => assert_eq!(what, "type main.mood"),
        other => panic!("{other:?}"),
    }
    let f = RoutineInfo::new("main", "f", RoutineKind::Function, "", None).object_ref();
    assert!(matches!(
        definition(&fx, &f).await,
        Err(DefinitionError::Unsupported { .. })
    ));
}

#[tokio::test]
async fn an_unknown_or_dropped_object_is_missing() {
    let fx = sqlite_fixture(SETUP, "");
    let o = objects(&fx, main_rel("t")).await;
    let partial = o
        .indexes()
        .iter()
        .find(|i| i.name() == "t_partial")
        .unwrap()
        .object_ref();
    rusqlite::Connection::open(fx.db_path())
        .unwrap()
        .execute_batch("DROP INDEX t_partial")
        .unwrap();
    match definition(&fx, &partial).await {
        Err(DefinitionError::Missing { object }) => assert_eq!(object, "index t_partial on main.t"),
        other => panic!("{other:?}"),
    }
    for r in [main_rel("nope"), RelationRef::new("nope", "t")] {
        assert!(matches!(
            definition(&fx, &ObjectRef::relation(r)).await,
            Err(DefinitionError::Missing { .. })
        ));
    }
}

#[tokio::test]
async fn a_definition_past_the_cap_is_cut_without_a_semicolon() {
    let big = format!(
        "CREATE VIEW big AS SELECT '{}' AS x;",
        "x".repeat(1_100_000)
    );
    let fx = sqlite_fixture(&big, "");
    let d = definition(&fx, &ObjectRef::relation(main_rel("big")))
        .await
        .unwrap();
    assert_eq!(d.text().len(), DEFINITION_CAP);
    assert!(
        d.truncated()
            && !d.text().ends_with(';')
            && d.text().starts_with("CREATE VIEW big AS SELECT 'xxx")
    );
}
