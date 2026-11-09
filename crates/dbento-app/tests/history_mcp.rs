use crate::support;
use std::time::{Duration, SystemTime};

use crate::support::{label_of, present, wait_real};
use dbento_app::app::{AppPaths, start};
use dbento_core::{History, HistoryEntry, HistoryOutcome, HistorySource};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

#[gpui_kit::test]
fn mcp_rows_carry_an_mcp_tag_and_app_rows_do_not(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let (fx, files) = (support::fixture("", ""), tempfile::tempdir().unwrap());
    let mut h = History::open(&AppPaths::in_dir(files.path()).history).unwrap();
    let ok = HistoryOutcome::Ok {
        rows: 0,
        truncated: false,
        rows_affected: Some(1),
    };
    let at = SystemTime::now();
    let entry = |sql: &str| {
        HistoryEntry::new(
            at,
            fx.ws.dir(),
            "lite",
            sql,
            Duration::from_micros(4_321),
            ok.clone(),
        )
    };
    h.append(&entry("SELECT 1")).unwrap();
    h.append(&entry("DELETE FROM t WHERE a = 1").with_source(HistorySource::Mcp))
        .unwrap();
    let (win, _shell) = cx
        .update(|cx| {
            start(
                support::services(files.path()),
                Some(fx.dir.path().into()),
                cx,
            )
        })
        .unwrap();
    assert!(present(cx, win, "connection-list"));
    cx.update_window(win, |_, w, cx| w.press("ctrl-shift-h", cx))
        .unwrap();
    wait_real(cx, "the rows", |cx| {
        label_of(cx, win, ("history-row", 1usize)).is_some()
    });
    // Newest first, so the MCP run is row 0.
    assert_eq!(
        label_of(cx, win, ("history-row", 0usize)).as_deref(),
        Some("DELETE FROM t WHERE a = 1")
    );
    assert_eq!(
        label_of(cx, win, ("history-mcp", 0usize)).as_deref(),
        Some("MCP")
    );
    assert!(!present(cx, win, ("history-mcp", 1usize)));
    assert_eq!(
        label_of(cx, win, ("history-detail", 0usize)).as_deref(),
        Some("lite · 1 row affected · 4 ms")
    );
}
