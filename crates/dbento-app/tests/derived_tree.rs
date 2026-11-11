//! Derived child connections in the schema tree model.
use dbento_app::catalog::{CatalogData, CatalogQuery, CatalogReply};
use dbento_app::schema_tree::{CatalogTree, LoadRequest};

fn answer(t: &mut CatalogTree, req: LoadRequest, result: Result<CatalogData, String>) {
    t.apply(
        &req.conn,
        CatalogReply {
            generation: req.generation,
            query: req.query,
            result,
        },
    );
}

#[test]
fn a_databases_reply_is_cached_and_ensure_asks_once() {
    let mut t = CatalogTree::new();
    let req = t
        .ensure("srv", &CatalogQuery::Databases)
        .expect("absent, so it loads");
    assert!(t.ensure("srv", &CatalogQuery::Databases).is_none());
    assert_eq!(t.databases("srv"), None);
    answer(&mut t, req, Ok(CatalogData::Databases(vec!["app".into()])));
    assert_eq!(t.databases("srv"), Some(&["app".to_owned()][..]));
    let req = t.refresh_connection("srv");
    assert!(req.is_empty(), "nothing is expanded");
    assert_eq!(t.databases("srv"), None, "a refresh drops the list");
}
