//! MySQL browse tabs: a table that cannot be edited says why on its chip and keeps its keys.
use crate::support::{self, wait_real};
use dbento_app::tab_model::TabModel;
use dbento_core::RelationRef;
use gpui_kit::{AppContext, Entity, TestAppContext};

/// Opens `rel` as a browse tab and waits until its own browse result has landed.
fn browse(cx: &mut TestAppContext, m: &Entity<TabModel>, rel: RelationRef) {
    let name = rel.name().to_owned();
    m.update(cx, |m, cx| m.load_target(rel, cx));
    wait_real(cx, "the browse", |cx| {
        m.read_with(cx, |m, _| {
            m.browse()
                .is_some_and(|b| b.target().relation().name() == name)
                && m.browse_result()
                && !m.running()
        })
    });
}

#[gpui_kit::test]
fn a_mysql_table_that_cannot_be_edited_names_why_and_keeps_its_links(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let rt = cx.update(|cx| dbento_app::tokio_bridge::handle(cx));
    let _enter = rt.enter();
    let db = rt.block_on(dbento_testkit::mysql_db());
    let (_dir, spec) = support::my_spec(&db, "");
    let m = cx.new(|_| TabModel::new(spec));
    cx.update(|cx| {
        cx.subscribe(&m, |m, e: &dbento_app::browse::BrowseEvent, cx| {
            if let dbento_app::browse::BrowseEvent::Run(sql) = e {
                let sql = sql.clone();
                m.update(cx, |m, cx| m.run(sql, cx));
            }
        })
        .detach();
    });
    let ddl = "CREATE TABLE p (id INT PRIMARY KEY); CREATE TABLE c (pid INT, FOREIGN KEY (pid) REFERENCES p (id)); \
        CREATE TABLE m (id INT PRIMARY KEY) ENGINE=MyISAM; INSERT INTO p VALUES (1); INSERT INTO c VALUES (1)";
    m.update(cx, |m, cx| m.run(ddl.into(), cx));
    wait_real(cx, "the setup", |cx| m.read_with(cx, |m, _| !m.running()));

    browse(cx, &m, RelationRef::new(db.database(), "c"));
    m.read_with(cx, |m, _| {
        assert_eq!(m.not_editable(), Some("Read-only: the table has no primary key or NOT NULL unique key that can identify a row"));
        assert_eq!(m.browse().unwrap().foreign_keys().len(), 1);
        assert!(!m.editable());
    });
    browse(cx, &m, RelationRef::new(db.database(), "m"));
    m.read_with(cx, |m, _| {
        assert_eq!(
            m.not_editable(),
            Some("Read-only: MyISAM tables cannot roll back a change")
        )
    });
    browse(cx, &m, RelationRef::new(db.database(), "p"));
    m.read_with(cx, |m, _| {
        assert_eq!(m.not_editable(), None);
        assert!(m.editable());
        assert_eq!(
            m.browse().unwrap().foreign_keys().len(),
            1,
            "the incoming key from c"
        );
    });
}
