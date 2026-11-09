//! SQL Server keys, indexes, checks, triggers and foreign keys, read from the `sys` views of
//! the connection's database with names compared as the database compares them.

use crate::support::{MsFixture, ms_fixture, ms_off};
use dbento_core::{
    ConstraintKind, FkDirection, ForeignKey, IndexInfo, RelationRef, Session, SessionOptions,
    TriggerInfo, TriggerState,
};

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

/// `orders` holds a clustered unique index that is no constraint, a nonclustered primary key
/// whose key order differs from the table's, a filtered index with an included column and a
/// nonclustered columnstore index; `ref_lines` references `lines` in a different column order.
const SHOP: &[&str] = &[
    "CREATE SCHEMA sales",
    "CREATE TABLE sales.customers (id int IDENTITY CONSTRAINT pk_c PRIMARY KEY, \
     email nvarchar(50) NOT NULL CONSTRAINT uq_email UNIQUE)",
    "CREATE TABLE sales.orders (id int NOT NULL, line int NOT NULL, cid int NOT NULL, \
     status varchar(20) NOT NULL, amount decimal(10,2) NOT NULL, \
     CONSTRAINT pk_o PRIMARY KEY NONCLUSTERED (line, id), \
     CONSTRAINT fk_o_c FOREIGN KEY (cid) REFERENCES sales.customers (id) ON DELETE CASCADE, \
     CONSTRAINT ck_amt CHECK (amount >= 0), CONSTRAINT ck_two CHECK (id <> line))",
    "CREATE UNIQUE CLUSTERED INDEX ix_c ON sales.orders (id DESC, line)",
    "CREATE INDEX ix_f ON sales.orders (status DESC, cid) INCLUDE (amount) \
     WHERE status <> 'void'",
    "CREATE NONCLUSTERED COLUMNSTORE INDEX ix_cs ON sales.orders (amount, cid)",
    "CREATE TABLE sales.tree (id int PRIMARY KEY, parent int NULL, \
     CONSTRAINT fk_self FOREIGN KEY (parent) REFERENCES sales.tree (id))",
    "CREATE TABLE sales.lines (a int NOT NULL, b int NOT NULL, CONSTRAINT pk_l PRIMARY KEY (a, b))",
    "CREATE TABLE sales.ref_lines (p int NOT NULL, q int NOT NULL, \
     CONSTRAINT fk_l FOREIGN KEY (p, q) REFERENCES sales.lines (a, b))",
    "CREATE TRIGGER sales.tr_on ON sales.orders AFTER INSERT AS RETURN",
    "CREATE TRIGGER sales.tr_off ON sales.orders AFTER UPDATE AS RETURN",
    "DISABLE TRIGGER sales.tr_off ON sales.orders",
    "CREATE VIEW sales.v AS SELECT id FROM sales.orders",
    "CREATE TRIGGER sales.tr_v ON sales.v INSTEAD OF DELETE AS RETURN",
];

fn cols(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

fn rel(name: &str) -> RelationRef {
    RelationRef::new("sales", name)
}

#[tokio::test]
async fn indexes_list_key_columns_in_key_order_with_their_kinds() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(&fx, SHOP).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let orders = rel("orders");
    let objects = s.relation_objects(&orders).await.unwrap();
    let index = |name, c: &[&str], unique, primary, partial, method: &str| {
        IndexInfo::new(
            orders.clone(),
            name,
            cols(c),
            unique,
            primary,
            partial,
            Some(method.to_owned()),
        )
    };
    assert_eq!(
        objects.indexes(),
        [
            index("ix_c", &["id", "line"], true, false, false, "clustered"),
            index(
                "ix_cs",
                &["amount", "cid"],
                false,
                false,
                false,
                "nonclustered columnstore"
            ),
            index(
                "ix_f",
                &["status", "cid"],
                false,
                false,
                true,
                "nonclustered"
            ),
            index("pk_o", &["line", "id"], true, true, false, "nonclustered"),
        ]
    );
    assert_eq!(objects.unreadable_reason(), None);
}

#[tokio::test]
async fn constraints_come_by_kind_then_name_with_columns_in_key_order() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(&fx, SHOP).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let objects = s.relation_objects(&rel("orders")).await.unwrap();
    let got: Vec<_> = objects
        .constraints()
        .iter()
        .map(|c| {
            (
                c.kind(),
                c.name().unwrap().to_owned(),
                c.columns().to_vec(),
                c.referenced().cloned(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                ConstraintKind::PrimaryKey,
                "pk_o".to_owned(),
                cols(&["line", "id"]),
                None
            ),
            (
                ConstraintKind::ForeignKey,
                "fk_o_c".to_owned(),
                cols(&["cid"]),
                Some(rel("customers"))
            ),
            // A check on two columns names none; a check on one names it.
            (
                ConstraintKind::Check,
                "ck_amt".to_owned(),
                cols(&["amount"]),
                None
            ),
            (ConstraintKind::Check, "ck_two".to_owned(), cols(&[]), None),
        ]
    );
    let customers = s.relation_objects(&rel("customers")).await.unwrap();
    let kinds: Vec<_> = customers.constraints().iter().map(|c| c.kind()).collect();
    assert_eq!(kinds, [ConstraintKind::PrimaryKey, ConstraintKind::Unique]);
}

#[tokio::test]
async fn triggers_report_their_state_on_tables_and_views() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(&fx, SHOP).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let orders = rel("orders");
    let objects = s.relation_objects(&orders).await.unwrap();
    assert_eq!(
        objects.triggers(),
        [
            TriggerInfo::new(orders.clone(), "tr_off", TriggerState::Disabled),
            TriggerInfo::new(orders.clone(), "tr_on", TriggerState::Enabled),
        ]
    );
    let view = s.relation_objects(&rel("v")).await.unwrap();
    assert_eq!(
        view.triggers(),
        [TriggerInfo::new(rel("v"), "tr_v", TriggerState::Enabled)]
    );
    assert!(view.indexes().is_empty() && view.constraints().is_empty());
}

#[tokio::test]
async fn an_unknown_relation_has_empty_lists_and_no_keys() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(&fx, SHOP).await;
    let mut s = fx.session(SessionOptions::default()).await;
    for missing in [rel("nope"), RelationRef::new("nope", "orders")] {
        let objects = s.relation_objects(&missing).await.unwrap();
        assert!(objects.indexes().is_empty() && objects.constraints().is_empty());
        assert!(objects.triggers().is_empty());
        assert!(s.foreign_keys(&missing).await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn foreign_keys_read_both_ways_in_constraint_column_order() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(&fx, SHOP).await;
    let mut s = fx.session(SessionOptions::default()).await;
    let key = |name: &str, from: &str, c: &[&str], to: &str, r: &[&str], d| {
        ForeignKey::new(
            Some(name.to_owned()),
            rel(from),
            cols(c),
            rel(to),
            cols(r),
            d,
        )
    };
    assert_eq!(
        s.foreign_keys(&rel("orders")).await.unwrap(),
        [key(
            "fk_o_c",
            "orders",
            &["cid"],
            "customers",
            &["id"],
            FkDirection::Outgoing
        )]
    );
    assert_eq!(
        s.foreign_keys(&rel("customers")).await.unwrap(),
        [key(
            "fk_o_c",
            "orders",
            &["cid"],
            "customers",
            &["id"],
            FkDirection::Incoming
        )]
    );
    assert_eq!(
        s.foreign_keys(&rel("lines")).await.unwrap(),
        [key(
            "fk_l",
            "ref_lines",
            &["p", "q"],
            "lines",
            &["a", "b"],
            FkDirection::Incoming
        )]
    );
    assert_eq!(
        s.foreign_keys(&rel("tree")).await.unwrap(),
        [
            key(
                "fk_self",
                "tree",
                &["parent"],
                "tree",
                &["id"],
                FkDirection::Outgoing
            ),
            key(
                "fk_self",
                "tree",
                &["parent"],
                "tree",
                &["id"],
                FkDirection::Incoming
            ),
        ]
    );
}

#[tokio::test]
async fn a_read_only_session_reads_the_objects_and_keeps_its_fence() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(&fx, SHOP).await;
    let mut o = SessionOptions::default();
    o.force_read_only = true;
    let mut s = fx.session(o).await;
    assert_eq!(
        s.relation_objects(&rel("orders"))
            .await
            .unwrap()
            .indexes()
            .len(),
        4
    );
    assert_eq!(s.foreign_keys(&rel("orders")).await.unwrap().len(), 1);
    s.settle().await.unwrap();
}

/// Name, columns and method of each index of `sales.<name>`.
async fn listed(s: &mut Session, name: &str) -> Vec<(String, Vec<String>, Option<String>)> {
    let objects = s.relation_objects(&rel(name)).await.unwrap();
    objects
        .indexes()
        .iter()
        .map(|i| {
            (
                i.name().to_owned(),
                i.columns().to_vec(),
                i.method().map(str::to_owned),
            )
        })
        .collect()
}

/// XML, spatial and clustered columnstore indexes have no key columns, and a partitioned index
/// carries its partition column with no key position.
#[tokio::test]
async fn keyless_index_kinds_list_their_column_and_a_partition_column_stays_hidden() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(
        &fx,
        &[
            "CREATE SCHEMA sales",
            "CREATE TABLE sales.docs (id int CONSTRAINT pk_d PRIMARY KEY, x xml, g geometry)",
            "CREATE PRIMARY XML INDEX ix_x ON sales.docs (x)",
            "CREATE SPATIAL INDEX ix_g ON sales.docs (g) \
             WITH (BOUNDING_BOX = (0, 0, 10, 10))",
            "CREATE TABLE sales.cold (a int, b int, INDEX ix_cc CLUSTERED COLUMNSTORE)",
            "CREATE PARTITION FUNCTION pf (int) AS RANGE LEFT FOR VALUES (10)",
            "CREATE PARTITION SCHEME ps AS PARTITION pf ALL TO ([PRIMARY])",
            "CREATE TABLE sales.parts (id int NOT NULL, p int NOT NULL)",
            "CREATE INDEX ix_part ON sales.parts (id) ON ps (p)",
        ],
    )
    .await;
    let mut s = fx.session(SessionOptions::default()).await;
    let entry =
        |name: &str, c: &[&str], method: &str| (name.to_owned(), cols(c), Some(method.to_owned()));
    assert_eq!(
        listed(&mut s, "docs").await,
        [
            entry("ix_g", &["g"], "spatial"),
            entry("ix_x", &["x"], "xml"),
            entry("pk_d", &["id"], "clustered"),
        ]
    );
    assert_eq!(
        listed(&mut s, "cold").await,
        [entry("ix_cc", &["a", "b"], "clustered columnstore")]
    );
    assert_eq!(
        listed(&mut s, "parts").await,
        [entry("ix_part", &["id"], "nonclustered")]
    );
}

#[tokio::test]
async fn a_key_to_a_table_out_of_sight_keeps_its_local_side_as_a_constraint() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_fixture(
        dbento_testkit::mssql_db().await,
        "tls = \"off\"\nread_only = true\nread_only_user = \"r\"",
    );
    setup(&fx, SHOP).await;
    setup(
        &fx,
        &[
            "CREATE USER r WITHOUT LOGIN",
            "GRANT SELECT ON sales.orders TO r",
        ],
    )
    .await;
    let mut s = fx.session(SessionOptions::default()).await;
    let orders = rel("orders");
    let objects = s.relation_objects(&orders).await.unwrap();
    let keys: Vec<_> = objects
        .constraints()
        .iter()
        .filter(|c| c.kind() == ConstraintKind::ForeignKey)
        .map(|c| (c.name(), c.columns().to_vec(), c.referenced().cloned()))
        .collect();
    assert_eq!(keys, [(Some("fk_o_c"), cols(&["cid"]), None)]);
    // With no far name there is nothing to follow, so the key list leaves it out.
    assert!(s.foreign_keys(&orders).await.unwrap().is_empty());
}

#[tokio::test]
async fn an_edge_constraint_is_listed_as_another_kind() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    setup(
        &fx,
        &[
            "CREATE SCHEMA sales",
            "CREATE TABLE sales.n1 (id int PRIMARY KEY) AS NODE",
            "CREATE TABLE sales.n2 (id int PRIMARY KEY) AS NODE",
            "CREATE TABLE sales.e (CONSTRAINT ec_e CONNECTION (sales.n1 TO sales.n2)) AS EDGE",
        ],
    )
    .await;
    let mut s = fx.session(SessionOptions::default()).await;
    let objects = s.relation_objects(&rel("e")).await.unwrap();
    let got: Vec<_> = objects
        .constraints()
        .iter()
        .map(|c| {
            (
                c.kind(),
                c.name(),
                c.columns().to_vec(),
                c.referenced().cloned(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [(ConstraintKind::Other, Some("ec_e"), cols(&[]), None)]
    );
}
