//! Export writes the loaded rows to a file; it never re-runs.
// Test helpers may panic on a broken fixture too; clippy's in-tests exemption covers only
// #[test] functions, not the helpers they call.
#![allow(clippy::unwrap_used, clippy::expect_used)]
mod support;
use dbento_app::export::{ExportFormat, chosen_path, export_format, export_text, write_export};
use dbento_app::results::ResultStore;
use gpui_kit::AppContext as _;
use gpui_kit::TestAppContext;
use gpui_kit::test::TestWindowExt as _;
use std::path::{Path, PathBuf};
use support::{label_of, opened, wait_real};

fn store(sql: &str) -> ResultStore {
    let (columns, batches) = support::batches(sql);
    let mut s = ResultStore::new(columns);
    for b in batches {
        s.push(b);
    }
    s
}

#[test]
fn a_failed_save_dialog_says_so_and_a_cancelled_one_says_nothing() {
    // The test platform's dialog always answers Ok, so its failure is driven here.
    let picked = PathBuf::from("/w/t.csv");
    assert_eq!(
        chosen_path::<&str, ()>(Ok(Ok(Some(picked.clone())))),
        Ok(Some(picked))
    );
    assert_eq!(chosen_path::<&str, ()>(Ok(Ok(None))), Ok(None));
    assert_eq!(
        chosen_path::<&str, ()>(Err(())),
        Ok(None),
        "a dropped dialog"
    );
    assert_eq!(
        chosen_path::<_, ()>(Ok(Err("portal unavailable"))),
        Err("Export failed: portal unavailable".to_owned())
    );
}

#[test]
fn export_text_writes_every_loaded_row() {
    let s = store("SELECT 1 AS a, 'x,y' AS b UNION ALL SELECT 2, NULL");
    assert_eq!(export_text(&s, ExportFormat::Csv), "a,b\n1,\"x,y\"\n2,");
    assert_eq!(
        export_text(&s, ExportFormat::Json),
        "[\n  {\"a\": 1, \"b\": \"x,y\"},\n  {\"a\": 2, \"b\": null}\n]"
    );
    assert_eq!(export_format(Path::new("/tmp/r.json")), ExportFormat::Json);
    assert_eq!(export_format(Path::new("/tmp/r.JSON")), ExportFormat::Json);
    assert_eq!(export_format(Path::new("/tmp/r.txt")), ExportFormat::Csv);
}

// The pane renders an export on the background executor from a snapshot, so the snapshot must
// travel to another thread, keep the local sort, and ignore rows that arrive after it.
#[test]
fn a_snapshot_renders_the_same_export_on_another_thread() {
    let mut s = store("SELECT 1 AS a, 'x' AS b UNION ALL SELECT 3, 'z' UNION ALL SELECT 2, NULL");
    s.sort_local(0, true);
    let want = (
        export_text(&s, ExportFormat::Csv),
        export_text(&s, ExportFormat::Json),
    );
    let snap = s.snapshot();
    for b in support::batches("SELECT 9 AS a, 'late' AS b").1 {
        s.push(b);
    }
    assert_eq!(snap.rows(), 3);
    let got = std::thread::spawn(move || {
        (
            export_text(&snap, ExportFormat::Csv),
            export_text(&snap, ExportFormat::Json),
        )
    })
    .join()
    .unwrap();
    assert_eq!(got, want);
    assert_eq!(got.0, "a,b\n3,z\n2,\n1,x");
}

#[test]
fn write_export_replaces_the_file_whole_and_reports_a_bad_path() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("out.csv");
    std::fs::write(&p, "old").unwrap();
    write_export(&p, "a\n1").unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "a\n1");
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        1,
        "no temp file left"
    );
    assert!(write_export(&dir.path().join("missing/out.csv"), "x").is_err());
}

// A non-default umask proves the new file goes through it rather than a fixed mode.
#[cfg(unix)]
#[test]
fn write_export_creates_a_new_file_through_the_umask() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("new.csv");
    // SAFETY: umask has no memory-safety preconditions.
    let old = unsafe { libc::umask(0o027) };
    let written = write_export(&p, "a\n1");
    unsafe { libc::umask(old) };
    written.unwrap();
    let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o666 & !0o027, "got {mode:o}");
}

#[cfg(unix)]
#[test]
fn write_export_keeps_an_overwritten_files_mode() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("out.csv");
    std::fs::write(&p, "old").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o640)).unwrap();
    write_export(&p, "a\n1").unwrap();
    let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o640, "got {mode:o}");
}

#[gpui_kit::test]
fn the_strip_export_writes_the_loaded_rows_without_a_rerun(cx: &mut TestAppContext) {
    let o = opened(
        cx,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); INSERT INTO t VALUES (1, 'b'), (2, 'a');",
        "",
        0,
    );
    o.run(cx, "SELECT id, name FROM t ORDER BY id");
    o.idle(cx);
    let run_id = o.model(cx).read_with(cx, |m, _| m.run_id());
    let out = o.files.path().join("rows.json");
    let target = out.clone();
    let ws = o.fx.dir.path().to_path_buf();
    cx.update_window(o.win, |_, w, cx| w.click("result-export", cx))
        .unwrap();
    cx.run_until_parked();
    cx.simulate_new_path_selection(move |dir| {
        assert_eq!(dir, ws.as_path(), "the dialog opens in the workspace");
        Some(target)
    });
    wait_real(cx, "the file", |_| out.exists());
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        "[\n  {\"id\": 1, \"name\": \"b\"},\n  {\"id\": 2, \"name\": \"a\"}\n]"
    );
    wait_real(cx, "the notice", |cx| {
        label_of(cx, o.win, "notice").as_deref() == Some("Exported 2 rows to rows.json")
    });
    assert_eq!(
        o.model(cx).read_with(cx, |m, _| m.run_id()),
        run_id,
        "no re-run"
    );
}

#[gpui_kit::test]
fn the_strip_export_needs_rows_like_the_menu(cx: &mut TestAppContext) {
    let o = opened(cx, "CREATE TABLE t(id INTEGER);", "", 0);
    o.run(cx, "SELECT id FROM t");
    o.idle(cx);
    cx.update_window(o.win, |_, w, cx| w.click("result-export", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(!cx.did_prompt_for_new_path(), "no rows, no export dialog");
}
