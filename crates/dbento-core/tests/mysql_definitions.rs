//! MySQL routines and definitions on both flavors: the server's SHOW CREATE text, and
//! Missing once the object is gone.
mod support;

use dbento_core::{
    Definition, DefinitionError, ObjectRef, RelationRef, RoutineInfo, RoutineKind, SchemaObjects,
    SessionOptions,
};
use mysql_async::prelude::*;
use support::{MyFixture, my_fixtures};

const SETUP: &str = "CREATE TABLE parent (id INT PRIMARY KEY, code VARCHAR(10) NOT NULL, UNIQUE KEY parent_code (code));
CREATE TABLE child (id INT PRIMARY KEY, parent_id INT, note TEXT,
  CONSTRAINT child_parent FOREIGN KEY (parent_id) REFERENCES parent (id));
CREATE VIEW v AS SELECT id FROM parent;
CREATE TRIGGER child_bi BEFORE INSERT ON child FOR EACH ROW SET NEW.note = COALESCE(NEW.note, 'x');
CREATE PROCEDURE p(IN a INT, OUT b TEXT) SELECT a;
CREATE PROCEDURE q() SELECT 1;
CREATE FUNCTION f(x INT) RETURNS INT DETERMINISTIC RETURN x + 1;";

async fn admin(fx: &MyFixture, sql: &str) {
    let mut c = fx.db.admin().await;
    c.query_drop(sql).await.unwrap();
    c.disconnect().await.unwrap();
}

fn text(d: Result<Definition, DefinitionError>) -> String {
    let d = d.unwrap();
    assert!(!d.truncated() && !d.composed(), "{d:?}");
    d.text().to_owned()
}

#[tokio::test]
async fn routines_list_with_their_arguments_and_results() {
    for (label, fx) in my_fixtures("").await {
        admin(&fx, SETUP).await;
        let mut s = fx.session(SessionOptions::default()).await;
        let db = fx.db.database();
        let routines = vec![
            RoutineInfo::new(
                db,
                "f",
                RoutineKind::Function,
                "x int",
                Some("int".to_owned()),
            ),
            RoutineInfo::new(db, "p", RoutineKind::Procedure, "a int, OUT b text", None),
            RoutineInfo::new(db, "q", RoutineKind::Procedure, "", None),
        ];
        assert_eq!(
            s.schema_objects(db).await.unwrap(),
            SchemaObjects::new(Vec::new(), routines, false),
            "{label}"
        );
        assert_eq!(
            s.schema_objects("dbento_no_such_schema").await.unwrap(),
            SchemaObjects::new(Vec::new(), Vec::new(), false),
            "{label}"
        );
    }
}

#[tokio::test]
async fn definitions_are_the_servers_create_text() {
    for (label, fx) in my_fixtures("").await {
        admin(&fx, SETUP).await;
        let mut s = fx.session(SessionOptions::default()).await;
        let at = |n: &str| RelationRef::new(fx.db.database(), n);
        let parent = text(s.definition(&ObjectRef::relation(at("parent"))).await);
        assert!(
            parent.starts_with("CREATE TABLE `parent` (") && parent.ends_with(';'),
            "{label}: {parent}"
        );
        assert!(
            parent.contains("UNIQUE KEY `parent_code` (`code`)"),
            "{label}: {parent}"
        );
        let view = text(s.definition(&ObjectRef::relation(at("v"))).await);
        assert!(view.contains("VIEW `v` AS select"), "{label}: {view}");
        let objects = s.relation_objects(&at("parent")).await.unwrap();
        let index = objects
            .indexes()
            .iter()
            .find(|i| i.name() == "parent_code")
            .unwrap();
        assert_eq!(
            text(s.definition(&index.object_ref()).await),
            parent,
            "{label}"
        );
        let child = s.relation_objects(&at("child")).await.unwrap();
        let key = child
            .constraints()
            .iter()
            .find(|c| c.name() == Some("child_parent"))
            .unwrap();
        let table = text(s.definition(&key.object_ref()).await);
        assert!(
            table.contains(
                "CONSTRAINT `child_parent` FOREIGN KEY (`parent_id`) REFERENCES `parent` (`id`)"
            ),
            "{label}: {table}"
        );
        let trigger = text(s.definition(&child.triggers()[0].object_ref()).await);
        assert!(
            trigger.contains("TRIGGER") && trigger.contains("BEFORE INSERT ON"),
            "{label}: {trigger}"
        );
        let routines = s.schema_objects(fx.db.database()).await.unwrap();
        let f = text(s.definition(&routines.routines()[0].object_ref()).await);
        assert!(
            f.contains("FUNCTION `f`(x INT) RETURNS int"),
            "{label}: {f}"
        );
        let p = text(s.definition(&routines.routines()[1].object_ref()).await);
        assert!(
            p.contains("PROCEDURE `p`(IN a INT, OUT b TEXT)"),
            "{label}: {p}"
        );
    }
}

#[tokio::test]
async fn objects_that_are_gone_are_missing() {
    for (label, fx) in my_fixtures("").await {
        admin(&fx, SETUP).await;
        let mut s = fx.session(SessionOptions::default()).await;
        let at = |n: &str| RelationRef::new(fx.db.database(), n);
        let trigger = s.relation_objects(&at("child")).await.unwrap().triggers()[0].object_ref();
        let q = s.schema_objects(fx.db.database()).await.unwrap().routines()[2].object_ref();
        admin(&fx, "DROP TRIGGER child_bi; DROP PROCEDURE q").await;
        for object in [
            ObjectRef::relation(at("nope")),
            ObjectRef::relation(RelationRef::new("dbento_no_such_schema", "t")),
            trigger,
            q,
        ] {
            assert!(
                matches!(
                    s.definition(&object).await,
                    Err(DefinitionError::Missing { .. })
                ),
                "{label}: {object:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_function_wins_over_a_procedure_of_the_same_name() {
    for (label, fx) in my_fixtures("").await {
        admin(
            &fx,
            "CREATE FUNCTION g(x INT) RETURNS INT DETERMINISTIC RETURN x;
             CREATE PROCEDURE g(x INT) SELECT x;",
        )
        .await;
        let mut s = fx.session(SessionOptions::default()).await;
        let listed = s.schema_objects(fx.db.database()).await.unwrap();
        let g = listed.routines();
        assert_eq!(g.len(), 2, "{label}: {g:?}");
        assert!(
            g.iter()
                .all(|r| r.name() == "g" && r.arguments() == "x int"),
            "{label}: {g:?}"
        );
        let kinds: Vec<RoutineKind> = g.iter().map(|r| r.kind()).collect();
        assert!(kinds.contains(&RoutineKind::Function), "{label}: {kinds:?}");
        assert!(
            kinds.contains(&RoutineKind::Procedure),
            "{label}: {kinds:?}"
        );
        assert_eq!(g[0].object_ref(), g[1].object_ref(), "{label}");
        let body = text(s.definition(&g[0].object_ref()).await);
        assert!(
            body.contains("FUNCTION `g`(x INT) RETURNS int"),
            "{label}: {body}"
        );
    }
}
