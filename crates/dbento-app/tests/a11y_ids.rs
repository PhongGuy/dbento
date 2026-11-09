//! The elements gpui-pilot's session backend finds by AT-SPI id. An element enters the
//! accessibility tree only with both an element id and a role, so each must carry a role, and
//! those the pilot reads by name must carry it. Test windows never activate
//! accessibility, so the ids themselves are proven by the pilot's session test.
use crate::support;
use crate::support::opened;
use dbento_app::definition_panel::DefinitionPanel;
use dbento_app::explorer::{connection_a11y_id, connection_id};
use dbento_app::query_tab::row_limit_label;
use dbento_core::{Definition, ObjectRef, RelationRef};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, ElementId, Role};

#[gpui_kit::test]
fn the_pilot_ids_are_in_the_accessibility_tree(cx: &mut gpui_kit::TestAppContext) {
    let o = opened(cx, "", "", 0);
    o.run(cx, "SELECT 1 AS one UNION ALL SELECT 2");
    o.idle(cx);
    cx.run_until_parked();
    let cap = o.model(cx).read_with(cx, |m, _| m.max_rows());
    let ids: [(ElementId, Option<String>); 6] = [
        (connection_id("lite"), Some("lite".into())),
        ("editor".into(), None),
        ("run-all".into(), Some("Run all".into())),
        ("row-limit".into(), Some(row_limit_label(cap))),
        ("result-grid".into(), None),
        ("footer-rows".into(), Some("2 rows".into())),
    ];
    for (id, label) in ids {
        let (role, found) = cx
            .update_window(o.win, |_, w, cx| {
                w.render_frame(cx);
                let e = w
                    .try_find(id.clone())
                    .unwrap_or_else(|| panic!("no {id:?}"));
                (e.role(), e.label().map(str::to_owned))
            })
            .unwrap();
        assert!(
            role.is_some_and(|r| r != Role::GenericContainer),
            "{id:?} has no accessibility role: {role:?}"
        );
        if label.is_some() {
            assert_eq!(found, label, "{id:?}");
        }
    }
    // AT-SPI names a Label node by its value, which is what the pilot reads the count from.
    let value = cx
        .update_window(o.win, |_, w, _| {
            w.try_find("footer-rows")
                .and_then(|e| e.value().map(str::to_owned))
        })
        .unwrap();
    assert_eq!(value.as_deref(), Some("2 rows"));
}

/// The session backend finds a connection row by the same spelling the headless path sees for
/// its element id: `kind-name`, or `kind[name]` when the name holds a hyphen.
#[test]
fn a_connection_row_is_found_by_its_kind_and_name() {
    assert_eq!(connection_a11y_id("lite"), "connection-lite");
    assert_eq!(connection_a11y_id("toggle-lite"), "connection[toggle-lite]");
}

fn assert_roles(cx: &mut gpui_kit::TestAppContext, win: AnyWindowHandle, ids: &[&'static str]) {
    for id in ids {
        let role = cx
            .update_window(win, |_, w, cx| {
                w.render_frame(cx);
                w.try_find(*id).unwrap_or_else(|| panic!("no {id}")).role()
            })
            .unwrap();
        assert!(
            role.is_some_and(|r| r != Role::GenericContainer),
            "{id} has no accessibility role: {role:?}"
        );
    }
}

#[gpui_kit::test]
fn the_definition_panel_ids_carry_roles(cx: &mut gpui_kit::TestAppContext) {
    support::init(cx);
    let win = cx.add_window(|_, cx| DefinitionPanel::new(cx));
    let any: AnyWindowHandle = win.into();
    let v = ObjectRef::relation(RelationRef::new("main", "v"));
    win.update(cx, |p, _, cx| p.loading("lite", v.clone(), cx))
        .unwrap();
    assert_roles(
        cx,
        any,
        &[
            "definition-panel",
            "definition-title",
            "definition-loading",
            "definition-reload",
            "definition-close",
        ],
    );
    win.update(cx, |p, w, cx| p.show("lite", &v, Err("boom".into()), w, cx))
        .unwrap();
    assert_roles(cx, any, &["definition-error", "definition-reload"]);
    // fixture: core constructor
    let composed = Definition::new("CREATE TYPE main.mood AS ENUM ('a');", false, true);
    win.update(cx, |p, w, cx| p.show("lite", &v, Ok(composed), w, cx))
        .unwrap();
    assert_roles(
        cx,
        any,
        &[
            "definition-note",
            "definition-editor",
            "definition-copy",
            "definition-open-tab",
            "definition-reload",
        ],
    );
}
