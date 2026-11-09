//! MySQL keys, indexes, checks, triggers and foreign keys on both flavors, with names
//! compared as the server compares them.

use crate::support::{MyFixture, my_fixtures};
use dbento_core::{
    ConstraintInfo, ConstraintKind, FkDirection, ForeignKey, IndexInfo, RelationObjects,
    RelationRef, SessionOptions, TriggerInfo, TriggerState,
};
use mysql_async::prelude::*;

/// InnoDB names each foreign key's own index after the constraint. `P1` and `p1` differ only
/// in case, and `r1` references each.
const SETUP: &str = "CREATE TABLE parent (id INT PRIMARY KEY, code VARCHAR(10) NOT NULL, UNIQUE KEY parent_code (code));
CREATE TABLE pair (a INT, b VARCHAR(5), PRIMARY KEY (a, b));
CREATE TABLE child (
  id INT PRIMARY KEY, parent_id INT, pa INT, pb VARCHAR(5), note VARCHAR(100), up_id INT,
  KEY child_note (note(10), id),
  CONSTRAINT child_parent FOREIGN KEY (parent_id) REFERENCES parent (id) ON DELETE CASCADE,
  CONSTRAINT child_pair FOREIGN KEY (pa, pb) REFERENCES pair (a, b),
  CONSTRAINT child_up FOREIGN KEY (up_id) REFERENCES child (id),
  CONSTRAINT note_short CHECK (CHAR_LENGTH(note) < 100));
CREATE TRIGGER child_bi BEFORE INSERT ON child FOR EACH ROW SET NEW.note = COALESCE(NEW.note, 'x');
CREATE TRIGGER child_au AFTER UPDATE ON child FOR EACH ROW SET @n = 1;
CREATE TABLE P1 (id INT PRIMARY KEY);
CREATE TABLE p1 (id INT PRIMARY KEY);
CREATE TABLE r1 (a INT, b INT, CONSTRAINT r1_big FOREIGN KEY (a) REFERENCES P1 (id),
  CONSTRAINT r1_small FOREIGN KEY (b) REFERENCES p1 (id));";

async fn setup(fx: &MyFixture) {
    let mut c = fx.db.admin().await;
    c.query_drop(SETUP).await.unwrap();
    c.disconnect().await.unwrap();
}

fn cols(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

fn index(rel: &RelationRef, name: &str, c: &[&str], unique: bool) -> IndexInfo {
    let primary = name == "PRIMARY";
    IndexInfo::new(
        rel.clone(),
        name,
        cols(c),
        unique,
        primary,
        false,
        Some("btree".to_owned()),
    )
}

fn key(
    rel: &RelationRef,
    name: &str,
    kind: ConstraintKind,
    c: &[&str],
    referenced: Option<RelationRef>,
) -> ConstraintInfo {
    ConstraintInfo::new(
        rel.clone(),
        Some(name.to_owned()),
        kind,
        cols(c),
        referenced,
    )
}

fn fk(
    name: &str,
    table: &RelationRef,
    c: &[&str],
    referenced: &RelationRef,
    rc: &[&str],
    d: FkDirection,
) -> ForeignKey {
    ForeignKey::new(
        Some(name.to_owned()),
        table.clone(),
        cols(c),
        referenced.clone(),
        cols(rc),
        d,
    )
}

#[tokio::test]
async fn relation_objects_list_indexes_keys_checks_and_triggers() {
    for (label, fx) in my_fixtures("").await {
        setup(&fx).await;
        let mut s = fx.session(SessionOptions::default()).await;
        let at = |n: &str| RelationRef::new(fx.db.database(), n);
        let child = at("child");
        let want = RelationObjects::new(
            vec![
                index(&child, "PRIMARY", &["id"], true),
                index(&child, "child_note", &["note", "id"], false),
                index(&child, "child_pair", &["pa", "pb"], false),
                index(&child, "child_parent", &["parent_id"], false),
                index(&child, "child_up", &["up_id"], false),
            ],
            vec![
                key(&child, "PRIMARY", ConstraintKind::PrimaryKey, &["id"], None),
                key(
                    &child,
                    "child_pair",
                    ConstraintKind::ForeignKey,
                    &["pa", "pb"],
                    Some(at("pair")),
                ),
                key(
                    &child,
                    "child_parent",
                    ConstraintKind::ForeignKey,
                    &["parent_id"],
                    Some(at("parent")),
                ),
                key(
                    &child,
                    "child_up",
                    ConstraintKind::ForeignKey,
                    &["up_id"],
                    Some(at("child")),
                ),
                key(&child, "note_short", ConstraintKind::Check, &[], None),
            ],
            vec![
                TriggerInfo::new(child.clone(), "child_au", TriggerState::Enabled),
                TriggerInfo::new(child.clone(), "child_bi", TriggerState::Enabled),
            ],
        );
        assert_eq!(s.relation_objects(&child).await.unwrap(), want, "{label}");
        let parent = at("parent");
        assert_eq!(
            s.relation_objects(&parent).await.unwrap().constraints(),
            [
                key(
                    &parent,
                    "PRIMARY",
                    ConstraintKind::PrimaryKey,
                    &["id"],
                    None
                ),
                key(
                    &parent,
                    "parent_code",
                    ConstraintKind::Unique,
                    &["code"],
                    None
                ),
            ],
            "{label}"
        );
        let small = s.relation_objects(&at("p1")).await.unwrap();
        assert_eq!(
            small.indexes(),
            [index(&at("p1"), "PRIMARY", &["id"], true)],
            "{label}"
        );
        assert_eq!(
            small.constraints().len(),
            1,
            "{label}: P1's key is not p1's"
        );
        assert_eq!(
            s.relation_objects(&at("nope")).await.unwrap(),
            RelationObjects::new(Vec::new(), Vec::new(), Vec::new()),
            "{label}"
        );
    }
}

#[tokio::test]
async fn foreign_keys_point_both_ways_and_never_match_another_case() {
    for (label, fx) in my_fixtures("").await {
        setup(&fx).await;
        let mut s = fx.session(SessionOptions::default()).await;
        let at = |n: &str| RelationRef::new(fx.db.database(), n);
        let (child, pair, parent) = (at("child"), at("pair"), at("parent"));
        assert_eq!(
            s.foreign_keys(&child).await.unwrap(),
            vec![
                fk(
                    "child_up",
                    &child,
                    &["up_id"],
                    &child,
                    &["id"],
                    FkDirection::Outgoing
                ),
                fk(
                    "child_pair",
                    &child,
                    &["pa", "pb"],
                    &pair,
                    &["a", "b"],
                    FkDirection::Outgoing
                ),
                fk(
                    "child_parent",
                    &child,
                    &["parent_id"],
                    &parent,
                    &["id"],
                    FkDirection::Outgoing
                ),
                fk(
                    "child_up",
                    &child,
                    &["up_id"],
                    &child,
                    &["id"],
                    FkDirection::Incoming
                ),
            ],
            "{label}"
        );
        let (r1, small, big) = (at("r1"), at("p1"), at("P1"));
        assert_eq!(
            s.foreign_keys(&small).await.unwrap(),
            vec![fk(
                "r1_small",
                &r1,
                &["b"],
                &small,
                &["id"],
                FkDirection::Incoming
            )],
            "{label}"
        );
        assert_eq!(
            s.foreign_keys(&big).await.unwrap(),
            vec![fk(
                "r1_big",
                &r1,
                &["a"],
                &big,
                &["id"],
                FkDirection::Incoming
            )],
            "{label}"
        );
        assert!(
            s.foreign_keys(&at("nope")).await.unwrap().is_empty(),
            "{label}"
        );
    }
}
