//! MySQL foreign keys as browse links: keys over BIT or FLOAT columns are not offered, and keys
//! load for a table that cannot be edited.
use crate::support::{my_fixtures, run, text};
use dbento_core::{
    Browse, EditValue, Engine, FkDirection, Limits, NotEditable, SessionOptions, browse_sql,
    key_filter,
};

const SETUP: &str = "CREATE TABLE bp (b BIT(8) PRIMARY KEY, id INT NOT NULL UNIQUE) ENGINE=InnoDB;
CREATE TABLE fp (f FLOAT PRIMARY KEY) ENGINE=InnoDB;
CREATE TABLE bc (k INT, pb BIT(8), pid INT, pf FLOAT,
  CONSTRAINT bc_bit FOREIGN KEY (pb) REFERENCES bp (b),
  CONSTRAINT bc_id FOREIGN KEY (pid) REFERENCES bp (id),
  CONSTRAINT bc_float FOREIGN KEY (pf) REFERENCES fp (f)) ENGINE=InnoDB;
INSERT INTO bp VALUES (b'101', 1); INSERT INTO fp VALUES (1.1); INSERT INTO bc VALUES (1, b'101', 1, 1.1);";

#[tokio::test(flavor = "multi_thread")]
async fn bit_and_float_keys_are_dropped_and_a_keyless_table_still_links() {
    for (label, fx) in my_fixtures("").await {
        fx.exec(SETUP).await;
        let mut s = fx.session(SessionOptions::default()).await;
        let bc = s.edit_target(&fx.rel("bc")).await.unwrap();
        assert_eq!(bc.not_editable(), Some(&NotEditable::NoKey), "{label}");
        let out = s.foreign_keys(&fx.rel("bc")).await.unwrap();
        assert_eq!(out.len(), 1, "{label}: {out:?}");
        assert_eq!(
            (out[0].name(), out[0].direction()),
            (Some("bc_id"), FkDirection::Outgoing),
            "{label}"
        );
        let incoming = s.foreign_keys(&fx.rel("bp")).await.unwrap();
        assert_eq!(incoming.len(), 1, "{label}: {incoming:?}");
        assert_eq!(
            (incoming[0].name(), incoming[0].direction()),
            (Some("bc_id"), FkDirection::Incoming),
            "{label}"
        );
        let float_side = s.foreign_keys(&fx.rel("fp")).await.unwrap();
        assert!(float_side.is_empty(), "{label}: {float_side:?}");

        // Following bc_id from the row: the far table's browse filtered on the key.
        let bp = s.edit_target(&fx.rel("bp")).await.unwrap();
        assert_eq!(
            bp.identity(),
            ["id"],
            "{label}: the BIT primary key is passed over"
        );
        let filter = key_filter(
            Engine::Mysql,
            &[("id".to_owned(), EditValue::Text("1".to_owned()))],
        );
        let sql = browse_sql(&bp, &Browse::new(filter, None)).unwrap();
        let ran = run(&mut s, &sql, Limits::default())
            .await
            .remove(0)
            .unwrap();
        assert_eq!(ran.rows.len(), 1, "{label}");
        assert_eq!(ran.rows[0][1], text("1"), "{label}");
        s.close().await.unwrap();
    }
}
