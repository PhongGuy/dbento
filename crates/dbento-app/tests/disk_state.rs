//! The tab table as a pure judge, and the tab's banner state: what shows, what Keep does,
//! and how a moved file is rebased.
use crate::support::{Opened, label_of, open_saved, opened, present};
use dbento_app::query_tab::QueryTab;
use dbento_app::workspace_reload::{DiskBanner, DiskRead, TabDisk, TabVerdict, judge_tab};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, Entity, Role, TestAppContext};
use std::path::{Path, PathBuf};

fn at(file: &str, disk: Option<&str>) -> TabDisk {
    TabDisk {
        file: PathBuf::from(file),
        disk: disk.map(str::to_owned),
    }
}
fn tab(cx: &mut TestAppContext, o: &Opened) -> Entity<QueryTab> {
    o.shell.read_with(cx, |s, _| s.tab().unwrap().clone())
}
fn show(cx: &mut TestAppContext, o: &Opened, b: Option<DiskBanner>) {
    let t = tab(cx, o);
    t.update(cx, |t, cx| t.set_disk_banner(b, cx));
    cx.run_until_parked();
}

#[test]
fn judge_tab_follows_the_table() {
    let t = |s: &str| DiskRead::Text(s.to_owned());
    let a = at("queries/lite/a.sql", Some("a"));
    assert_eq!(
        judge_tab(&at("queries/lite/a.sql", Some("x")), &a, false, &t("b")),
        TabVerdict::Stale,
        "saved meanwhile"
    );
    assert_eq!(
        judge_tab(
            &at("queries/old/a.sql", Some("a")),
            &a,
            false,
            &DiskRead::Missing
        ),
        TabVerdict::Stale,
        "moved meanwhile"
    );
    assert_eq!(judge_tab(&a, &a, true, &t("a")), TabVerdict::Same);
    assert_eq!(
        judge_tab(&a, &a, false, &t("b")),
        TabVerdict::Take("b".into())
    );
    assert_eq!(
        judge_tab(&a, &a, true, &t("b")),
        TabVerdict::Show(DiskBanner::Changed { disk: "b".into() })
    );
    assert_eq!(
        judge_tab(&a, &a, false, &DiskRead::Missing),
        TabVerdict::Show(DiskBanner::Deleted)
    );
    let kept = at("queries/lite/a.sql", None);
    assert_eq!(
        judge_tab(&kept, &kept, true, &DiskRead::Missing),
        TabVerdict::Same,
        "kept after a delete"
    );
    let bad = DiskRead::Unreadable("denied".into());
    assert_eq!(
        judge_tab(&a, &a, false, &bad),
        TabVerdict::Show(DiskBanner::Unreadable {
            reason: "denied".into()
        })
    );
}

#[gpui_kit::test]
fn the_changed_banner_shows_its_choices_with_roles(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    open_saved(cx, &o, "lite", "SELECT 1");
    show(
        cx,
        &o,
        Some(DiskBanner::Changed {
            disk: "SELECT 2".into(),
        }),
    );
    assert_eq!(
        label_of(cx, o.win, "disk-banner-text").as_deref(),
        Some("Changed on disk")
    );
    for id in ["disk-reload", "disk-keep", "disk-compare"] {
        assert!(present(cx, o.win, id), "{id}");
    }
    assert!(!present(cx, o.win, "disk-close"), "no Close on a change");
    let roles = cx
        .update_window(o.win, |_, w, cx| {
            w.render_frame(cx);
            ["disk-banner", "disk-banner-text"].map(|id| w.try_find(id).and_then(|e| e.role()))
        })
        .unwrap();
    assert_eq!(roles, [Some(Role::Status), Some(Role::Label)]);
}

#[gpui_kit::test]
fn keep_mine_takes_the_disk_text_as_known(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    open_saved(cx, &o, "lite", "SELECT 1");
    show(
        cx,
        &o,
        Some(DiskBanner::Changed {
            disk: "SELECT 2".into(),
        }),
    );
    let t = tab(cx, &o);
    t.update(cx, |t, cx| t.keep_mine(cx));
    assert_eq!(
        t.read_with(cx, |t, _| t.disk().map(str::to_owned))
            .as_deref(),
        Some("SELECT 2")
    );
    assert!(!present(cx, o.win, "disk-banner"), "hidden");
}

#[gpui_kit::test]
fn keep_on_a_deleted_file_forgets_the_disk_text(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    open_saved(cx, &o, "lite", "SELECT 1");
    show(cx, &o, Some(DiskBanner::Deleted));
    assert_eq!(
        label_of(cx, o.win, "disk-banner-text").as_deref(),
        Some("Deleted on disk")
    );
    assert!(
        present(cx, o.win, "disk-close") && !present(cx, o.win, "disk-reload"),
        "Keep and Close only"
    );
    let t = tab(cx, &o);
    t.update(cx, |t, cx| t.keep_mine(cx));
    assert_eq!(t.read_with(cx, |t, _| t.disk().map(str::to_owned)), None);
}

#[gpui_kit::test]
fn a_kept_read_failure_stays_hidden_until_something_else_is_read(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    open_saved(cx, &o, "lite", "SELECT 1");
    let failed = DiskBanner::Unreadable {
        reason: "denied".into(),
    };
    show(cx, &o, Some(failed.clone()));
    assert_eq!(
        label_of(cx, o.win, "disk-banner-text").as_deref(),
        Some("Cannot read the file on disk: denied")
    );
    let t = tab(cx, &o);
    t.update(cx, |t, cx| t.keep_mine(cx));
    show(cx, &o, Some(failed.clone()));
    assert!(
        !present(cx, o.win, "disk-banner"),
        "the same failure stays hidden"
    );
    show(cx, &o, None);
    show(cx, &o, Some(failed));
    assert!(
        present(cx, o.win, "disk-banner"),
        "shows again after a good read"
    );
}

#[gpui_kit::test]
fn rebase_moves_only_a_file_under_the_old_folder(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    open_saved(cx, &o, "lite", "SELECT 1");
    let t = tab(cx, &o);
    let moved = t.update(cx, |t, cx| {
        t.rebase_file(Path::new("queries/other"), Path::new("queries/x"), cx)
    });
    assert!(!moved, "another folder");
    let moved = t.update(cx, |t, cx| {
        t.rebase_file(Path::new("queries/lite"), Path::new("queries/lite2"), cx)
    });
    assert!(moved, "its folder");
    assert_eq!(
        t.read_with(cx, |t, _| t.file().map(Path::to_path_buf)),
        Some(PathBuf::from("queries/lite2/a.sql"))
    );
}
