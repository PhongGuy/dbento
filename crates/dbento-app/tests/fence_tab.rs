//! A reload that changes or removes a tab's connection fences the tab: the chip says why,
//! Run is refused and the editor text stays.
use crate::support::{Opened, files_changed, label_of, opened, rewrite};
use dbento_app::query_tab::fence_text;
use dbento_app::tab_model::Fence;
use gpui_kit::TestAppContext;

fn chip(cx: &mut TestAppContext, o: &Opened) -> Option<String> {
    label_of(cx, o.win, "conn-chip")
}
fn fence(cx: &mut TestAppContext, o: &Opened) -> Option<Fence> {
    o.model(cx).read_with(cx, |m, _| m.fence())
}

#[test]
fn the_chip_texts() {
    assert_eq!(
        fence_text(Fence::Changed {
            now_read_only: true
        }),
        "Now read-only: Reconnect to run (rolls back any open transaction)"
    );
    assert_eq!(
        fence_text(Fence::Changed {
            now_read_only: false
        }),
        "Settings changed: Reconnect to run"
    );
    assert_eq!(
        fence_text(Fence::Gone),
        "Not in dbento.toml: pick a connection to run"
    );
}

#[gpui_kit::test]
fn a_gone_connection_fences_the_tab_and_keeps_its_text(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    std::fs::write(
        o.fx.dir.path().join("dbento.toml"),
        "dbento = 1\n[connections.other]\nengine = \"sqlite\"\npath = \"data/app.db\"\n",
    )
    .unwrap();
    files_changed(cx, &o);
    assert_eq!(chip(cx, &o).as_deref(), Some(fence_text(Fence::Gone)));
    assert_eq!(fence(cx, &o), Some(Fence::Gone));
    o.run(cx, "SELECT 2");
    let m = o.model(cx);
    assert!(
        m.read_with(cx, |m, _| m.results().is_empty() && !m.running()),
        "run refused"
    );
    let sql = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).sql(cx));
    assert_eq!(sql, "SELECT 2", "the editor text stays");
}

#[gpui_kit::test]
fn turning_read_only_on_fences_until_reconnect(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    rewrite(&o.fx, "read_only = true", "");
    files_changed(cx, &o);
    let f = Fence::Changed {
        now_read_only: true,
    };
    assert_eq!(
        (fence(cx, &o), chip(cx, &o).as_deref()),
        (Some(f), Some(fence_text(f)))
    );
    rewrite(&o.fx, "", "");
    files_changed(cx, &o);
    assert_eq!(
        fence(cx, &o),
        None,
        "a file back to the session's settings lifts the fence"
    );
}
