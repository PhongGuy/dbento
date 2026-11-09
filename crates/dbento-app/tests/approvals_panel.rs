//! The approvals panel: pending requests, this session's decisions and their outcome, the MCP command.
use crate::support;
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use crate::support::{Started, click_id, label_of, present, started};
use dbento_app::app::AppPaths;
use dbento_app::approvals::{
    APPIMAGE_HINT, APPROVAL_POLL, CliPath, Quoting, cli_hint, connect_command, connect_command_with,
};
use dbento_app::approvals_panel::{ApprovalsPanel, Decided, Outcome};
use dbento_core::{ApprovalRequest, ApprovalStore, Claim, Decision, GrantState, Writes};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

const TOML: &str = "dbento = 1\n[connections.calm]\nengine = \"sqlite\"\npath = \"c.db\"\n";
const SQL: &str = "INSERT INTO t VALUES (1)";

fn store(s: &Started) -> ApprovalStore {
    ApprovalStore::open(&AppPaths::in_dir(s.files.path()).approvals).unwrap()
}
fn file(s: &Started, age_s: u64) -> ApprovalRequest {
    let r = ApprovalRequest::new(s.dir.path(), "calm", SQL, Writes::Yes, vec![])
        .with_time(SystemTime::now() - Duration::from_secs(age_s));
    store(s).request(&r).unwrap();
    r
}
fn tick(cx: &mut TestAppContext) {
    cx.executor().advance_clock(APPROVAL_POLL);
    cx.run_until_parked();
}
fn decided(cx: &mut TestAppContext, s: &Started) -> Option<String> {
    label_of(cx, s.win, ("approvals-decided", 0usize))
}

#[test]
fn the_command_quotes_only_a_path_that_needs_it() {
    let bare = CliPath::Bare;
    assert_eq!(
        connect_command_with(&bare, Path::new("/home/a/shop"), Quoting::Posix),
        "dbento --workspace /home/a/shop mcp"
    );
    assert_eq!(
        connect_command_with(&bare, Path::new("/home/a/my shop"), Quoting::Posix),
        "dbento --workspace '/home/a/my shop' mcp"
    );
    assert_eq!(
        connect_command_with(&bare, Path::new("/tmp/it's"), Quoting::Posix),
        "dbento --workspace '/tmp/it'\\''s' mcp"
    );
}

#[cfg(unix)]
#[test]
fn the_native_command_is_posix_on_unix() {
    assert_eq!(
        connect_command(&CliPath::Bare, Path::new("/home/a/my shop")),
        "dbento --workspace '/home/a/my shop' mcp"
    );
}

#[test]
fn a_bundled_cli_is_named_by_its_absolute_path() {
    let cli = CliPath::Absolute("/opt/dbento/bin/dbento".into());
    assert_eq!(
        connect_command_with(&cli, Path::new("/home/a/shop"), Quoting::Posix),
        "/opt/dbento/bin/dbento --workspace /home/a/shop mcp"
    );
    let spaced = CliPath::Absolute("/opt/my apps/dbento".into());
    assert_eq!(
        connect_command_with(&spaced, Path::new("/home/a/shop"), Quoting::Posix),
        "'/opt/my apps/dbento' --workspace /home/a/shop mcp"
    );
}

#[test]
fn inside_an_appimage_the_command_stays_bare_with_a_hint() {
    assert_eq!(
        connect_command_with(
            &CliPath::AppImage,
            Path::new("/home/a/shop"),
            Quoting::Posix
        ),
        "dbento --workspace /home/a/shop mcp"
    );
    assert_eq!(cli_hint(&CliPath::AppImage), Some(APPIMAGE_HINT));
    assert_eq!(cli_hint(&CliPath::Bare), None);
    assert_eq!(cli_hint(&CliPath::Absolute("/x/dbento".into())), None);
}

#[test]
fn windows_double_quotes_a_path_with_a_space() {
    let cli = CliPath::Absolute(r"C:\Program Files\Dbento\dbento.exe".into());
    assert_eq!(
        connect_command_with(&cli, Path::new(r"C:\Users\a\my shop"), Quoting::Windows),
        r#""C:\Program Files\Dbento\dbento.exe" --workspace "C:\Users\a\my shop" mcp"#
    );
    assert_eq!(
        connect_command_with(
            &CliPath::Bare,
            Path::new(r"C:\Users\a\shop"),
            Quoting::Windows
        ),
        r"dbento --workspace C:\Users\a\shop mcp"
    );
}

fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
    move |k| {
        pairs
            .iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| (*v).to_owned())
    }
}

/// A temp dir holding `dbento-app` and, when `cli`, a sibling `dbento`; returns it and the app's path.
fn install(cli: bool) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let suffix = std::env::consts::EXE_SUFFIX;
    let exe = dir.path().join(format!("dbento-app{suffix}"));
    std::fs::write(&exe, b"app").unwrap();
    if cli {
        std::fs::write(dir.path().join(format!("dbento{suffix}")), b"cli").unwrap();
    }
    (dir, exe)
}

#[test]
fn a_sibling_cli_resolves_to_its_absolute_path() {
    let (dir, exe) = install(true);
    let want = dir
        .path()
        .join(format!("dbento{}", std::env::consts::EXE_SUFFIX));
    assert_eq!(
        CliPath::resolve(Some(exe.as_path()), &env_of(&[])),
        CliPath::Absolute(want)
    );
}

#[test]
fn an_appimage_wins_over_a_sibling_cli() {
    let (_dir, exe) = install(true);
    let env = env_of(&[("APPIMAGE", "/home/a/Dbento.AppImage")]);
    assert_eq!(
        CliPath::resolve(Some(exe.as_path()), &env),
        CliPath::AppImage
    );
    let empty = env_of(&[("APPIMAGE", "")]);
    assert!(matches!(
        CliPath::resolve(Some(exe.as_path()), &empty),
        CliPath::Absolute(_)
    ));
}

#[test]
fn no_sibling_no_exe_or_a_relative_exe_is_bare() {
    let (_dir, exe) = install(false);
    assert_eq!(
        CliPath::resolve(Some(exe.as_path()), &env_of(&[])),
        CliPath::Bare
    );
    assert_eq!(CliPath::resolve(None, &env_of(&[])), CliPath::Bare);
    assert_eq!(
        CliPath::resolve(Some(Path::new("dbento-app")), &env_of(&[])),
        CliPath::Bare
    );
}

#[test]
fn a_directory_named_dbento_is_not_the_cli() {
    let (dir, exe) = install(false);
    std::fs::create_dir(
        dir.path()
            .join(format!("dbento{}", std::env::consts::EXE_SUFFIX)),
    )
    .unwrap();
    assert_eq!(
        CliPath::resolve(Some(exe.as_path()), &env_of(&[])),
        CliPath::Bare
    );
}

/// A bare panel window naming `cli`, showing the command for `/home/a/shop`.
fn panel(cx: &mut TestAppContext, cli: CliPath) -> gpui_kit::AnyWindowHandle {
    support::init(cx);
    let win = cx.add_window(|_, cx| ApprovalsPanel::with_cli(cli, cx));
    win.update(cx, |p, _, cx| {
        p.set(Vec::new(), Vec::new(), Some("/home/a/shop".into()), cx)
    })
    .unwrap();
    win.into()
}

#[test]
fn each_decision_has_its_own_outcome() {
    assert_eq!(
        Outcome::of(Decision::Approve),
        Some(Outcome::Granted(GrantState::Waiting))
    );
    assert_eq!(Outcome::of(Decision::Deny), Some(Outcome::Denied));
}

#[gpui_kit::test]
fn setting_the_same_lists_again_does_not_redraw_the_panel(cx: &mut TestAppContext) {
    support::init(cx);
    let win = cx.add_window(|_, cx| ApprovalsPanel::with_cli(CliPath::Bare, cx));
    let panel = win.update(cx, |_, _, cx| cx.entity()).unwrap();
    let redraws = Rc::new(Cell::new(0));
    let seen = Rc::clone(&redraws);
    cx.update(|cx| {
        cx.observe(&panel, move |_, _| seen.set(seen.get() + 1))
            .detach()
    });
    let r = ApprovalRequest::new(Path::new("/w"), "calm", SQL, Writes::Yes, vec![]);
    let set = |cx: &mut TestAppContext| {
        let decided = vec![Decided::new(r.clone(), Outcome::Denied)];
        win.update(cx, |p, _, cx| {
            p.set(vec![r.clone()], decided, Some("/w".into()), cx)
        })
        .unwrap();
        cx.run_until_parked();
    };
    set(cx);
    assert_eq!(redraws.get(), 1);
    set(cx);
    assert_eq!(redraws.get(), 1, "unchanged lists must not notify");
}

#[gpui_kit::test]
fn the_panel_shows_the_appimage_hint_under_a_bare_command(cx: &mut TestAppContext) {
    let win = panel(cx, CliPath::AppImage);
    assert_eq!(
        label_of(cx, win, "approvals-connect").as_deref(),
        Some("dbento --workspace /home/a/shop mcp")
    );
    assert_eq!(
        label_of(cx, win, "approvals-connect-hint").as_deref(),
        Some(APPIMAGE_HINT)
    );
}

#[cfg(unix)]
#[gpui_kit::test]
fn the_panel_names_a_bundled_cli_without_a_hint(cx: &mut TestAppContext) {
    let win = panel(cx, CliPath::Absolute("/opt/dbento/bin/dbento".into()));
    assert_eq!(
        label_of(cx, win, "approvals-connect").as_deref(),
        Some("/opt/dbento/bin/dbento --workspace /home/a/shop mcp")
    );
    assert!(!present(cx, win, "approvals-connect-hint"));
}
#[gpui_kit::test]
fn the_robot_opens_the_panel_in_the_shared_dock(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    assert!(!present(cx, s.win, "approvals-panel"));
    click_id(cx, s.win, "rail-approvals");
    assert!(present(cx, s.win, "approvals-panel"));
    assert!(s.shell.read_with(cx, |sh, _| sh.approvals_open()));
    click_id(cx, s.win, "history-toggle");
    assert!(
        !present(cx, s.win, "approvals-panel"),
        "the dock holds one panel"
    );
    click_id(cx, s.win, "rail-approvals");
    click_id(cx, s.win, "rail-approvals");
    assert!(!present(cx, s.win, "approvals-panel"));
}

#[gpui_kit::test]
fn the_panel_lists_pending_requests(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    click_id(cx, s.win, "rail-approvals");
    file(&s, 1);
    tick(cx);
    assert_eq!(
        label_of(cx, s.win, ("approvals-pending", 0usize)).as_deref(),
        Some("calm \u{B7} INSERT INTO t VALUES (1)")
    );
}

#[gpui_kit::test]
fn an_approval_waits_then_reads_ran_once_the_ai_retries(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    click_id(cx, s.win, "rail-approvals");
    let r = file(&s, 1);
    tick(cx);
    click_id(cx, s.win, "approval-approve");
    tick(cx);
    assert!(!present(cx, s.win, ("approvals-pending", 0usize)));
    assert_eq!(
        decided(cx, &s).as_deref(),
        Some("calm \u{B7} INSERT INTO t VALUES (1) \u{B7} waiting for retry")
    );
    assert_eq!(store(&s).claim(r.id()).unwrap(), Claim::Granted);
    tick(cx);
    assert_eq!(
        decided(cx, &s).as_deref(),
        Some("calm \u{B7} INSERT INTO t VALUES (1) \u{B7} ran")
    );
}

#[gpui_kit::test]
fn a_denial_reads_denied(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    click_id(cx, s.win, "rail-approvals");
    file(&s, 1);
    tick(cx);
    click_id(cx, s.win, "approval-deny");
    tick(cx);
    assert_eq!(
        decided(cx, &s).as_deref(),
        Some("calm \u{B7} INSERT INTO t VALUES (1) \u{B7} denied")
    );
}

#[gpui_kit::test]
fn the_panel_offers_the_mcp_command_to_copy(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    click_id(cx, s.win, "rail-approvals");
    let ws = s
        .shell
        .read_with(cx, |sh, _| sh.workspace().unwrap().dir().to_path_buf());
    let want = connect_command(&CliPath::current(), &ws);
    assert_eq!(label_of(cx, s.win, "approvals-connect"), Some(want.clone()));
    click_id(cx, s.win, "approvals-copy");
    assert_eq!(cx.read_from_clipboard().and_then(|c| c.text()), Some(want));
}

#[gpui_kit::test]
fn an_expired_reading_waits_one_tick_for_the_marker(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    click_id(cx, s.win, "rail-approvals");
    let r = file(&s, 1);
    tick(cx);
    click_id(cx, s.win, "approval-approve");
    tick(cx);
    // The server has taken the grant but not yet written its marker.
    let dir = AppPaths::in_dir(s.files.path()).approvals;
    std::fs::remove_file(dir.join("grants").join(format!("{}.json", r.id()))).unwrap();
    tick(cx);
    assert_eq!(
        decided(cx, &s).as_deref(),
        Some("calm \u{B7} INSERT INTO t VALUES (1) \u{B7} waiting for retry")
    );
    let ms = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let marker = format!(r#"{{"v":1,"id":"{}","at_ms":{ms}}}"#, r.id());
    std::fs::write(dir.join("used").join(format!("{}.json", r.id())), marker).unwrap();
    tick(cx);
    assert_eq!(
        decided(cx, &s).as_deref(),
        Some("calm \u{B7} INSERT INTO t VALUES (1) \u{B7} ran")
    );
}

#[gpui_kit::test]
fn a_grant_gone_without_a_marker_reads_expired_on_the_second_tick(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    click_id(cx, s.win, "rail-approvals");
    let r = file(&s, 1);
    tick(cx);
    click_id(cx, s.win, "approval-approve");
    tick(cx);
    let dir = AppPaths::in_dir(s.files.path()).approvals;
    std::fs::remove_file(dir.join("grants").join(format!("{}.json", r.id()))).unwrap();
    tick(cx);
    tick(cx);
    assert_eq!(
        decided(cx, &s).as_deref(),
        Some("calm \u{B7} INSERT INTO t VALUES (1) \u{B7} expired")
    );
}

#[gpui_kit::test]
fn a_long_statement_never_hides_its_outcome(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    click_id(cx, s.win, "rail-approvals");
    let sql = "UPDATE products SET price = price * 1.1, name = upper(name) WHERE id = 3";
    let r = ApprovalRequest::new(s.dir.path(), "calm", sql, Writes::Yes, vec![]);
    store(&s).request(&r).unwrap();
    tick(cx);
    click_id(cx, s.win, "approval-approve");
    tick(cx);
    assert!(present(cx, s.win, ("approvals-decided", 0usize)));
    cx.update_window(s.win, |_, w, cx| {
        w.render_frame(cx);
        let panel = w.find("approvals-panel").bounds();
        let word = w.find(("approvals-decided-word", 0usize)).bounds();
        assert!(
            word.left() > panel.left() && word.right() <= panel.right(),
            "the outcome word sits inside the panel: {word:?} in {panel:?}"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_mcp_command_follows_a_workspace_switch_at_once(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    click_id(cx, s.win, "rail-approvals");
    let other = tempfile::tempdir().unwrap();
    std::fs::write(other.path().join("dbento.toml"), "dbento = 1\n").unwrap();
    let dir = other.path().to_path_buf();
    cx.update_window(s.win, |_, w, cx| {
        s.shell.update(cx, |sh, cx| sh.switch_workspace(dir, w, cx))
    })
    .unwrap();
    // No poll tick passes, so only the switch itself can update the command.
    cx.run_until_parked();
    let ws = s
        .shell
        .read_with(cx, |sh, _| sh.workspace().unwrap().dir().to_path_buf());
    let want = connect_command(&CliPath::current(), &ws);
    assert_eq!(label_of(cx, s.win, "approvals-connect"), Some(want));
}
