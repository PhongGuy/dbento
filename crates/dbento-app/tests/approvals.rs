//! The approvals dialog: what it shows, what its buttons write, and that no key answers it.
mod support;
use std::time::{Duration, SystemTime};

use dbento_app::app::AppPaths;
use dbento_app::approvals::{
    APPROVAL_POLL, answered_elsewhere_notice, approve_is_danger, approved_notice, class_label,
    headline, pending_count,
};
use dbento_app::confirm::DIALOG_TOP;
use dbento_core::{ApprovalRequest, ApprovalStore, Claim, Decision, Unguarded, Verdict, Writes};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext, px};
use support::{Started, click_id, label_of, present, started, wait_real};

// `lite` is production through its red tag; `calm` is untagged.
const TOML: &str = "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"l.db\"\ncolor = \"red\"\n\n[connections.calm]\nengine = \"sqlite\"\npath = \"c.db\"\n";

fn store(s: &Started) -> ApprovalStore {
    ApprovalStore::open(&AppPaths::in_dir(s.files.path()).approvals).unwrap()
}

/// Files a request for this workspace, `age_s` seconds old.
fn file(
    s: &Started,
    conn: &str,
    sql: &str,
    unguarded: Vec<Unguarded>,
    age_s: u64,
) -> ApprovalRequest {
    let r = ApprovalRequest::new(s.dir.path(), conn, sql, Writes::Yes, unguarded)
        .with_time(SystemTime::now() - Duration::from_secs(age_s));
    store(s).request(&r).unwrap();
    r
}

fn tick(cx: &mut TestAppContext) {
    cx.executor().advance_clock(APPROVAL_POLL);
    cx.run_until_parked();
}

#[gpui_kit::test]
fn a_filed_request_opens_the_dialog_with_what_it_will_run(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    assert!(!present(cx, s.win, "approval-dialog"));
    file(
        &s,
        "lite",
        "DELETE FROM t",
        vec![Unguarded::DeleteWithoutWhere],
        1,
    );
    tick(cx);
    assert!(present(cx, s.win, "approval-dialog"));
    assert_eq!(
        label_of(cx, s.win, "approval-sql").as_deref(),
        Some("DELETE FROM t")
    );
    assert_eq!(
        label_of(cx, s.win, "approval-connection").as_deref(),
        Some("lite \u{B7} DELETE")
    );
    assert_eq!(
        label_of(cx, s.win, "approval-class").as_deref(),
        Some("DELETE without WHERE")
    );
    assert!(present(cx, s.win, "approval-production"));
    let ws = s
        .dir
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(label_of(cx, s.win, "approval-workspace"), Some(ws));
}

#[gpui_kit::test]
fn approve_writes_a_grant_and_closes(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    let r = file(&s, "calm", "INSERT INTO t VALUES (1)", vec![], 1);
    tick(cx);
    assert!(
        !present(cx, s.win, "approval-production"),
        "calm is not production"
    );
    click_id(cx, s.win, "approval-approve");
    assert!(!present(cx, s.win, "approval-dialog"));
    assert!(store(&s).pending().is_empty());
    assert_eq!(store(&s).claim(r.id()).unwrap(), Claim::Granted);
}

#[gpui_kit::test]
fn deny_writes_a_denial_and_closes(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    let r = file(&s, "calm", "INSERT INTO t VALUES (1)", vec![], 1);
    tick(cx);
    click_id(cx, s.win, "approval-deny");
    assert!(!present(cx, s.win, "approval-dialog"));
    assert_eq!(store(&s).claim(r.id()).unwrap(), Claim::Denied);
}

#[gpui_kit::test]
fn deny_sends_the_typed_reason_with_the_denial(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    let r = file(&s, "calm", "INSERT INTO t VALUES (1)", vec![], 1);
    tick(cx);
    assert!(present(cx, s.win, "approval-reason"));
    click_id(cx, s.win, "approval-reason");
    cx.update_window(s.win, |_, w, cx| w.input("use the staging table", cx))
        .unwrap();
    cx.run_until_parked();
    // Enter in the reason field answers nothing; only the buttons do.
    cx.update_window(s.win, |_, w, cx| w.press("enter", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(
        present(cx, s.win, "approval-dialog"),
        "enter closed the dialog"
    );
    click_id(cx, s.win, "approval-deny");
    assert!(!present(cx, s.win, "approval-dialog"));
    assert_eq!(
        store(&s).claim_with_reason(r.id()).unwrap(),
        (Claim::Denied, Some("use the staging table".to_owned()))
    );
}

#[gpui_kit::test]
fn approve_ignores_a_typed_reason(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    let r = file(&s, "calm", "INSERT INTO t VALUES (1)", vec![], 1);
    tick(cx);
    click_id(cx, s.win, "approval-reason");
    cx.update_window(s.win, |_, w, cx| w.input("changed my mind", cx))
        .unwrap();
    cx.run_until_parked();
    click_id(cx, s.win, "approval-approve");
    assert_eq!(
        store(&s).claim_with_reason(r.id()).unwrap(),
        (Claim::Granted, None)
    );
}

#[gpui_kit::test]
fn enter_escape_and_space_answer_nothing(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    let r = file(&s, "calm", "INSERT INTO t VALUES (1)", vec![], 1);
    tick(cx);
    // The keys must reach the dialog for their silence to mean anything.
    let focused = cx
        .update_window(s.win, |_, w, cx| {
            w.render_frame(cx);
            w.find("approval-dialog").focused()
        })
        .unwrap();
    assert_eq!(focused, Some(true), "focus is not inside the dialog");
    for key in ["enter", "escape", "space"] {
        cx.update_window(s.win, |_, w, cx| w.press(key, cx))
            .unwrap();
        assert!(
            present(cx, s.win, "approval-dialog"),
            "{key} closed the dialog"
        );
    }
    assert_eq!(store(&s).pending().len(), 1);
    assert_eq!(store(&s).claim(r.id()).unwrap(), Claim::None);
}

#[gpui_kit::test]
fn requests_queue_oldest_first_and_show_one_at_a_time(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    let older = file(&s, "calm", "INSERT INTO t VALUES (1)", vec![], 20);
    let newer = file(&s, "calm", "INSERT INTO t VALUES (2)", vec![], 10);
    tick(cx);
    assert_eq!(
        label_of(cx, s.win, "approval-sql").as_deref(),
        Some("INSERT INTO t VALUES (1)")
    );
    click_id(cx, s.win, "approval-approve");
    tick(cx);
    assert_eq!(
        label_of(cx, s.win, "approval-sql").as_deref(),
        Some("INSERT INTO t VALUES (2)")
    );
    click_id(cx, s.win, "approval-deny");
    assert_eq!(store(&s).claim(older.id()).unwrap(), Claim::Granted);
    assert_eq!(store(&s).claim(newer.id()).unwrap(), Claim::Denied);
}

#[gpui_kit::test]
fn a_request_for_another_workspace_shows_its_folder_name(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    let elsewhere = tempfile::tempdir().unwrap();
    let r = ApprovalRequest::new(elsewhere.path(), "far", "DROP TABLE x", Writes::Yes, vec![]);
    store(&s).request(&r).unwrap();
    tick(cx);
    assert_eq!(
        label_of(cx, s.win, "approval-workspace"),
        Some(
            elsewhere
                .path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        )
    );
    assert_eq!(
        label_of(cx, s.win, "approval-connection").as_deref(),
        Some("far \u{B7} DROP")
    );
    assert!(!present(cx, s.win, "approval-production"));
}

#[test]
fn class_labels_name_only_what_needs_a_second_look() {
    use Unguarded::{DeleteWithoutWhere, Drop, Truncate, UpdateWithoutWhere};
    assert_eq!(class_label(Writes::Yes, &[]), None);
    assert_eq!(
        class_label(Writes::No, &[]),
        None,
        "a read is not an unparsed write"
    );
    assert_eq!(
        class_label(Writes::Unknown, &[]).as_deref(),
        Some("may write (not parsed)")
    );
    assert_eq!(
        class_label(Writes::Yes, &[UpdateWithoutWhere, DeleteWithoutWhere]).as_deref(),
        Some("UPDATE without WHERE \u{B7} DELETE without WHERE")
    );
    assert_eq!(
        class_label(Writes::Yes, &[Drop, Truncate]).as_deref(),
        Some("DROP \u{B7} TRUNCATE")
    );
    assert_eq!(
        class_label(Writes::Unknown, &[Drop]).as_deref(),
        Some("may write (not parsed) \u{B7} DROP")
    );
}

#[test]
fn headlines_lead_with_the_connection_and_the_statement() {
    assert_eq!(headline("shop", "UPDATE t SET x = 1"), "shop \u{B7} UPDATE");
    assert_eq!(
        headline("shop", "-- why\n  delete from t"),
        "shop \u{B7} DELETE"
    );
    assert_eq!(headline("shop", "  "), "shop");
}

#[test]
fn approve_is_red_only_for_production_or_unguarded_writes() {
    assert!(!approve_is_danger(false, &[]));
    assert!(approve_is_danger(true, &[]));
    assert!(approve_is_danger(false, &[Unguarded::Truncate]));
}

#[test]
fn the_approved_notice_says_what_to_do_next() {
    assert_eq!(
        approved_notice().text.to_string(),
        "Approved. Ask your AI to retry the same call."
    );
}

#[gpui_kit::test]
fn approving_shows_what_to_do_next(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    file(&s, "calm", "INSERT INTO t VALUES (1)", vec![], 1);
    tick(cx);
    assert!(!present(cx, s.win, "notice"));
    click_id(cx, s.win, "approval-approve");
    wait_real(cx, "the approved notice", |cx| {
        label_of(cx, s.win, "notice").as_deref()
            == Some("Approved. Ask your AI to retry the same call.")
    });
}

#[gpui_kit::test]
fn denying_shows_no_notice(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    let r = file(&s, "calm", "INSERT INTO t VALUES (1)", vec![], 1);
    tick(cx);
    click_id(cx, s.win, "approval-deny");
    // The decision lands off the main thread; only once it has can "no notice" mean anything.
    wait_real(cx, "the denial to land", |_| store(&s).pending().is_empty());
    tick(cx);
    assert!(!present(cx, s.win, "notice"));
    assert_eq!(store(&s).claim(r.id()).unwrap(), Claim::Denied);
}

#[gpui_kit::test]
fn several_waiting_requests_show_one_of_n(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    file(&s, "calm", "INSERT INTO t VALUES (1)", vec![], 1);
    tick(cx);
    assert!(
        !present(cx, s.win, "approval-queue"),
        "a lone request has no count"
    );
    click_id(cx, s.win, "approval-deny");
    file(&s, "calm", "INSERT INTO t VALUES (2)", vec![], 3);
    file(&s, "calm", "INSERT INTO t VALUES (3)", vec![], 2);
    tick(cx);
    assert_eq!(
        label_of(cx, s.win, "approval-queue").as_deref(),
        Some("1 of 2")
    );
}

#[test]
fn pending_count_counts_live_requests() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(pending_count(dir.path()), 0);
    let store = ApprovalStore::open(dir.path()).unwrap();
    for sql in ["INSERT INTO t VALUES (1)", "INSERT INTO t VALUES (2)"] {
        let r = ApprovalRequest::new(dir.path(), "calm", sql, Writes::Yes, vec![]);
        store.request(&r).unwrap();
    }
    assert_eq!(pending_count(dir.path()), 2);
}

#[gpui_kit::test]
fn the_dialog_sits_under_the_toolbar_like_the_confirm_dialog(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    cx.simulate_window_resize(s.win, gpui_kit::size(px(1280.), px(800.)));
    file(
        &s,
        "lite",
        "DELETE FROM t",
        vec![Unguarded::DeleteWithoutWhere],
        1,
    );
    tick(cx);
    assert!(present(cx, s.win, "approval-dialog"));
    cx.update_window(s.win, |_, w, cx| {
        w.render_frame(cx);
        let surface = w.find("approval-dialog").bounds();
        let approve = w.find("approval-approve").bounds();
        assert!(
            surface.top() >= DIALOG_TOP && surface.top() < DIALOG_TOP + px(4.),
            "{surface:?}"
        );
        assert!(
            (f32::from(surface.size.width) - 480.).abs() <= 2.,
            "{surface:?}"
        );
        assert_eq!(approve.size.height, px(32.));
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_rail_badge_counts_pending_requests(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    assert!(present(cx, s.win, "rail-approvals"));
    assert!(!present(cx, s.win, "rail-approvals-badge"));
    file(&s, "calm", "INSERT INTO t VALUES (1)", vec![], 2);
    file(&s, "calm", "INSERT INTO t VALUES (2)", vec![], 1);
    tick(cx);
    assert_eq!(
        label_of(cx, s.win, "rail-approvals-badge").as_deref(),
        Some("2")
    );
    click_id(cx, s.win, "approval-approve");
    tick(cx);
    assert_eq!(
        label_of(cx, s.win, "rail-approvals-badge").as_deref(),
        Some("1")
    );
    click_id(cx, s.win, "approval-deny");
    tick(cx);
    assert!(!present(cx, s.win, "rail-approvals-badge"));
}

#[gpui_kit::test]
fn the_rail_badge_stays_live_while_the_dialog_waits(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    file(&s, "calm", "INSERT INTO t VALUES (1)", vec![], 3);
    tick(cx);
    assert!(present(cx, s.win, "approval-dialog"));
    file(&s, "calm", "INSERT INTO t VALUES (2)", vec![], 2);
    file(&s, "calm", "INSERT INTO t VALUES (3)", vec![], 1);
    tick(cx);
    assert!(present(cx, s.win, "approval-dialog"), "nothing answered it");
    assert_eq!(
        label_of(cx, s.win, "rail-approvals-badge").as_deref(),
        Some("3")
    );
}

// The badge's width at `n` pending requests, with its label.
fn badge_at(cx: &mut TestAppContext, s: &Started, n: usize) -> (Option<String>, f32) {
    let have = store(s).pending().len();
    for i in have..n {
        file(s, "calm", &format!("INSERT INTO t VALUES ({i})"), vec![], 1);
    }
    tick(cx);
    let width = cx
        .update_window(s.win, |_, w, cx| {
            w.render_frame(cx);
            f32::from(w.find("rail-approvals-badge").bounds().size.width)
        })
        .unwrap();
    (label_of(cx, s.win, "rail-approvals-badge"), width)
}

#[gpui_kit::test]
fn the_rail_badge_caps_at_99_plus_and_widens_to_fit(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    let (one, round) = badge_at(cx, &s, 1);
    assert_eq!(one.as_deref(), Some("1"));
    assert_eq!(round, 14., "a lone digit keeps the round badge");
    let (most, _) = badge_at(cx, &s, 99);
    assert_eq!(most.as_deref(), Some("99"));
    let (capped, pill) = badge_at(cx, &s, 120);
    assert_eq!(capped.as_deref(), Some("99+"));
    assert!(pill > round, "the 99+ pill is {pill} px wide");
}

const APPROVED_ELSEWHERE: &str =
    "Already approved in another window. Ask your AI to retry the same call.";
const DENIED_ELSEWHERE: &str = "Already denied in another window.";
const ANSWERED_ELSEWHERE: &str = "Already answered in another window.";

#[test]
fn the_answered_elsewhere_notice_names_only_what_the_store_shows() {
    let text = |v| answered_elsewhere_notice(v).text;
    assert_eq!(text(Verdict::Approved), APPROVED_ELSEWHERE);
    assert_eq!(text(Verdict::Denied), DENIED_ELSEWHERE);
    assert_eq!(text(Verdict::Unknown), ANSWERED_ELSEWHERE);
}

// Another app process answers through the same store while this window's dialog is open; the
// click comes before any poll could notice, so this window's own answer is the one that loses.
fn lose_with(
    cx: &mut TestAppContext,
    winner: Decision,
    click: &'static str,
) -> (Started, ApprovalRequest) {
    let s = started(cx, TOML);
    let r = file(&s, "calm", "INSERT INTO t VALUES (1)", vec![], 1);
    tick(cx);
    assert!(present(cx, s.win, "approval-dialog"));
    ApprovalStore::open(&AppPaths::in_dir(s.files.path()).approvals)
        .unwrap()
        .decide(r.id(), winner)
        .unwrap();
    click_id(cx, s.win, click);
    (s, r)
}

#[gpui_kit::test]
fn a_losing_deny_shows_the_approval_another_window_made(cx: &mut TestAppContext) {
    let (s, r) = lose_with(cx, Decision::Approve, "approval-deny");
    wait_real(cx, "the approved-elsewhere notice", |cx| {
        label_of(cx, s.win, "notice").as_deref() == Some(APPROVED_ELSEWHERE)
    });
    assert_eq!(
        store(&s).claim(r.id()).unwrap(),
        Claim::Granted,
        "the first answer stands"
    );
}

#[gpui_kit::test]
fn a_losing_approve_shows_the_denial_another_window_made(cx: &mut TestAppContext) {
    let (s, r) = lose_with(cx, Decision::Deny, "approval-approve");
    wait_real(cx, "the denied-elsewhere notice", |cx| {
        label_of(cx, s.win, "notice").as_deref() == Some(DENIED_ELSEWHERE)
    });
    assert_eq!(
        store(&s).claim(r.id()).unwrap(),
        Claim::Denied,
        "the first answer stands"
    );
}

#[gpui_kit::test]
fn an_answer_with_no_readable_verdict_claims_neither(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    let r = file(&s, "calm", "INSERT INTO t VALUES (1)", vec![], 1);
    tick(cx);
    assert!(present(cx, s.win, "approval-dialog"));
    // The request vanishes without a verdict, as when it is taken but not yet recorded.
    let inbox = AppPaths::in_dir(s.files.path()).approvals.join("inbox");
    std::fs::remove_file(inbox.join(format!("{}.json", r.id()))).unwrap();
    click_id(cx, s.win, "approval-approve");
    wait_real(cx, "the answered-elsewhere notice", |cx| {
        label_of(cx, s.win, "notice").as_deref() == Some(ANSWERED_ELSEWHERE)
    });
    assert_eq!(
        store(&s).claim(r.id()).unwrap(),
        Claim::None,
        "the loser wrote no grant"
    );
}

#[gpui_kit::test]
fn an_open_dialog_closes_when_another_window_answers_it(cx: &mut TestAppContext) {
    let s = started(cx, TOML);
    let r = file(&s, "calm", "INSERT INTO t VALUES (1)", vec![], 1);
    tick(cx);
    assert!(present(cx, s.win, "approval-dialog"));
    store(&s).decide(r.id(), Decision::Deny).unwrap();
    wait_real(cx, "the dialog to close on the other answer", |cx| {
        tick(cx);
        !present(cx, s.win, "approval-dialog")
            && label_of(cx, s.win, "notice").as_deref() == Some(DENIED_ELSEWHERE)
    });
    assert_eq!(store(&s).claim(r.id()).unwrap(), Claim::Denied);
}

#[test]
fn a_mongodb_headline_names_the_method() {
    assert_eq!(
        headline("shop", "db.orders.deleteMany({})"),
        "shop \u{B7} deleteMany"
    );
    assert_eq!(
        headline(
            "shop",
            "  // purge\ndb[\"order lines\"].updateMany({}, {$set: {a: 1}})"
        ),
        "shop \u{B7} updateMany"
    );
    assert_eq!(
        headline("shop", "db.getSiblingDB(\"x(\").getCollection('o)').drop()"),
        "shop \u{B7} drop"
    );
    assert_eq!(
        headline("shop", "db.runCommand({drop: \"o\"})"),
        "shop \u{B7} runCommand"
    );
    assert_eq!(headline("shop", "db.orders"), "shop");
    assert_eq!(headline("shop", "UPDATE t SET x = 1"), "shop \u{B7} UPDATE");
}

#[test]
fn comments_inside_a_mongosh_chain_never_name_the_method() {
    assert_eq!(
        headline("shop", "db.orders/*find(*/.deleteMany({})"),
        "shop \u{B7} deleteMany"
    );
    assert_eq!(
        headline("shop", "db.orders // find(\n  .deleteMany({})"),
        "shop \u{B7} deleteMany"
    );
}
