//! The conflict banner: the server's side is on disk, mine is saved; Reload, Keep mine and
//! Compare each settle the file.
use dbento_app::explorer::part_id;
use dbento_app::team_view::conflict_text;
use dbento_app::workspace_reload::{disk_diff, labelled_diff};
use dbento_core::{ConflictKind, SyncState};
use gpui_kit::{AppContext as _, TestAppContext};

use crate::support::team::{TeamFx, TeamStarted, team_fx, team_started, wait_view};
use crate::support::{click_id, label_of, present, wait_real};

const A: &str = "queries/a.sql";

fn conflicted(cx: &mut TestAppContext) -> (TeamFx, TeamStarted) {
    let fx = team_fx("write");
    fx.local_commit(A, "select 2;\n");
    fx.teammate(A, "select 3;\n");
    let s = team_started(cx, &fx);
    wait_view(cx, &s, "conflicted", |v| {
        matches!(v.state, SyncState::Conflicted(_))
    });
    (fx, s)
}

#[test]
fn each_kind_says_where_each_side_went() {
    assert_eq!(
        conflict_text(A, ConflictKind::BothChanged),
        "queries/a.sql changed here and on the server. The server's version is open; yours is saved."
    );
    assert_eq!(
        conflict_text(A, ConflictKind::DeletedLocally),
        "queries/a.sql was deleted here and changed on the server. The server's version is back."
    );
    assert_eq!(
        conflict_text(A, ConflictKind::DeletedRemotely),
        "queries/a.sql was deleted on the server and changed here. Yours is saved."
    );
}

#[test]
fn the_labelled_diff_names_both_sides_and_disk_diff_keeps_its_headers() {
    let d = labelled_diff("server", "select 3;\n", "mine", "select 2;\n");
    assert!(d.contains("--- server") && d.contains("+++ mine"), "{d}");
    assert!(d.contains("-select 3;") && d.contains("+select 2;"), "{d}");
    let old = disk_diff("a\n", "b\n");
    assert!(
        old.contains("--- disk") && old.contains("+++ this tab"),
        "{old}"
    );
}

#[gpui_kit::test]
fn a_conflict_shows_its_banner_with_the_server_side_on_disk(cx: &mut TestAppContext) {
    let (fx, s) = conflicted(cx);
    assert_eq!(
        std::fs::read_to_string(fx.ws.join(A)).unwrap(),
        "select 3;\n"
    );
    assert_eq!(
        label_of(cx, s.win, part_id("team-conflict-text", A)).as_deref(),
        Some(conflict_text(A, ConflictKind::BothChanged).as_str())
    );
    for id in [
        "team-conflict-reload",
        "team-conflict-keep",
        "team-conflict-compare",
    ] {
        assert!(present(cx, s.win, part_id(id, A)), "{id}");
    }
    assert_eq!(
        label_of(cx, s.win, "team-status").as_deref(),
        Some("Conflict in 1 file")
    );
    let copy = s
        .shell
        .read_with(cx, |sh, cx| sh.team().unwrap().read(cx).saved_copy(A));
    assert_eq!(copy.as_deref(), Some("select 2;\n"));
}

#[gpui_kit::test]
fn keep_mine_pushes_my_side_and_clears_the_banner(cx: &mut TestAppContext) {
    let (fx, s) = conflicted(cx);
    click_id(cx, s.win, part_id("team-conflict-keep", A));
    wait_view(cx, &s, "synced", |v| v.state == SyncState::Synced);
    // Settling reports Synced at once; the push of my side follows in the next cycle.
    wait_real(cx, "my side on the origin", |_| {
        fx.origin_show(A).as_deref() == Some("select 2;\n")
    });
    assert_eq!(fx.origin_show(A).as_deref(), Some("select 2;\n"));
    assert!(!present(cx, s.win, part_id("team-conflict", A)));
}

#[gpui_kit::test]
fn reload_keeps_the_server_side_and_drops_my_copy(cx: &mut TestAppContext) {
    let (fx, s) = conflicted(cx);
    click_id(cx, s.win, part_id("team-conflict-reload", A));
    wait_view(cx, &s, "synced", |v| v.state == SyncState::Synced);
    assert_eq!(
        std::fs::read_to_string(fx.ws.join(A)).unwrap(),
        "select 3;\n"
    );
    assert_eq!(fx.origin_show(A).as_deref(), Some("select 3;\n"));
    let copy = s
        .shell
        .read_with(cx, |sh, cx| sh.team().unwrap().read(cx).saved_copy(A));
    assert_eq!(copy, None);
    assert!(!present(cx, s.win, part_id("team-conflict", A)));
}

#[gpui_kit::test]
fn compare_shows_both_sides_and_its_keep_mine_settles(cx: &mut TestAppContext) {
    let (fx, s) = conflicted(cx);
    click_id(cx, s.win, part_id("team-conflict-compare", A));
    wait_real(cx, "the compare dialog", |cx| {
        present(cx, s.win, "confirm-dialog")
    });
    for id in [
        "team-compare-cancel",
        "team-compare-theirs",
        "team-compare-keep",
    ] {
        assert!(present(cx, s.win, id), "{id}");
    }
    click_id(cx, s.win, "team-compare-keep");
    wait_view(cx, &s, "synced", |v| v.state == SyncState::Synced);
    // Settling reports Synced at once; the push of my side follows in the next cycle.
    wait_real(cx, "my side on the origin", |_| {
        fx.origin_show(A).as_deref() == Some("select 2;\n")
    });
    assert_eq!(fx.origin_show(A).as_deref(), Some("select 2;\n"));
}

// Opens `A` as the palette opens a saved query and returns the tab's text.
fn open_a(cx: &mut TestAppContext, s: &TeamStarted) -> String {
    let ws = s
        .shell
        .read_with(cx, |sh, _| sh.workspace().cloned().unwrap());
    let q = ws
        .saved_queries()
        .unwrap()
        .into_iter()
        .find(|q| q.path() == std::path::Path::new(A))
        .unwrap();
    let shell = s.shell.clone();
    cx.update_window(s.win, |_, w, cx| {
        shell.update(cx, |sh, cx| sh.open_saved(&q, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    s.shell
        .read_with(cx, |sh, cx| sh.tab().unwrap().read(cx).sql(cx))
}

#[gpui_kit::test]
fn keep_mine_rereads_an_open_tab_of_the_file(cx: &mut TestAppContext) {
    // Tests never start the watcher, so only the app's own reread can update the tab.
    let (_fx, s) = conflicted(cx);
    assert_eq!(open_a(cx, &s), "select 3;\n");
    click_id(cx, s.win, part_id("team-conflict-keep", A));
    wait_real(cx, "the tab to show my side", |cx| {
        s.shell
            .read_with(cx, |sh, cx| sh.tab().unwrap().read(cx).sql(cx))
            == "select 2;\n"
    });
}
