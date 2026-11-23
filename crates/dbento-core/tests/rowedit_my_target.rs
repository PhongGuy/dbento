//! MySQL and MariaDB edit targets: row identity, writable columns, and why a table cannot be edited.
use crate::support::{MyFixture, my_fixtures, run, text};
use dbento_core::{
    Browse, EditColumn, IntrospectError, Limits, NotEditable, SessionOptions, browse_sql,
};
use dbento_testkit::MyFlavor;

const SETUP: &str = "CREATE TABLE pk (id INT PRIMARY KEY, a INT, s INT AS (a * 2) STORED, \
  v INT AS (a + 1) VIRTUAL, c TIMESTAMP NULL DEFAULT CURRENT_TIMESTAMP, h INT INVISIBLE DEFAULT 7) ENGINE=InnoDB;
CREATE TABLE uq (a INT NULL, b INT NOT NULL, c VARCHAR(5) NOT NULL, UNIQUE KEY a_null (a), UNIQUE KEY z_bc (b, c)) ENGINE=InnoDB;
CREATE TABLE fl (f FLOAT PRIMARY KEY, n INT) ENGINE=InnoDB;
CREATE TABLE fl2 (f FLOAT PRIMARY KEY, n INT NOT NULL, UNIQUE KEY n_u (n)) ENGINE=InnoDB;
CREATE TABLE bt (b BIT(8) PRIMARY KEY, n INT) ENGINE=InnoDB;
CREATE TABLE dbl (d DOUBLE PRIMARY KEY, n INT) ENGINE=InnoDB;
CREATE TABLE nokey (a INT, b INT) ENGINE=InnoDB;
CREATE TABLE myi (id INT PRIMARY KEY) ENGINE=MyISAM;
CREATE TABLE mem (id INT PRIMARY KEY) ENGINE=MEMORY;
CREATE VIEW vw AS SELECT id, a FROM pk;";

fn names(cols: &[EditColumn]) -> Vec<(&str, bool)> {
    cols.iter().map(|c| (c.name(), c.editable())).collect()
}

async fn refused(fx: &MyFixture, label: &str, table: &str, why: NotEditable) {
    let t = fx.target(table).await;
    assert_eq!(t.not_editable(), Some(&why), "{label} {table}");
    assert!(
        t.identity().is_empty() && t.locators().is_empty(),
        "{label} {table}"
    );
    assert!(t.columns().iter().all(|c| !c.editable()), "{label} {table}");
}

#[tokio::test(flavor = "multi_thread")]
async fn keys_and_columns_follow_the_identity_rules() {
    for (label, fx) in my_fixtures("").await {
        fx.exec(SETUP).await;
        let pk = fx.target("pk").await;
        assert_eq!(pk.identity(), ["id"], "{label}");
        assert_eq!(pk.not_editable(), None, "{label}");
        assert_eq!(
            names(pk.columns()),
            [
                ("id", true),
                ("a", true),
                ("s", false),
                ("v", false),
                ("c", true)
            ],
            "{label}: generated columns are read-only, the invisible one is left out"
        );
        assert_eq!(fx.target("uq").await.identity(), ["b", "c"], "{label}");
        assert_eq!(fx.target("fl2").await.identity(), ["n"], "{label}");
        assert_eq!(fx.target("dbl").await.identity(), ["d"], "{label}");
        refused(&fx, &label, "fl", NotEditable::NoKey).await;
        refused(&fx, &label, "bt", NotEditable::NoKey).await;
        refused(&fx, &label, "nokey", NotEditable::NoKey).await;
        refused(&fx, &label, "vw", NotEditable::View).await;
        refused(
            &fx,
            &label,
            "myi",
            NotEditable::NonTransactional {
                engine: "MyISAM".to_owned(),
            },
        )
        .await;
        refused(
            &fx,
            &label,
            "mem",
            NotEditable::NonTransactional {
                engine: "MEMORY".to_owned(),
            },
        )
        .await;
        let mut s = fx.session(SessionOptions::default()).await;
        assert!(
            matches!(
                s.edit_target(&fx.rel("nope")).await,
                Err(IntrospectError::Missing { .. })
            ),
            "{label}"
        );
        s.close().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mariadb_sequences_and_system_versioned_tables_are_not_edited() {
    for (label, fx) in my_fixtures("").await {
        if fx.db.flavor() != MyFlavor::Mariadb {
            continue;
        }
        fx.exec("CREATE SEQUENCE sq; CREATE TABLE sv (k INT PRIMARY KEY, a INT) WITH SYSTEM VERSIONING;").await;
        refused(&fx, &label, "sq", NotEditable::Sequence).await;
        refused(&fx, &label, "sv", NotEditable::SystemVersioned).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invisible_generated_primary_key_is_selected_as_a_locator() {
    for (label, fx) in my_fixtures("").await {
        if fx.db.flavor() != MyFlavor::Mysql {
            continue;
        }
        fx.exec(
            "SET SESSION sql_generate_invisible_primary_key = ON; \
            CREATE TABLE gipk (a INT, b VARCHAR(5)); \
            SET SESSION sql_generate_invisible_primary_key = OFF; \
            INSERT INTO gipk (a, b) VALUES (1, 'x'), (2, 'y');",
        )
        .await;
        let t = fx.target("gipk").await;
        assert_eq!(t.identity(), ["my_row_id"], "{label}");
        assert_eq!(t.locators(), ["my_row_id"], "{label}");
        assert!(t.column("my_row_id").is_none(), "{label}");
        let sql = browse_sql(&t, &Browse::default()).unwrap();
        let db = fx.db.database();
        assert_eq!(
            sql,
            format!(
                "SELECT `my_row_id`, `{db}`.`gipk`.*\nFROM `{db}`.`gipk`\nORDER BY `my_row_id` ASC"
            )
        );
        let mut s = fx.session(SessionOptions::default()).await;
        let ran = run(&mut s, &sql, Limits::default())
            .await
            .remove(0)
            .unwrap();
        s.close().await.unwrap();
        let cols: Vec<&str> = ran.columns.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(cols, ["my_row_id", "a", "b"], "{label}");
        assert_eq!(
            ran.rows,
            vec![
                vec![text("1"), text("1"), text("x")],
                vec![text("2"), text("2"), text("y")],
            ],
            "{label}"
        );
    }
}
