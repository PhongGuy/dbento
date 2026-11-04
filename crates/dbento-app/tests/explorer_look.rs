//! The explorer's look: header, filter, row pitch, indent and node glyphs.
mod support;
use dbento_app::explorer::{connection_id, folder_id, part_id};
use dbento_app::schema_tree::{ObjectGroup, RowKind};
use dbento_app::sidebar::{connection_tooltip, engine_icon, node_icon};
use dbento_app::ui::Icons;
use dbento_core::{Engine, FolderPath, ObjectRef, RelationKind, RelationRef};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, Bounds, ElementId, Pixels, TestAppContext, px};
use support::{TREE, fixture, started};

fn at(cx: &mut TestAppContext, win: AnyWindowHandle, id: impl Into<ElementId>) -> Bounds<Pixels> {
    let id = id.into();
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        w.find(id).bounds()
    })
    .unwrap()
}
fn path(p: &str) -> FolderPath {
    p.parse().unwrap()
}
fn column(primary_key: bool) -> RowKind {
    RowKind::Column {
        type_name: "int4".into(),
        primary_key,
    }
}

#[test]
fn every_node_kind_has_its_glyph() {
    assert_eq!(node_icon(&RowKind::Schema), Some(Icons::Layers));
    assert_eq!(
        node_icon(&RowKind::Kind(RelationKind::Table)),
        Some(Icons::Table2)
    );
    assert_eq!(
        node_icon(&RowKind::Relation(RelationKind::Table)),
        Some(Icons::Table2)
    );
    assert_eq!(
        node_icon(&RowKind::Kind(RelationKind::View)),
        Some(Icons::Eye)
    );
    assert_eq!(
        node_icon(&RowKind::Relation(RelationKind::MaterializedView)),
        Some(Icons::Eye)
    );
    assert_eq!(
        node_icon(&RowKind::Relation(RelationKind::ForeignTable)),
        Some(Icons::Table2)
    );
    assert_eq!(node_icon(&column(false)), Some(Icons::Columns3));
    assert_eq!(node_icon(&column(true)), Some(Icons::KeyRound));
    assert_eq!(node_icon(&RowKind::Loading), None);
    assert_eq!(node_icon(&RowKind::Empty), None);
    assert_eq!(node_icon(&RowKind::Truncated), None);
}
#[test]
fn engines_get_generic_glyphs_never_logos() {
    assert_eq!(engine_icon(Engine::Postgres), Icons::Server);
    assert_eq!(engine_icon(Engine::Sqlite), Icons::FileBox);
    assert_eq!(engine_icon(Engine::Mysql), Icons::Database);
    assert_eq!(engine_icon(Engine::Duckdb), Icons::FileBox);
}
#[gpui_kit::test]
fn the_header_is_36_px_with_24_px_buttons(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    let (tree, filter, add) = (
        at(cx, s.win, "schema-tree"),
        at(cx, s.win, "schema-filter"),
        at(cx, s.win, "connection-new"),
    );
    assert_eq!(filter.top() - tree.top(), px(36.));
    assert_eq!((add.size.width, add.size.height), (px(24.), px(24.)));
}
#[gpui_kit::test]
fn rows_are_26_px_and_indent_14_px_per_level(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    let (team, sub) = (
        at(cx, s.win, folder_id(&path("team"))),
        at(cx, s.win, folder_id(&path("team/sub"))),
    );
    assert_eq!(team.size.height, px(26.));
    assert_eq!(sub.top() - team.top(), px(26.));
    assert_eq!(at(cx, s.win, connection_id("lite")).size.height, px(26.));
    // `lite` sits two folders deep, `top` at the root.
    let deep = at(cx, s.win, part_id("connection-toggle", "lite"));
    let root = at(cx, s.win, part_id("connection-toggle", "top"));
    assert_eq!(deep.left() - root.left(), px(28.));
}
#[test]
fn a_connection_tooltip_names_the_engine_and_target() {
    let fx = fixture("", "");
    let lite = &fx.ws.connections()[0];
    assert_eq!(
        connection_tooltip(lite),
        format!(
            "sqlite \u{b7} {} \u{b7} Double-click to open a query tab",
            lite.display_target()
        )
    );
}

#[test]
fn object_groups_and_their_objects_share_a_glyph() {
    let cases = [
        (ObjectGroup::Keys, Icons::Key),
        (ObjectGroup::Indexes, Icons::Hash),
        (ObjectGroup::Checks, Icons::ShieldCheck),
        (ObjectGroup::Triggers, Icons::Zap),
        (ObjectGroup::Types, Icons::Shapes),
        (ObjectGroup::Routines, Icons::SquareFunction),
    ];
    for (group, icon) in cases {
        assert_eq!(node_icon(&RowKind::Group(group)), Some(icon), "{group:?}");
        let object = RowKind::Object {
            group,
            object: ObjectRef::relation(RelationRef::new("main", "t")),
            detail: String::new(),
        };
        assert_eq!(node_icon(&object), Some(icon), "{group:?}");
    }
}
