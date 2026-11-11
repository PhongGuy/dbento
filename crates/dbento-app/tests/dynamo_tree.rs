//! The DynamoDB explorer's Open: the table alone, and an index after its table.
use dbento_app::schema_tree::{
    ObjectGroup, RowKind, catalog_entries, index_select_sql, select_sql,
};
use dbento_core::{Engine, IndexInfo, RelationKind, RelationRef};
use gpui_kit::TestAppContext;

const R: &str = "eu-west-1";

#[test]
fn open_text_names_the_table_alone_and_an_index_after_it() {
    let rel = RelationRef::new(R, "Order \"Items\"");
    assert_eq!(
        select_sql(Engine::Dynamodb, &rel),
        "SELECT * FROM \"Order \"\"Items\"\"\""
    );
    assert_eq!(
        index_select_sql(
            Engine::Dynamodb,
            &RelationRef::new(R, "orders"),
            "by_status"
        ),
        "SELECT * FROM \"orders\".\"by_status\""
    );
}

#[gpui_kit::test]
fn a_dynamodb_index_row_opens_and_a_table_keeps_no_sql_items(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let index = IndexInfo::new(
            RelationRef::new(R, "orders"),
            "by_status",
            vec!["status".into(), "created".into()],
            false,
            false,
            false,
            Some("GSI".into()),
        );
        let row = RowKind::Object {
            group: ObjectGroup::Indexes,
            object: index.object_ref(),
            detail: "GSI".into(),
        };
        let ids: Vec<&str> = catalog_entries(&row, Engine::Dynamodb, cx)
            .iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(ids, ["tree-menu-open", "tree-menu-copy-name"]);
        let pg: Vec<&str> = catalog_entries(&row, Engine::Postgres, cx)
            .iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(pg, ["tree-menu-view-definition", "tree-menu-copy-name"]);
        let table: Vec<&str> = catalog_entries(
            &RowKind::Relation(RelationKind::Table),
            Engine::Dynamodb,
            cx,
        )
        .iter()
        .map(|e| e.id)
        .collect();
        assert_eq!(
            table,
            [
                "tree-menu-open",
                "tree-menu-new-query",
                "tree-menu-copy-name",
                "tree-menu-generate-select",
                "tree-menu-refresh"
            ]
        );
    });
}
