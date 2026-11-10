//! Postgres schema objects: listings and definitions, read through the catalog session.

use std::future::Future;
use std::time::Duration;

use crate::support::{PgFixture, pg_fixture, run, text};
use dbento_core::{
    ConstraintInfo, ConstraintKind, DEFINITION_CAP, Definition, DefinitionError, ExecuteError,
    IndexInfo, IntrospectError, Limits, ObjectRef, RELATION_CAP, RelationObjects, RelationRef,
    RoutineInfo, RoutineKind, SchemaObjects, SessionOptions, TriggerInfo, TriggerState, TypeInfo,
    TypeKind,
};

const SCHEMA: &str = "Obj";
const STATE: &str = "SELECT state FROM pg_stat_activity WHERE application_name = 'dbento' AND datname = current_database()";

/// Every kind the tree lists, in the mixed-case schema "Obj" so every name is quoted on the
/// way out. Nothing depends on the indexes, the constraints of t, t_off, v_ins, the add
/// overloads, rows_of, bump, the types, the aggregates, vc or mv, so each can be dropped and
/// replayed alone; trg is used by t's triggers and p's key by t_a_b_fkey.
const SETUP: &str = r#"
CREATE SCHEMA "Obj";
SET search_path = "Obj";
CREATE TABLE p(a int, b text, PRIMARY KEY (a, b));
CREATE TABLE t(
  id int PRIMARY KEY,
  email text NOT NULL CONSTRAINT t_email_key UNIQUE,
  "Mixed Col" text,
  a int, b text,
  during int4range,
  qty int CONSTRAINT qty_positive CHECK (qty > 0),
  CONSTRAINT t_a_b_fkey FOREIGN KEY (a, b) REFERENCES p(a, b),
  CONSTRAINT t_no_overlap EXCLUDE USING gist (during WITH &&)
);
CREATE INDEX t_partial ON t (a) WHERE a > 10;
CREATE INDEX "T Expr" ON t (lower(email), "Mixed Col") INCLUDE (qty);
CREATE INDEX t_gin ON t USING gin (to_tsvector('simple', email));
CREATE FUNCTION trg() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN RETURN NEW; END$$;
CREATE TRIGGER t_on BEFORE INSERT ON t FOR EACH ROW EXECUTE FUNCTION trg();
CREATE TRIGGER t_off AFTER UPDATE OF email ON t FOR EACH ROW
  WHEN (OLD.email IS DISTINCT FROM NEW.email) EXECUTE FUNCTION trg();
ALTER TABLE t DISABLE TRIGGER t_off;
CREATE TYPE mood AS ENUM ('sad', 'ok', 'it''s fine');
CREATE DOMAIN pos AS int DEFAULT 1 NOT NULL CONSTRAINT pos_check CHECK (VALUE > 0);
CREATE DOMAIN code AS text COLLATE "C" CHECK (length(VALUE) < 9);
CREATE TYPE pair AS (x int, "Y" text COLLATE "C", z numeric(10,2));
CREATE TYPE fr AS RANGE (SUBTYPE = float8, SUBTYPE_DIFF = float8mi);
CREATE FUNCTION add(x int, y int DEFAULT 1) RETURNS int LANGUAGE sql IMMUTABLE AS 'SELECT x + y';
CREATE FUNCTION add(text, text) RETURNS text LANGUAGE sql AS $$SELECT $1 || $2$$;
CREATE FUNCTION rows_of(n int) RETURNS TABLE(i int) LANGUAGE sql AS 'SELECT generate_series(1, n)';
CREATE PROCEDURE bump(INOUT n int) LANGUAGE plpgsql AS $$BEGIN n := n + 1; END$$;
CREATE AGGREGATE mysum(int) (SFUNC = int4pl, STYPE = int4, INITCOND = '0');
CREATE AGGREGATE mymax(int) (SFUNC = int4larger, STYPE = int4, COMBINEFUNC = int4larger, SORTOP = >);
CREATE VIEW v AS SELECT id, email FROM t WHERE id > 0;
CREATE VIEW vc WITH (security_barrier = true, check_option = local) AS SELECT id FROM t WHERE id > 5;
CREATE MATERIALIZED VIEW mv AS SELECT count(*) AS n FROM t;
CREATE FUNCTION vtrg() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN RETURN NEW; END$$;
CREATE TRIGGER v_ins INSTEAD OF INSERT ON v FOR EACH ROW EXECUTE FUNCTION vtrg();
CREATE TABLE parted(k int PRIMARY KEY) PARTITION BY RANGE (k);
CREATE INDEX parted_k ON parted (k);
RESET search_path;
"#;

async fn bounded(test: impl Future<Output = ()>) {
    tokio::time::timeout(Duration::from_secs(180), test)
        .await
        .expect("test timed out");
}

fn read_only() -> SessionOptions {
    let mut opts = SessionOptions::default();
    opts.force_read_only = true;
    opts
}

async fn setup() -> PgFixture {
    let fx = pg_fixture("").await;
    fx.db.admin().await.batch_execute(SETUP).await.unwrap();
    fx
}

fn rel(name: &str) -> RelationRef {
    RelationRef::new(SCHEMA, name)
}

async fn definition(fx: &PgFixture, object: &ObjectRef) -> Result<Definition, DefinitionError> {
    let mut s = fx.session(read_only()).await;
    let d = s.definition(object).await;
    s.close().await.unwrap();
    d
}

/// Captures `object`'s definition, runs `drop` and then the captured text on the admin
/// connection, and asserts the definition reads back unchanged.
async fn round_trip(fx: &PgFixture, object: &ObjectRef, drop: &str) -> Definition {
    let before = definition(fx, object).await.unwrap();
    assert!(!before.truncated(), "{before:?}");
    assert!(
        before.text().ends_with(';') && !before.text().ends_with(";;"),
        "{before:?}"
    );
    let admin = fx.db.admin().await;
    admin.batch_execute(drop).await.unwrap();
    admin
        .batch_execute(before.text())
        .await
        .unwrap_or_else(|e| panic!("replaying {:?}: {e}", before.text()));
    let after = definition(fx, object).await.unwrap();
    assert_eq!(
        after, before,
        "the replayed definition matches the original"
    );
    before
}

#[tokio::test]
async fn view_definitions_round_trip_with_their_options() {
    bounded(async {
        let fx = setup().await;
        let vc = round_trip(&fx, &ObjectRef::relation(rel("vc")), r#"DROP VIEW "Obj".vc"#).await;
        assert!(
            vc.text().starts_with(
                "CREATE VIEW \"Obj\".vc WITH (security_barrier=true, check_option=local) AS\n SELECT "
            ),
            "{vc:?}"
        );
        assert!(!vc.composed());
        let mv = round_trip(&fx, &ObjectRef::relation(rel("mv")), r#"DROP MATERIALIZED VIEW "Obj".mv"#).await;
        assert!(mv.text().starts_with("CREATE MATERIALIZED VIEW \"Obj\".mv AS\n SELECT count(*) AS n"), "{mv:?}");
        // DROP VIEW takes v's INSTEAD OF trigger with it; the view text is what is compared.
        round_trip(&fx, &ObjectRef::relation(rel("v")), r#"DROP VIEW "Obj".v"#).await;
    })
    .await;
}

#[tokio::test]
async fn tables_have_no_definition_on_postgres() {
    bounded(async {
        let fx = setup().await;
        for (name, what) in [("t", "table Obj.t"), ("parted", "table Obj.parted")] {
            match definition(&fx, &ObjectRef::relation(rel(name))).await {
                Err(DefinitionError::Unsupported { what: w }) => assert_eq!(w, what),
                other => panic!("{name}: {other:?}"),
            }
        }
    })
    .await;
}

#[tokio::test]
async fn an_unknown_or_dropped_relation_is_missing() {
    bounded(async {
        let fx = setup().await;
        match definition(&fx, &ObjectRef::relation(rel("nope"))).await {
            Err(DefinitionError::Missing { object }) => assert_eq!(object, "relation Obj.nope"),
            other => panic!("{other:?}"),
        }
        let vc = ObjectRef::relation(rel("vc"));
        definition(&fx, &vc).await.unwrap();
        fx.db
            .admin()
            .await
            .batch_execute(r#"DROP VIEW "Obj".vc"#)
            .await
            .unwrap();
        assert!(matches!(
            definition(&fx, &vc).await,
            Err(DefinitionError::Missing { .. })
        ));
    })
    .await;
}

#[tokio::test]
async fn a_definition_past_the_cap_is_cut_without_a_semicolon() {
    bounded(async {
        let fx = setup().await;
        fx.db
            .admin()
            .await
            .batch_execute(
                r#"DO $d$ BEGIN
                  EXECUTE format('CREATE VIEW "Obj".big AS SELECT %L::text AS x', repeat('x', 1100000));
                  EXECUTE format('CREATE VIEW "Obj".wide AS SELECT %L::text AS x', repeat('é', 600000));
                END $d$"#,
            )
            .await
            .unwrap();
        let big = definition(&fx, &ObjectRef::relation(rel("big"))).await.unwrap();
        assert_eq!(big.text().len(), DEFINITION_CAP);
        assert!(big.truncated() && !big.composed() && !big.text().ends_with(';'));
        assert!(big.text().starts_with("CREATE VIEW \"Obj\".big AS\n SELECT 'xxx"));
        let wide = definition(&fx, &ObjectRef::relation(rel("wide"))).await.unwrap();
        assert!(wide.truncated() && wide.text().ends_with('é'));
        assert!((DEFINITION_CAP - 1..=DEFINITION_CAP).contains(&wide.text().len()));
    })
    .await;
}

#[tokio::test]
async fn a_definition_leaves_a_read_only_session_idle() {
    bounded(async {
        let fx = setup().await;
        let mut s = fx.session(read_only()).await;
        s.definition(&ObjectRef::relation(rel("vc"))).await.unwrap();
        assert_eq!(fx.db.scalar(STATE).await.as_deref(), Some("idle"));
        s.close().await.unwrap();
    })
    .await;
}

fn index(table: &str, name: &str) -> ObjectRef {
    IndexInfo::new(rel(table), name, Vec::new(), false, false, false, None).object_ref()
}
fn constraint(table: &str, name: &str, kind: ConstraintKind) -> ObjectRef {
    ConstraintInfo::new(rel(table), Some(name.to_owned()), kind, Vec::new(), None).object_ref()
}
fn trigger(table: &str, name: &str) -> ObjectRef {
    TriggerInfo::new(rel(table), name, TriggerState::Enabled).object_ref()
}
fn routine(name: &str, arguments: &str) -> ObjectRef {
    RoutineInfo::new(SCHEMA, name, RoutineKind::Function, arguments, None).object_ref()
}

/// Points `fx` at a new login role that cannot read pg_proc in the test database; returns
/// the role for `drop_role`.
async fn without_pg_proc(fx: &mut PgFixture) -> String {
    let role = format!("noproc_{}", fx.db.database());
    fx.db
        .admin()
        .await
        .batch_execute(&format!(
            "DROP ROLE IF EXISTS \"{role}\"; CREATE ROLE \"{role}\" LOGIN PASSWORD '{}';
             REVOKE SELECT ON pg_catalog.pg_proc FROM PUBLIC",
            fx.db.password()
        ))
        .await
        .unwrap();
    let toml = std::fs::read_to_string(fx.dir.path().join("dbento.toml")).unwrap();
    let user = format!("user = \"{}\"", fx.db.user());
    assert!(toml.contains(&user), "{toml}");
    let toml = toml.replace(&user, &format!("user = \"{role}\""));
    std::fs::write(fx.dir.path().join("dbento.toml"), toml).unwrap();
    fx.ws = dbento_core::Workspace::open(fx.dir.path()).unwrap();
    role
}

async fn drop_role(fx: &PgFixture, role: &str) {
    fx.db
        .admin()
        .await
        .batch_execute(&format!("DROP ROLE \"{role}\""))
        .await
        .unwrap();
}

#[tokio::test]
async fn index_definitions_round_trip() {
    bounded(async {
        let fx = setup().await;
        let expr = round_trip(&fx, &index("t", "T Expr"), r#"DROP INDEX "Obj"."T Expr""#).await;
        assert_eq!(
            expr.text(),
            r#"CREATE INDEX "T Expr" ON "Obj".t USING btree (lower(email), "Mixed Col") INCLUDE (qty);"#
        );
        assert!(!expr.composed());
        round_trip(&fx, &index("t", "t_partial"), r#"DROP INDEX "Obj".t_partial"#).await;
        round_trip(&fx, &index("t", "t_gin"), r#"DROP INDEX "Obj".t_gin"#).await;
        round_trip(&fx, &index("parted", "parted_k"), r#"DROP INDEX "Obj".parted_k"#).await;
        // A constraint's index shows as a plain index; it replays only as the constraint.
        let pkey = definition(&fx, &index("t", "t_pkey")).await.unwrap();
        assert_eq!(pkey.text(), r#"CREATE UNIQUE INDEX t_pkey ON "Obj".t USING btree (id);"#);
    })
    .await;
}

#[tokio::test]
async fn constraint_definitions_round_trip_as_alter_table() {
    bounded(async {
        let fx = setup().await;
        let fk = round_trip(
            &fx,
            &constraint("t", "t_a_b_fkey", ConstraintKind::ForeignKey),
            r#"ALTER TABLE "Obj".t DROP CONSTRAINT t_a_b_fkey"#,
        )
        .await;
        assert_eq!(
            fk.text(),
            r#"ALTER TABLE "Obj".t ADD CONSTRAINT t_a_b_fkey FOREIGN KEY (a, b) REFERENCES "Obj".p(a, b);"#
        );
        for (name, kind) in [
            ("qty_positive", ConstraintKind::Check),
            ("t_email_key", ConstraintKind::Unique),
            ("t_no_overlap", ConstraintKind::Exclusion),
            ("t_pkey", ConstraintKind::PrimaryKey),
        ] {
            round_trip(
                &fx,
                &constraint("t", name, kind),
                &format!(r#"ALTER TABLE "Obj".t DROP CONSTRAINT {name}"#),
            )
            .await;
        }
    })
    .await;
}

#[tokio::test]
async fn trigger_definitions_round_trip_on_tables_and_views() {
    bounded(async {
        let fx = setup().await;
        let off = round_trip(&fx, &trigger("t", "t_off"), r#"DROP TRIGGER t_off ON "Obj".t"#).await;
        assert_eq!(
            off.text(),
            r#"CREATE TRIGGER t_off AFTER UPDATE OF email ON "Obj".t FOR EACH ROW WHEN (old.email IS DISTINCT FROM new.email) EXECUTE FUNCTION "Obj".trg();"#
        );
        round_trip(&fx, &trigger("v", "v_ins"), r#"DROP TRIGGER v_ins ON "Obj".v"#).await;
    })
    .await;
}

#[tokio::test]
async fn routine_definitions_round_trip_per_overload() {
    bounded(async {
        let fx = setup().await;
        let ints = round_trip(&fx, &routine("add", "x integer, y integer"), r#"DROP FUNCTION "Obj".add(int, int)"#).await;
        assert!(
            ints.text().starts_with("CREATE OR REPLACE FUNCTION \"Obj\".add(x integer, y integer DEFAULT 1)\n RETURNS integer\n"),
            "{ints:?}"
        );
        assert!(ints.text().ends_with("$function$;"), "{ints:?}");
        let texts = round_trip(&fx, &routine("add", "text, text"), r#"DROP FUNCTION "Obj".add(text, text)"#).await;
        assert_ne!(ints.text(), texts.text());
        round_trip(&fx, &routine("rows_of", "n integer"), r#"DROP FUNCTION "Obj".rows_of(int)"#).await;
        let bump = round_trip(&fx, &routine("bump", "INOUT n integer"), r#"DROP PROCEDURE "Obj".bump(int)"#).await;
        assert!(bump.text().starts_with("CREATE OR REPLACE PROCEDURE \"Obj\".bump(INOUT n integer)\n"), "{bump:?}");
        assert!(!bump.composed());
    })
    .await;
}

#[tokio::test]
async fn a_wrong_overload_or_a_dropped_object_is_missing() {
    bounded(async {
        let fx = setup().await;
        match definition(&fx, &routine("add", "integer")).await {
            Err(DefinitionError::Missing { object }) => {
                assert_eq!(object, "routine Obj.add(integer)")
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            definition(&fx, &index("t", "nope")).await,
            Err(DefinitionError::Missing { .. })
        ));
        let unnamed = ConstraintInfo::new(rel("t"), None, ConstraintKind::Check, Vec::new(), None)
            .object_ref();
        assert!(matches!(
            definition(&fx, &unnamed).await,
            Err(DefinitionError::Missing { .. })
        ));
        let on = trigger("t", "t_on");
        definition(&fx, &on).await.unwrap();
        fx.db
            .admin()
            .await
            .batch_execute(r#"DROP TRIGGER t_on ON "Obj".t"#)
            .await
            .unwrap();
        match definition(&fx, &on).await {
            Err(DefinitionError::Missing { object }) => assert_eq!(object, "trigger t_on on Obj.t"),
            other => panic!("{other:?}"),
        }
    })
    .await;
}

#[tokio::test]
async fn a_role_that_cannot_read_pg_proc_gets_the_permission_error() {
    bounded(async {
        let mut fx = setup().await;
        let role = without_pg_proc(&mut fx).await;
        let got = definition(&fx, &routine("add", "text, text")).await;
        drop_role(&fx, &role).await;
        match got {
            Err(DefinitionError::Execute(ExecuteError::Database { code, .. })) => {
                assert_eq!(code, "42501")
            }
            other => panic!("{other:?}"),
        }
    })
    .await;
}

#[tokio::test]
async fn a_failed_lookup_inside_a_users_block_leaves_the_block_usable() {
    bounded(async {
        let mut fx = setup().await;
        let role = without_pg_proc(&mut fx).await;
        let mut session = fx.session(SessionOptions::default()).await;
        for r in run(&mut session, "BEGIN", Limits::default()).await {
            r.unwrap();
        }
        let got = session.definition(&routine("add", "text, text")).await;
        assert!(
            matches!(&got, Err(DefinitionError::Execute(ExecuteError::Database { code, .. })) if code == "42501"),
            "{got:?}"
        );
        let one = run(&mut session, "SELECT 1::text", Limits::default()).await.remove(0).unwrap();
        assert_eq!(one.rows, vec![vec![text("1")]]);
        for r in run(&mut session, "ROLLBACK", Limits::default()).await {
            r.unwrap();
        }
        session.close().await.unwrap();
        drop_role(&fx, &role).await;
    })
    .await;
}

/// A lowercase schema a connection can put on its search path: a type, a view over a table
/// of it, and a function taking it.
const ON_PATH: &str = "CREATE SCHEMA onpath; CREATE TYPE onpath.kind AS ENUM ('a');
CREATE TABLE onpath.t2(k onpath.kind); CREATE VIEW onpath.v2 AS SELECT k FROM onpath.t2;
CREATE FUNCTION onpath.f(m onpath.kind) RETURNS text LANGUAGE sql AS 'SELECT m::text'";

fn on_path(name: &str) -> RelationRef {
    RelationRef::new("onpath", name)
}

#[tokio::test]
async fn definitions_stay_qualified_when_their_schema_is_on_the_search_path() {
    bounded(async {
        let fx = pg_fixture(r#"settings = { search_path = "onpath, public" }"#).await;
        fx.db.admin().await.batch_execute(ON_PATH).await.unwrap();
        let v2 = definition(&fx, &ObjectRef::relation(on_path("v2")))
            .await
            .unwrap();
        assert!(v2.text().ends_with("FROM onpath.t2;"), "{v2:?}");
        let f = RoutineInfo::new("onpath", "f", RoutineKind::Function, "m onpath.kind", None)
            .object_ref();
        let body = definition(&fx, &f).await.unwrap();
        assert!(
            body.text()
                .starts_with("CREATE OR REPLACE FUNCTION onpath.f(m onpath.kind)\n"),
            "{body:?}"
        );
        // A read-write session's own path is untouched by the lookup.
        let mut session = fx.session(SessionOptions::default()).await;
        session.definition(&f).await.unwrap();
        let shown = run(&mut session, "SHOW search_path", Limits::default())
            .await
            .remove(0)
            .unwrap();
        assert_eq!(shown.rows, vec![vec![text("onpath, public")]]);
        session.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn a_definition_inside_an_open_transaction_keeps_the_users_path_and_work() {
    bounded(async {
        let fx = pg_fixture("").await;
        fx.db.admin().await.batch_execute(ON_PATH).await.unwrap();
        let mut session = fx.session(SessionOptions::default()).await;
        let begin = "BEGIN; SET LOCAL search_path = onpath; CREATE TABLE inflight(x int)";
        for r in run(&mut session, begin, Limits::default()).await {
            r.unwrap();
        }
        let v2 = session
            .definition(&ObjectRef::relation(on_path("v2")))
            .await
            .unwrap();
        assert!(v2.text().ends_with("FROM onpath.t2;"), "{v2:?}");
        let gone = session
            .definition(&ObjectRef::relation(on_path("nope")))
            .await;
        assert!(
            matches!(gone, Err(DefinitionError::Missing { .. })),
            "{gone:?}"
        );
        let shown = run(&mut session, "SHOW search_path", Limits::default())
            .await
            .remove(0)
            .unwrap();
        assert_eq!(shown.rows, vec![vec![text("onpath")]]);
        // inflight was made under the user's path, inside the still-open block.
        let n = run(
            &mut session,
            "SELECT count(*)::text FROM inflight",
            Limits::default(),
        )
        .await
        .remove(0)
        .unwrap();
        assert_eq!(n.rows, vec![vec![text("0")]]);
        for r in run(&mut session, "ROLLBACK", Limits::default()).await {
            r.unwrap();
        }
        let left = "SELECT count(*)::text FROM pg_class WHERE relname = 'inflight'";
        assert_eq!(fx.db.scalar(left).await.as_deref(), Some("0"));
        session.close().await.unwrap();
    })
    .await;
}

fn type_ref(name: &str) -> ObjectRef {
    TypeInfo::new(SCHEMA, name, TypeKind::Other).object_ref()
}
fn aggregate(name: &str) -> ObjectRef {
    RoutineInfo::new(
        SCHEMA,
        name,
        RoutineKind::Aggregate,
        "integer",
        Some("integer".to_owned()),
    )
    .object_ref()
}

#[tokio::test]
async fn composed_type_definitions_round_trip() {
    bounded(async {
        let fx = setup().await;
        for (name, drop, text) in [
            ("mood", r#"DROP TYPE "Obj".mood"#, r#"CREATE TYPE "Obj".mood AS ENUM ('sad', 'ok', 'it''s fine');"#),
            (
                "pos",
                r#"DROP DOMAIN "Obj".pos"#,
                r#"CREATE DOMAIN "Obj".pos AS integer DEFAULT 1 NOT NULL CONSTRAINT pos_check CHECK (VALUE > 0);"#,
            ),
            (
                "code",
                r#"DROP DOMAIN "Obj".code"#,
                r#"CREATE DOMAIN "Obj".code AS text COLLATE pg_catalog."C" CONSTRAINT code_check CHECK (length(VALUE) < 9);"#,
            ),
            (
                "pair",
                r#"DROP TYPE "Obj".pair"#,
                r#"CREATE TYPE "Obj".pair AS (x integer, "Y" text COLLATE pg_catalog."C", z numeric(10,2));"#,
            ),
            (
                "fr",
                r#"DROP TYPE "Obj".fr"#,
                r#"CREATE TYPE "Obj".fr AS RANGE (SUBTYPE = double precision, SUBTYPE_DIFF = float8mi);"#,
            ),
        ] {
            let d = round_trip(&fx, &type_ref(name), drop).await;
            assert_eq!(d.text(), text);
            assert!(d.composed(), "{name}");
        }
    })
    .await;
}

#[tokio::test]
async fn composed_aggregate_definitions_round_trip() {
    bounded(async {
        let fx = setup().await;
        let sum = round_trip(&fx, &aggregate("mysum"), r#"DROP AGGREGATE "Obj".mysum(int)"#).await;
        assert_eq!(
            sum.text(),
            r#"CREATE AGGREGATE "Obj".mysum(integer) (SFUNC = int4pl, STYPE = integer, INITCOND = '0');"#
        );
        assert!(sum.composed());
        let max = round_trip(&fx, &aggregate("mymax"), r#"DROP AGGREGATE "Obj".mymax(int)"#).await;
        assert_eq!(
            max.text(),
            r#"CREATE AGGREGATE "Obj".mymax(integer) (SFUNC = int4larger, STYPE = integer, COMBINEFUNC = int4larger, SORTOP = OPERATOR(pg_catalog.>));"#
        );
    })
    .await;
}

#[tokio::test]
async fn composed_definitions_quote_user_functions_and_keep_range_options() {
    bounded(async {
        let fx = setup().await;
        fx.db
            .admin()
            .await
            .batch_execute(
                r#"CREATE SCHEMA "W2";
                CREATE FUNCTION "W2"."S f"(int, int) RETURNS int LANGUAGE sql AS 'SELECT $1 + $2';
                CREATE AGGREGATE "Obj".wsum(int) (SFUNC = "W2"."S f", STYPE = int);
                CREATE TYPE "Obj".tr AS RANGE (SUBTYPE = text, SUBTYPE_OPCLASS = text_pattern_ops, COLLATION = "C");"#,
            )
            .await
            .unwrap();
        let agg = round_trip(&fx, &aggregate("wsum"), r#"DROP AGGREGATE "Obj".wsum(int)"#).await;
        assert_eq!(
            agg.text(),
            r#"CREATE AGGREGATE "Obj".wsum(integer) (SFUNC = "W2"."S f", STYPE = integer);"#
        );
        let range = round_trip(&fx, &type_ref("tr"), r#"DROP TYPE "Obj".tr"#).await;
        assert_eq!(
            range.text(),
            r#"CREATE TYPE "Obj".tr AS RANGE (SUBTYPE = text, SUBTYPE_OPCLASS = pg_catalog.text_pattern_ops, COLLATION = pg_catalog."C");"#
        );
    })
    .await;
}

#[tokio::test]
async fn row_types_multiranges_and_arrays_are_not_type_objects() {
    bounded(async {
        let fx = setup().await;
        for name in ["t", "fr_multirange", "_mood", "nope"] {
            assert!(
                matches!(
                    definition(&fx, &type_ref(name)).await,
                    Err(DefinitionError::Missing { .. })
                ),
                "{name}"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn a_lookup_inside_an_aborted_block_reports_it_and_the_user_can_still_roll_back() {
    bounded(async {
        let fx = setup().await;
        let mut session = fx.session(SessionOptions::default()).await;
        let mut begun = run(&mut session, "BEGIN; SELECT 1/0", Limits::default()).await;
        assert!(begun.pop().unwrap().is_err());
        let got = session.definition(&index("t", "t_partial")).await;
        assert!(
            matches!(&got, Err(DefinitionError::Execute(ExecuteError::Database { code, .. })) if code == "25P02"),
            "{got:?}"
        );
        for r in run(&mut session, "ROLLBACK", Limits::default()).await {
            r.unwrap();
        }
        assert_eq!(fx.db.scalar(STATE).await.as_deref(), Some("idle"));
        session.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn a_lookup_after_a_committed_block_leaves_the_session_read_write_outside_a_block() {
    bounded(async {
        let fx = setup().await;
        let mut session = fx.session(SessionOptions::default()).await;
        for r in run(&mut session, "BEGIN; COMMIT", Limits::default()).await {
            r.unwrap();
        }
        session.definition(&index("t", "t_partial")).await.unwrap();
        assert_eq!(fx.db.scalar(STATE).await.as_deref(), Some("idle"));
        let ro = run(
            &mut session,
            "SHOW transaction_read_only",
            Limits::default(),
        )
        .await
        .remove(0)
        .unwrap();
        assert_eq!(ro.rows, vec![vec![text("off")]]);
        // A table made now must outlive the session: no lookup block is left around it.
        for r in run(
            &mut session,
            "CREATE TABLE after_lookup(x int)",
            Limits::default(),
        )
        .await
        {
            r.unwrap();
        }
        session.close().await.unwrap();
        let left = "SELECT count(*)::text FROM pg_class WHERE relname = 'after_lookup'";
        assert_eq!(fx.db.scalar(left).await.as_deref(), Some("1"));
    })
    .await;
}

#[tokio::test]
async fn a_domain_default_follows_a_renamed_schema() {
    bounded(async {
        let fx = setup().await;
        fx.db
            .admin()
            .await
            .batch_execute(
                r#"CREATE SCHEMA "We ird";
                CREATE FUNCTION "We ird"."F x"() RETURNS int LANGUAGE sql AS 'SELECT 1';
                CREATE DOMAIN "We ird".d AS int DEFAULT "We ird"."F x"();
                ALTER SCHEMA "We ird" RENAME TO "W2";"#,
            )
            .await
            .unwrap();
        let d = definition(
            &fx,
            &TypeInfo::new("W2", "d", TypeKind::Domain).object_ref(),
        )
        .await
        .unwrap();
        assert_eq!(
            d.text(),
            r#"CREATE DOMAIN "W2".d AS integer DEFAULT "W2"."F x"();"#
        );
    })
    .await;
}

#[tokio::test]
async fn a_not_valid_domain_check_replays_as_a_following_alter_domain() {
    bounded(async {
        let fx = setup().await;
        fx.db
            .admin()
            .await
            .batch_execute(
                r#"CREATE DOMAIN "Obj".nv AS int CONSTRAINT nv_small CHECK (VALUE < 100);
                ALTER DOMAIN "Obj".nv ADD CONSTRAINT nv_check CHECK (VALUE > 0) NOT VALID;"#,
            )
            .await
            .unwrap();
        let d = round_trip(&fx, &type_ref("nv"), r#"DROP DOMAIN "Obj".nv"#).await;
        assert_eq!(
            d.text(),
            "CREATE DOMAIN \"Obj\".nv AS integer CONSTRAINT nv_small CHECK (VALUE < 100);\n\
             ALTER DOMAIN \"Obj\".nv ADD CONSTRAINT nv_check CHECK (VALUE > 0) NOT VALID;"
        );
    })
    .await;
}

fn s(v: &str) -> String {
    v.to_owned()
}
fn cols(v: &[&str]) -> Vec<String> {
    v.iter().map(|c| (*c).to_owned()).collect()
}
async fn objects(fx: &PgFixture, rel: RelationRef) -> RelationObjects {
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
async fn a_table_lists_its_indexes_keys_checks_and_triggers() {
    bounded(async {
        let fx = setup().await;
        let o = objects(&fx, rel("t")).await;
        let btree = Some(s("btree"));
        assert_eq!(
            indexes(&o),
            vec![
                (
                    s("T Expr"),
                    cols(&["lower(email)", "Mixed Col"]),
                    false,
                    false,
                    false,
                    btree.clone()
                ),
                (
                    s("t_email_key"),
                    cols(&["email"]),
                    true,
                    false,
                    false,
                    btree.clone()
                ),
                (
                    s("t_gin"),
                    cols(&["to_tsvector('simple'::regconfig, email)"]),
                    false,
                    false,
                    false,
                    Some(s("gin"))
                ),
                (
                    s("t_no_overlap"),
                    cols(&["during"]),
                    false,
                    false,
                    false,
                    Some(s("gist"))
                ),
                (
                    s("t_partial"),
                    cols(&["a"]),
                    false,
                    false,
                    true,
                    btree.clone()
                ),
                (s("t_pkey"), cols(&["id"]), true, true, false, btree),
            ]
        );
        assert_eq!(
            keys(&o),
            vec![
                (
                    Some(s("t_pkey")),
                    ConstraintKind::PrimaryKey,
                    cols(&["id"]),
                    None
                ),
                (
                    Some(s("t_email_key")),
                    ConstraintKind::Unique,
                    cols(&["email"]),
                    None
                ),
                (
                    Some(s("t_a_b_fkey")),
                    ConstraintKind::ForeignKey,
                    cols(&["a", "b"]),
                    Some(rel("p"))
                ),
                (
                    Some(s("t_no_overlap")),
                    ConstraintKind::Exclusion,
                    cols(&["during"]),
                    None
                ),
                (
                    Some(s("qty_positive")),
                    ConstraintKind::Check,
                    cols(&["qty"]),
                    None
                ),
            ]
        );
        assert_eq!(triggers(&o), vec![(s("t_off"), false), (s("t_on"), true)]);
        // A listed object opens its own definition.
        let first = definition(&fx, &o.indexes()[0].object_ref()).await.unwrap();
        assert!(
            first.text().starts_with("CREATE INDEX \"T Expr\""),
            "{first:?}"
        );
        let check = definition(&fx, &o.constraints()[4].object_ref())
            .await
            .unwrap();
        assert_eq!(
            check.text(),
            r#"ALTER TABLE "Obj".t ADD CONSTRAINT qty_positive CHECK (qty > 0);"#
        );
    })
    .await;
}

#[tokio::test]
async fn a_replica_only_trigger_shows_its_state() {
    bounded(async {
        let fx = setup().await;
        fx.db
            .admin()
            .await
            .batch_execute(
                r#"CREATE TRIGGER t_replica BEFORE INSERT ON "Obj".t FOR EACH ROW EXECUTE FUNCTION "Obj".trg();
                ALTER TABLE "Obj".t ENABLE REPLICA TRIGGER t_replica;
                CREATE TRIGGER t_always BEFORE INSERT ON "Obj".t FOR EACH ROW EXECUTE FUNCTION "Obj".trg();
                ALTER TABLE "Obj".t ENABLE ALWAYS TRIGGER t_always;"#,
            )
            .await
            .unwrap();
        let o = objects(&fx, rel("t")).await;
        let states: Vec<(String, TriggerState)> =
            o.triggers().iter().map(|t| (s(t.name()), t.state())).collect();
        assert_eq!(
            states,
            vec![
                (s("t_always"), TriggerState::Enabled),
                (s("t_off"), TriggerState::Disabled),
                (s("t_on"), TriggerState::Enabled),
                (s("t_replica"), TriggerState::ReplicaOnly),
            ]
        );
        assert!(!o.triggers()[3].enabled());
    })
    .await;
}

#[tokio::test]
async fn a_view_lists_its_trigger_and_a_partitioned_table_its_indexes() {
    bounded(async {
        let fx = setup().await;
        let v = objects(&fx, rel("v")).await;
        assert!(v.indexes().is_empty() && v.constraints().is_empty());
        assert_eq!(triggers(&v), vec![(s("v_ins"), true)]);
        let parted = objects(&fx, rel("parted")).await;
        let btree = Some(s("btree"));
        assert_eq!(
            indexes(&parted),
            vec![
                (
                    s("parted_k"),
                    cols(&["k"]),
                    false,
                    false,
                    false,
                    btree.clone()
                ),
                (s("parted_pkey"), cols(&["k"]), true, true, false, btree),
            ]
        );
        assert_eq!(
            keys(&parted),
            vec![(
                Some(s("parted_pkey")),
                ConstraintKind::PrimaryKey,
                cols(&["k"]),
                None
            )]
        );
    })
    .await;
}

#[tokio::test]
async fn unknown_schemas_and_relations_have_no_objects() {
    bounded(async {
        let fx = setup().await;
        for r in [rel("nope"), RelationRef::new("nope", "t")] {
            let o = objects(&fx, r).await;
            assert!(
                o.indexes().is_empty() && o.constraints().is_empty() && o.triggers().is_empty()
            );
        }
    })
    .await;
}

#[tokio::test]
async fn listing_relation_objects_leaves_a_read_only_session_idle() {
    bounded(async {
        let fx = setup().await;
        let mut session = fx.session(read_only()).await;
        session.relation_objects(&rel("t")).await.unwrap();
        assert_eq!(fx.db.scalar(STATE).await.as_deref(), Some("idle"));
        session.close().await.unwrap();
    })
    .await;
}

async fn schema_objects(fx: &PgFixture, schema: &str) -> SchemaObjects {
    let mut session = fx.session(read_only()).await;
    let o = session.schema_objects(schema).await.unwrap();
    session.close().await.unwrap();
    o
}
fn types(o: &SchemaObjects) -> Vec<(String, TypeKind)> {
    o.types().iter().map(|t| (s(t.name()), t.kind())).collect()
}
type RoutineRow = (String, RoutineKind, String, Option<String>);
fn routines(o: &SchemaObjects) -> Vec<RoutineRow> {
    o.routines()
        .iter()
        .map(|r| (s(r.name()), r.kind(), s(r.arguments()), r.returns().map(s)))
        .collect()
}

#[tokio::test]
async fn a_schema_lists_its_types_and_routines_without_implicit_ones() {
    bounded(async {
        let fx = setup().await;
        let o = schema_objects(&fx, SCHEMA).await;
        assert_eq!(
            types(&o),
            vec![
                (s("code"), TypeKind::Domain),
                (s("fr"), TypeKind::Range),
                (s("mood"), TypeKind::Enum),
                (s("pair"), TypeKind::Composite),
                (s("pos"), TypeKind::Domain),
            ]
        );
        let int = Some(s("integer"));
        let trigger = Some(s("trigger"));
        assert_eq!(
            routines(&o),
            vec![
                (
                    s("add"),
                    RoutineKind::Function,
                    s("text, text"),
                    Some(s("text"))
                ),
                (
                    s("add"),
                    RoutineKind::Function,
                    s("x integer, y integer"),
                    int.clone()
                ),
                (
                    s("bump"),
                    RoutineKind::Procedure,
                    s("INOUT n integer"),
                    None
                ),
                (
                    s("mymax"),
                    RoutineKind::Aggregate,
                    s("integer"),
                    int.clone()
                ),
                (s("mysum"), RoutineKind::Aggregate, s("integer"), int),
                (
                    s("rows_of"),
                    RoutineKind::Function,
                    s("n integer"),
                    Some(s("TABLE(i integer)"))
                ),
                (s("trg"), RoutineKind::Function, s(""), trigger.clone()),
                (s("vtrg"), RoutineKind::Function, s(""), trigger),
            ]
        );
        assert!(!o.truncated());
        let add = definition(&fx, &o.routines()[1].object_ref())
            .await
            .unwrap();
        assert!(
            add.text().starts_with(
                "CREATE OR REPLACE FUNCTION \"Obj\".add(x integer, y integer DEFAULT 1)"
            )
        );
        let mood = definition(&fx, &o.types()[2].object_ref()).await.unwrap();
        assert!(mood.composed());
    })
    .await;
}

#[tokio::test]
async fn extension_members_are_not_listed() {
    bounded(async {
        let fx = setup().await;
        let before = schema_objects(&fx, SCHEMA).await;
        fx.db
            .admin()
            .await
            .batch_execute(r#"CREATE EXTENSION pg_trgm SCHEMA "Obj""#)
            .await
            .unwrap();
        // Scoped to the fixture's schema, so plpgsql's own members cannot satisfy it.
        let members = "SELECT count(*)::text FROM pg_depend WHERE deptype = 'e' \
                       AND classid = 'pg_proc'::regclass AND objid IN \
                       (SELECT oid FROM pg_proc WHERE pronamespace = '\"Obj\"'::regnamespace)";
        assert_ne!(fx.db.scalar(members).await.as_deref(), Some("0"));
        assert_eq!(schema_objects(&fx, SCHEMA).await, before);
    })
    .await;
}

#[tokio::test]
async fn more_routines_than_the_cap_are_truncated() {
    bounded(async {
        let fx = pg_fixture("").await;
        fx.db
            .admin()
            .await
            .batch_execute(
                r#"CREATE SCHEMA "Many";
                DO $d$ BEGIN FOR i IN 1..10001 LOOP
                  EXECUTE format('CREATE FUNCTION "Many".f%s() RETURNS int LANGUAGE sql AS ''SELECT 1''', i);
                END LOOP; END $d$"#,
            )
            .await
            .unwrap();
        let o = schema_objects(&fx, "Many").await;
        assert_eq!((o.routines().len(), o.types().len(), o.truncated()), (RELATION_CAP, 0, true));
    })
    .await;
}

#[tokio::test]
async fn more_types_than_the_cap_are_truncated() {
    bounded(async {
        let fx = pg_fixture("").await;
        fx.db
            .admin()
            .await
            .batch_execute(
                r#"CREATE SCHEMA "ManyT";
                DO $d$ BEGIN FOR i IN 1..10001 LOOP
                  EXECUTE format('CREATE TYPE "ManyT".e%s AS ENUM ()', i);
                END LOOP; END $d$"#,
            )
            .await
            .unwrap();
        let o = schema_objects(&fx, "ManyT").await;
        assert_eq!(
            (o.types().len(), o.routines().len(), o.truncated()),
            (RELATION_CAP, 0, true)
        );
    })
    .await;
}

#[tokio::test]
async fn an_unknown_schema_has_no_objects() {
    bounded(async {
        let fx = setup().await;
        assert_eq!(
            schema_objects(&fx, "nope").await,
            SchemaObjects::new(Vec::new(), Vec::new(), false)
        );
    })
    .await;
}

#[tokio::test]
async fn listing_routines_without_pg_proc_gets_the_permission_error() {
    bounded(async {
        let mut fx = setup().await;
        let role = without_pg_proc(&mut fx).await;
        let mut session = fx.session(read_only()).await;
        let got = session.schema_objects(SCHEMA).await;
        session.close().await.unwrap();
        drop_role(&fx, &role).await;
        match got {
            Err(IntrospectError::Execute(ExecuteError::Database { code, .. })) => {
                assert_eq!(code, "42501")
            }
            other => panic!("{other:?}"),
        }
    })
    .await;
}

#[tokio::test]
async fn listed_arguments_open_their_routine_when_the_schema_is_on_the_path() {
    bounded(async {
        let fx = pg_fixture(r#"settings = { search_path = "onpath, public" }"#).await;
        fx.db.admin().await.batch_execute(ON_PATH).await.unwrap();
        let o = schema_objects(&fx, "onpath").await;
        assert_eq!(types(&o), vec![(s("kind"), TypeKind::Enum)]);
        assert_eq!(
            routines(&o),
            vec![(
                s("f"),
                RoutineKind::Function,
                s("m onpath.kind"),
                Some(s("text"))
            )]
        );
        definition(&fx, &o.routines()[0].object_ref())
            .await
            .unwrap();
    })
    .await;
}
