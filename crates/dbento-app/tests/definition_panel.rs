//! The definition panel on its own: states, buttons, notes and titles.
use crate::support;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use crate::support::{click_id, label_of, present};
use dbento_app::definition_panel::{
    DefinitionPanel, DefinitionPanelEvent, cap_words, definition_note, definition_title,
};
use dbento_core::{
    ConstraintInfo, ConstraintKind, DEFINITION_CAP, Definition, IndexInfo, ObjectRef, RelationRef,
    RoutineInfo, RoutineKind, TriggerInfo, TriggerState, TypeInfo, TypeKind,
};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Entity, Focusable as _, InputEvent as _, MouseMoveEvent,
    Pixels, ScrollDelta, ScrollEvent, TestAppContext, WindowHandle, point, px,
};

// fixture
fn def(text: &str, truncated: bool, composed: bool) -> Definition {
    Definition::new(text, truncated, composed)
}
fn v() -> ObjectRef {
    ObjectRef::relation(RelationRef::new("main", "v"))
}
fn hosted(cx: &mut TestAppContext) -> (WindowHandle<DefinitionPanel>, Entity<DefinitionPanel>) {
    support::init(cx);
    let win = cx.add_window(|_, cx| DefinitionPanel::new(cx));
    let panel = win.update(cx, |_, _, cx| cx.entity()).unwrap();
    (win, panel)
}
fn show(
    cx: &mut TestAppContext,
    win: WindowHandle<DefinitionPanel>,
    conn: &str,
    object: &ObjectRef,
    result: Result<Definition, String>,
) -> bool {
    let took = win
        .update(cx, |p, w, cx| p.show(conn, object, result, w, cx))
        .unwrap();
    cx.run_until_parked();
    took
}
fn events(
    cx: &mut TestAppContext,
    panel: &Entity<DefinitionPanel>,
) -> Rc<RefCell<Vec<DefinitionPanelEvent>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let out = seen.clone();
    cx.update(|cx| {
        cx.subscribe(panel, move |_, e: &DefinitionPanelEvent, _| {
            out.borrow_mut().push(e.clone())
        })
        .detach()
    });
    seen
}

#[gpui_kit::test]
fn the_panel_loads_shows_and_drops_replies_for_other_objects(cx: &mut TestAppContext) {
    let (win, panel) = hosted(cx);
    let any: AnyWindowHandle = win.into();
    panel.update(cx, |p, cx| p.loading("lite", v(), cx));
    assert!(present(cx, any, "definition-loading"));
    assert_eq!(
        label_of(cx, any, "definition-title").as_deref(),
        Some("Relation main.v")
    );
    let w = ObjectRef::relation(RelationRef::new("main", "w"));
    let other = Ok(def("CREATE VIEW w AS SELECT 2;", false, false));
    assert!(
        !show(cx, win, "lite", &w, other.clone()),
        "another object is dropped"
    );
    assert!(
        !show(cx, win, "other", &v(), other),
        "so is another connection"
    );
    assert!(present(cx, any, "definition-loading"));
    let text = "CREATE VIEW v AS SELECT 1;";
    assert!(show(cx, win, "lite", &v(), Ok(def(text, false, false))));
    assert!(!present(cx, any, "definition-loading"));
    assert!(!present(cx, any, "definition-note"));
    assert_eq!(
        panel
            .read_with(cx, |p, _| p.text().map(str::to_owned))
            .as_deref(),
        Some(text)
    );
    assert_eq!(
        label_of(cx, any, "definition-editor").as_deref(),
        Some(text)
    );
    let state = panel.read_with(cx, |p, _| p.editor().cloned()).unwrap();
    state.read_with(cx, |s, _| {
        assert!(s.presentation().is_readonly());
        assert_eq!(s.language_name().to_string(), "sql");
    });
}

#[gpui_kit::test]
fn typing_into_the_definition_changes_nothing(cx: &mut TestAppContext) {
    let (win, panel) = hosted(cx);
    let text = "CREATE VIEW v AS SELECT 1;";
    panel.update(cx, |p, cx| p.loading("lite", v(), cx));
    show(cx, win, "lite", &v(), Ok(def(text, false, false)));
    let state = panel.read_with(cx, |p, _| p.editor().cloned()).unwrap();
    // Keys go through `update_window`: inside the root's own update they would double-lease it.
    cx.update_window(win.into(), |_, w, cx| {
        state.read(cx).focus_handle(cx).focus(w, cx);
        w.input("zz", cx);
        w.press("backspace", cx);
        w.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(state.read_with(cx, |s, _| s.value().to_string()), text);
}

#[gpui_kit::test]
fn a_failure_shows_its_message_and_the_buttons_emit(cx: &mut TestAppContext) {
    let (win, panel) = hosted(cx);
    let any: AnyWindowHandle = win.into();
    let seen = events(cx, &panel);
    panel.update(cx, |p, cx| p.loading("lite", v(), cx));
    let gone = "main.v no longer exists; refresh the explorer to see the current objects";
    show(cx, win, "lite", &v(), Err(gone.into()));
    assert_eq!(label_of(cx, any, "definition-error").as_deref(), Some(gone));
    assert!(!present(cx, any, "definition-editor"));
    assert!(!present(cx, any, "definition-copy"));
    click_id(cx, any, "definition-reload");
    let text = "CREATE VIEW v AS SELECT 1;";
    show(cx, win, "lite", &v(), Ok(def(text, false, false)));
    click_id(cx, any, "definition-copy");
    let copied = cx.read_from_clipboard().and_then(|c| c.text());
    assert_eq!(copied.as_deref(), Some(text));
    click_id(cx, any, "definition-open-tab");
    click_id(cx, any, "definition-close");
    assert_eq!(
        *seen.borrow(),
        [
            DefinitionPanelEvent::Reload {
                connection: "lite".into(),
                object: v()
            },
            DefinitionPanelEvent::OpenInTab {
                connection: "lite".into(),
                sql: text.into()
            },
            DefinitionPanelEvent::Close,
        ]
    );
}

#[gpui_kit::test]
fn reload_shows_while_loading_so_a_lost_reply_can_be_retried(cx: &mut TestAppContext) {
    let (win, panel) = hosted(cx);
    let any: AnyWindowHandle = win.into();
    let seen = events(cx, &panel);
    assert!(!present(cx, any, "definition-reload"), "nothing to reload");
    panel.update(cx, |p, cx| p.loading("lite", v(), cx));
    click_id(cx, any, "definition-reload");
    assert_eq!(
        *seen.borrow(),
        [DefinitionPanelEvent::Reload {
            connection: "lite".into(),
            object: v()
        }]
    );
}

#[test]
fn notes_say_when_text_is_composed_or_cut() {
    let composed = "Reconstructed from the catalog; may not run as is";
    let cut = format!("First {} shown", cap_words(DEFINITION_CAP));
    assert_eq!(definition_note(&def("x;", false, false)), None);
    assert_eq!(
        definition_note(&def("x;", false, true)).as_deref(),
        Some(composed)
    );
    assert_eq!(definition_note(&def("x", true, false)), Some(cut.clone()));
    assert_eq!(
        definition_note(&def("x", true, true)),
        Some(format!("{composed} \u{b7} {cut}"))
    );
    assert_eq!(cap_words(1 << 20), "1 MiB");
    assert_eq!(cap_words(1 << 18), "256 KiB");
}

#[test]
fn titles_name_the_kind_and_the_object() {
    let t = RelationRef::new("public", "t");
    let one = |s: &str| vec![s.to_owned()];
    // fixture: core constructors
    let index = IndexInfo::new(t.clone(), "t_idx", one("a"), false, false, false, None);
    let pk = ConstraintInfo::new(t.clone(), None, ConstraintKind::PrimaryKey, one("id"), None);
    let trig = TriggerInfo::new(t.clone(), "t_on", TriggerState::Enabled);
    let ty = TypeInfo::new("public", "mood", TypeKind::Enum);
    let add = RoutineInfo::new(
        "public",
        "add",
        RoutineKind::Function,
        "a integer, b integer",
        Some("integer".to_owned()),
    );
    let titles = [
        definition_title(&ObjectRef::relation(t.clone())),
        definition_title(&index.object_ref()),
        definition_title(&pk.object_ref()),
        definition_title(&trig.object_ref()),
        definition_title(&ty.object_ref()),
        definition_title(&add.object_ref()),
    ];
    assert_eq!(
        titles,
        [
            "Relation public.t",
            "Index t_idx on public.t",
            "Constraint PRIMARY KEY on public.t",
            "Trigger t_on on public.t",
            "Type public.mood",
            "Routine public.add(a integer, b integer)",
        ]
    );
}

#[gpui_kit::test]
fn the_editor_is_named_by_its_first_line_cut_to_200_chars(cx: &mut TestAppContext) {
    let (win, panel) = hosted(cx);
    let first = format!("CREATE VIEW v AS SELECT '{}' AS x", "y".repeat(300));
    let text = format!("{first}\nFROM t;");
    panel.update(cx, |p, cx| p.loading("lite", v(), cx));
    show(cx, win, "lite", &v(), Ok(def(&text, false, false)));
    let label = label_of(cx, win.into(), "definition-editor").expect("a labelled editor");
    assert_eq!(label, first.chars().take(200).collect::<String>());
}

#[gpui_kit::test]
fn a_reload_reply_keeps_focus_in_the_editor(cx: &mut TestAppContext) {
    let (win, panel) = hosted(cx);
    panel.update(cx, |p, cx| p.loading("lite", v(), cx));
    show(
        cx,
        win,
        "lite",
        &v(),
        Ok(def("CREATE VIEW v AS SELECT 1;", false, false)),
    );
    let old = panel.read_with(cx, |p, _| p.editor().cloned()).unwrap();
    cx.update_window(win.into(), |_, w, cx| {
        old.read(cx).focus_handle(cx).focus(w, cx)
    })
    .unwrap();
    cx.run_until_parked();
    let held = cx
        .update_window(win.into(), |_, w, cx| {
            old.read(cx).focus_handle(cx).is_focused(w)
        })
        .unwrap();
    assert!(held, "the first editor holds focus");
    // Reload names the same object again, then its reply lands.
    panel.update(cx, |p, cx| p.loading("lite", v(), cx));
    cx.run_until_parked();
    show(
        cx,
        win,
        "lite",
        &v(),
        Ok(def("CREATE VIEW v AS SELECT 2;", false, false)),
    );
    let new = panel.read_with(cx, |p, _| p.editor().cloned()).unwrap();
    assert_ne!(new.entity_id(), old.entity_id(), "a fresh editor per reply");
    let focused = cx
        .update_window(win.into(), |_, w, cx| {
            new.read(cx).focus_handle(cx).is_focused(w)
        })
        .unwrap();
    assert!(focused, "focus follows into the new editor");
}

/// The ready editor's vertical scroll offset.
fn editor_y(cx: &mut TestAppContext, panel: &Entity<DefinitionPanel>) -> Pixels {
    panel.read_with(cx, |p, cx| p.editor().unwrap().read(cx).scroll_offset().y)
}

/// Advances the clock by `dt`, runs the frame callbacks the last draw asked for and draws.
fn frame(cx: &mut TestAppContext, win: AnyWindowHandle, dt: Duration) {
    cx.executor().advance_clock(dt);
    cx.update_window(win, |_, w, cx| {
        w.simulate_next_frame(cx);
        w.render_frame(cx);
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_wheel_notch_over_the_editor_eases_like_the_grid(cx: &mut TestAppContext) {
    let (win, panel) = hosted(cx);
    let any: AnyWindowHandle = win.into();
    let text: Vec<String> = (0..300).map(|i| format!("SELECT {i};")).collect();
    panel.update(cx, |p, cx| p.loading("lite", v(), cx));
    show(
        cx,
        win,
        "lite",
        &v(),
        Ok(def(&text.join("\n"), false, false)),
    );
    for _ in 0..2 {
        cx.update_window(any, |_, w, cx| w.render_frame(cx))
            .unwrap();
    }
    let line = panel
        .read_with(cx, |p, cx| p.editor().unwrap().read(cx).line_height())
        .expect("the editor is laid out");
    cx.update_window(any, |_, w, cx| {
        let at = w.find("definition-editor").bounds().center();
        w.dispatch_event(
            MouseMoveEvent {
                position: at,
                pressed_button: None,
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
        w.dispatch_event(
            ScrollEvent {
                position: at,
                delta: ScrollDelta::Lines(point(0., -3.)),
                ..Default::default()
            }
            .to_platform_input(),
            cx,
        );
        w.render_frame(cx);
    })
    .unwrap();
    let mut last = editor_y(cx, &panel);
    assert!(last > line * -3.0, "the notch jumped: {last:?}");
    for i in 0..13 {
        frame(cx, any, Duration::from_millis(7));
        let y = editor_y(cx, &panel);
        assert!(
            y <= last && y > line * -3.0,
            "frame {i}: {y:?} after {last:?}"
        );
        last = y;
    }
    assert!(last < px(0.), "the editor never moved");
    frame(cx, any, Duration::from_millis(20));
    frame(cx, any, Duration::from_millis(7));
    assert_eq!(editor_y(cx, &panel), line * -3.0);
}
