//! The approval inbox: requests filed by the MCP server, decided in the app, claimed once.
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dbento_core::{
    ApprovalError, ApprovalRequest, ApprovalStore, Claim, Decision, GrantState,
    MAX_DENY_REASON_CHARS, Unguarded, Verdict, Writes, approval_id,
};
use serde_json::json;

const W: &str = "/w";

fn insert() -> ApprovalRequest {
    ApprovalRequest::new(
        Path::new(W),
        "lite",
        "INSERT INTO t VALUES (1)",
        Writes::Yes,
        vec![],
    )
}

fn store() -> (tempfile::TempDir, ApprovalStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = ApprovalStore::open(&dir.path().join("approvals")).unwrap();
    (dir, store)
}

fn ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

/// Writes a grant or denial file by hand, as the format documents it.
fn forge(root: &Path, sub: &str, id: &str, at: SystemTime) {
    let body = json!({"v": 1, "id": id, "at_ms": ms(at)}).to_string();
    std::fs::write(root.join(sub).join(format!("{id}.json")), body).unwrap();
}

#[test]
fn the_id_binds_workspace_connection_and_exact_text() {
    let ws = Path::new(W);
    // sha256 of {"v":1,"workspace":"/w","connection":"lite","sql":"DELETE FROM t"}
    assert_eq!(
        approval_id(ws, "lite", "DELETE FROM t"),
        "ba1c9c9ba39b49f987fc5c46442f71c6738e94bb48f1ccbb31d044838b73360e"
    );
    let id = approval_id(ws, "lite", "DELETE FROM t");
    for other in [
        approval_id(Path::new("/w2"), "lite", "DELETE FROM t"),
        approval_id(ws, "lite2", "DELETE FROM t"),
        approval_id(ws, "lite", "DELETE FROM t "),
        approval_id(ws, "lite", "delete from t"),
    ] {
        assert_ne!(id, other);
    }
    assert_eq!(
        insert().id(),
        approval_id(ws, "lite", "INSERT INTO t VALUES (1)")
    );
}

#[test]
fn the_request_file_holds_the_documented_fields() {
    let (d, store) = store();
    let r = ApprovalRequest::new(
        Path::new(W),
        "lite",
        "UPDATE t SET a = 1",
        Writes::Yes,
        vec![Unguarded::UpdateWithoutWhere],
    )
    .with_time(UNIX_EPOCH + Duration::from_millis(1_700_000_000_123));
    store.request(&r).unwrap();
    let file = d
        .path()
        .join("approvals/inbox")
        .join(format!("{}.json", r.id()));
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    assert_eq!(
        v,
        json!({"v": 1, "id": r.id(), "at_ms": 1_700_000_000_123u64, "workspace": "/w",
               "connection": "lite", "sql": "UPDATE t SET a = 1", "writes": "yes",
               "unguarded": ["update_without_where"]})
    );
}

#[test]
fn a_request_is_pending_until_decided_and_filing_it_twice_keeps_the_first() {
    let (_d, store) = store();
    let first_at = SystemTime::now() - Duration::from_secs(30);
    let r = ApprovalRequest::new(
        Path::new(W),
        "lite",
        "DELETE FROM t",
        Writes::Unknown,
        vec![Unguarded::DeleteWithoutWhere],
    );
    store.request(&r.clone().with_time(first_at)).unwrap();
    store.request(&r).unwrap();
    let pending = store.pending();
    assert_eq!(pending.len(), 1);
    let p = &pending[0];
    assert_eq!(
        (
            p.id(),
            p.workspace(),
            p.connection(),
            p.sql(),
            p.writes(),
            p.unguarded()
        ),
        (
            r.id(),
            Path::new(W),
            "lite",
            "DELETE FROM t",
            Writes::Unknown,
            &[Unguarded::DeleteWithoutWhere][..]
        )
    );
    assert_eq!(ms(p.time()), ms(first_at));
    assert_eq!(store.claim(r.id()).unwrap(), Claim::None);
    assert_eq!(
        store.pending().len(),
        1,
        "an unanswered claim leaves the request"
    );
}

#[test]
fn approve_grants_one_claim_and_removes_the_request() {
    let (_d, store) = store();
    let r = insert();
    store.request(&r).unwrap();
    store.decide(r.id(), Decision::Approve).unwrap();
    assert!(store.pending().is_empty());
    assert_eq!(store.claim(r.id()).unwrap(), Claim::Granted);
    assert_eq!(store.claim(r.id()).unwrap(), Claim::None);
}

#[test]
fn deny_answers_one_claim_and_wins_over_a_grant() {
    let (d, store) = store();
    let r = insert();
    store.request(&r).unwrap();
    store.decide(r.id(), Decision::Deny).unwrap();
    assert!(store.pending().is_empty());
    assert_eq!(store.claim(r.id()).unwrap(), Claim::Denied);
    assert_eq!(store.claim(r.id()).unwrap(), Claim::None);
    let root = d.path().join("approvals");
    forge(&root, "grants", r.id(), SystemTime::now());
    forge(&root, "denied", r.id(), SystemTime::now());
    assert_eq!(store.claim(r.id()).unwrap(), Claim::Denied);
    assert_eq!(
        store.claim(r.id()).unwrap(),
        Claim::None,
        "a denial also consumes the grant"
    );
}

#[test]
fn a_second_decide_is_not_pending_and_the_first_decision_stands() {
    for (first, second, verdict, claim) in [
        (
            Decision::Approve,
            Decision::Deny,
            Verdict::Approved,
            Claim::Granted,
        ),
        (
            Decision::Deny,
            Decision::Approve,
            Verdict::Denied,
            Claim::Denied,
        ),
    ] {
        let (d, store) = store();
        let other = ApprovalStore::open(&d.path().join("approvals")).unwrap();
        let r = insert();
        store.request(&r).unwrap();
        assert_eq!(store.verdict(&r), Verdict::Unknown, "nothing decided yet");
        store.decide(r.id(), first).unwrap();
        assert!(matches!(
            other.decide(r.id(), second),
            Err(ApprovalError::NotPending { .. })
        ));
        assert_eq!(other.verdict(&r), verdict);
        assert_eq!(store.claim(r.id()).unwrap(), claim);
        // The verdict outlives the claim, so a late reader still learns the answer.
        assert_eq!(other.verdict(&r), verdict);
    }
}

#[test]
fn of_two_racing_decides_exactly_one_wins_and_its_verdict_is_stored() {
    for _ in 0..50 {
        let (d, store) = store();
        let other = ApprovalStore::open(&d.path().join("approvals")).unwrap();
        let r = insert();
        store.request(&r).unwrap();
        let barrier = std::sync::Barrier::new(2);
        let results = std::thread::scope(|s| {
            let handles =
                [(&store, Decision::Approve), (&other, Decision::Deny)].map(|(st, dec)| {
                    let (barrier, id) = (&barrier, r.id());
                    s.spawn(move || {
                        barrier.wait();
                        st.decide(id, dec).map(|()| dec)
                    })
                });
            handles.map(|h| h.join().unwrap())
        });
        let won: Vec<Decision> = results
            .iter()
            .filter_map(|r| r.as_ref().ok().copied())
            .collect();
        assert_eq!(won.len(), 1, "{results:?}");
        let (verdict, claim) = match won[0] {
            Decision::Approve => (Verdict::Approved, Claim::Granted),
            _ => (Verdict::Denied, Claim::Denied),
        };
        assert_eq!(store.verdict(&r), verdict);
        assert_eq!(store.claim(r.id()).unwrap(), claim);
    }
}

#[test]
fn a_verdict_from_before_the_request_is_unknown() {
    let (_d, store) = store();
    let old = insert().with_time(SystemTime::now() - Duration::from_secs(60));
    store.request(&old).unwrap();
    store.decide(old.id(), Decision::Approve).unwrap();
    // The same call filed again later: the old answer does not speak for it.
    let again = insert().with_time(SystemTime::now() + Duration::from_secs(1));
    store.request(&again).unwrap();
    assert_eq!(store.verdict(&again), Verdict::Unknown);
    assert_eq!(store.verdict(&old), Verdict::Approved);
}

#[test]
fn deciding_an_unknown_or_malformed_id_is_refused() {
    let (_d, store) = store();
    let unknown = approval_id(Path::new(W), "lite", "X");
    assert!(matches!(
        store.decide(&unknown, Decision::Approve),
        Err(ApprovalError::NotPending { .. })
    ));
    let upper = "A".repeat(64);
    let not_hex = "g".repeat(64);
    for bad in [
        "../../etc/passwd",
        "",
        "abc",
        upper.as_str(),
        not_hex.as_str(),
    ] {
        assert!(
            matches!(
                store.decide(bad, Decision::Approve),
                Err(ApprovalError::InvalidId { .. })
            ),
            "{bad}"
        );
        assert!(
            matches!(store.claim(bad), Err(ApprovalError::InvalidId { .. })),
            "{bad}"
        );
    }
}

#[test]
fn a_grant_or_denial_older_than_an_hour_is_discarded() {
    let (d, store) = store();
    let root = d.path().join("approvals");
    let id = approval_id(Path::new(W), "lite", "X");
    let old = SystemTime::now() - Duration::from_secs(3601);
    forge(&root, "grants", &id, old);
    assert_eq!(store.claim(&id).unwrap(), Claim::None);
    assert!(!root.join("grants").join(format!("{id}.json")).exists());
    forge(&root, "denied", &id, old);
    assert_eq!(store.claim(&id).unwrap(), Claim::None);
    forge(
        &root,
        "grants",
        &id,
        SystemTime::now() - Duration::from_secs(3500),
    );
    assert_eq!(store.claim(&id).unwrap(), Claim::Granted);
}

#[test]
fn pending_prunes_stale_requests_and_decisions() {
    let (d, store) = store();
    let root = d.path().join("approvals");
    let day = Duration::from_secs(24 * 3600);
    let stale = insert().with_time(SystemTime::now() - day - Duration::from_secs(1));
    let fresh = ApprovalRequest::new(Path::new(W), "lite", "DELETE FROM t", Writes::Yes, vec![])
        .with_time(SystemTime::now() - day + Duration::from_secs(60));
    store.request(&stale).unwrap();
    store.request(&fresh).unwrap();
    let old_grant = approval_id(Path::new(W), "lite", "G");
    forge(
        &root,
        "grants",
        &old_grant,
        SystemTime::now() - Duration::from_secs(3601),
    );
    let ids: Vec<String> = store.pending().iter().map(|r| r.id().to_owned()).collect();
    assert_eq!(ids, [fresh.id()]);
    assert!(
        !root
            .join("inbox")
            .join(format!("{}.json", stale.id()))
            .exists()
    );
    assert!(
        !root
            .join("grants")
            .join(format!("{old_grant}.json"))
            .exists()
    );
}

#[test]
fn pending_is_oldest_first_and_drops_a_file_whose_id_does_not_match_its_content() {
    let (d, store) = store();
    let root = d.path().join("approvals");
    let now = SystemTime::now();
    let a = insert().with_time(now - Duration::from_secs(5));
    let b = ApprovalRequest::new(Path::new(W), "lite", "DELETE FROM t", Writes::Yes, vec![])
        .with_time(now - Duration::from_secs(10));
    store.request(&a).unwrap();
    store.request(&b).unwrap();
    let ids = |s: &ApprovalStore| -> Vec<String> {
        s.pending().iter().map(|r| r.id().to_owned()).collect()
    };
    assert_eq!(ids(&store), [b.id(), a.id()]);
    // The file keeps a's name and id but now carries other SQL.
    let forged = root.join("inbox").join(format!("{}.json", a.id()));
    let text = std::fs::read_to_string(&forged)
        .unwrap()
        .replace("INSERT INTO t VALUES (1)", "DROP TABLE t");
    std::fs::write(&forged, text).unwrap();
    std::fs::write(root.join("inbox").join("junk.json"), "{").unwrap();
    assert_eq!(ids(&store), [b.id()]);
    assert!(!forged.exists());
}

#[test]
fn of_two_racing_claims_exactly_one_wins() {
    for _ in 0..50 {
        let (d, store) = store();
        let other = ApprovalStore::open(&d.path().join("approvals")).unwrap();
        let r = insert();
        store.request(&r).unwrap();
        store.decide(r.id(), Decision::Approve).unwrap();
        let barrier = std::sync::Barrier::new(2);
        let wins = std::thread::scope(|s| {
            let handles = [&store, &other].map(|st| {
                let (barrier, id) = (&barrier, r.id());
                s.spawn(move || {
                    barrier.wait();
                    st.claim(id).unwrap()
                })
            });
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .filter(|c| *c == Claim::Granted)
                .count()
        });
        assert_eq!(wins, 1);
    }
}

#[cfg(unix)]
#[test]
fn folders_are_private_and_files_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("approvals");
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    let store = ApprovalStore::open(&root).unwrap();
    let r = insert();
    store.request(&r).unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    for sub in ["", "inbox", "grants", "denied", "used", "decided"] {
        assert_eq!(mode(&root.join(sub)), 0o700, "{sub}");
    }
    assert_eq!(
        mode(&root.join("inbox").join(format!("{}.json", r.id()))),
        0o600
    );
    store.decide(r.id(), Decision::Approve).unwrap();
    assert_eq!(
        mode(&root.join("grants").join(format!("{}.json", r.id()))),
        0o600
    );
}

#[test]
fn a_folder_that_cannot_be_created_is_unavailable() {
    let d = tempfile::tempdir().unwrap();
    let file = d.path().join("file");
    std::fs::write(&file, "x").unwrap();
    assert!(matches!(
        ApprovalStore::open(&file.join("approvals")),
        Err(ApprovalError::Unavailable { .. })
    ));
}

#[test]
fn drop_and_truncate_round_trip_and_an_unknown_kind_is_skipped() {
    let (d, store) = store();
    let r = ApprovalRequest::new(
        Path::new(W),
        "lite",
        "DROP TABLE t",
        Writes::Yes,
        vec![Unguarded::Drop, Unguarded::Truncate],
    );
    store.request(&r).unwrap();
    assert_eq!(
        store.pending()[0].unguarded(),
        [Unguarded::Drop, Unguarded::Truncate]
    );
    let file = d
        .path()
        .join("approvals")
        .join("inbox")
        .join(format!("{}.json", r.id()));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains(r#""unguarded":["drop","truncate"]"#),
        "{text}"
    );
    std::fs::write(&file, text.replace(r#""truncate""#, r#""future_kind""#)).unwrap();
    let pending = store.pending();
    assert_eq!(
        pending.len(),
        1,
        "a file naming an unknown kind is still read"
    );
    assert_eq!(pending[0].unguarded(), [Unguarded::Drop]);
}

#[test]
fn a_grant_waits_until_claimed_then_reads_ran() {
    let (_dir, store) = store();
    let r = insert();
    store.request(&r).unwrap();
    store.decide(r.id(), Decision::Approve).unwrap();
    assert_eq!(store.grant_state(r.id()), GrantState::Waiting);
    assert_eq!(store.claim(r.id()).unwrap(), Claim::Granted);
    assert_eq!(store.grant_state(r.id()), GrantState::Ran);
    assert!(
        store
            .dir()
            .join("used")
            .join(format!("{}.json", r.id()))
            .is_file()
    );
}

#[test]
fn approving_the_same_call_again_reads_waiting() {
    let (_dir, store) = store();
    let r = insert();
    store.request(&r).unwrap();
    store.decide(r.id(), Decision::Approve).unwrap();
    store.claim(r.id()).unwrap();
    store.request(&r).unwrap();
    store.decide(r.id(), Decision::Approve).unwrap();
    assert_eq!(store.grant_state(r.id()), GrantState::Waiting);
    assert!(
        !store
            .dir()
            .join("used")
            .join(format!("{}.json", r.id()))
            .exists()
    );
}

#[test]
fn unknown_denied_malformed_and_stale_grants_read_expired() {
    let (dir, store) = store();
    let root = dir.path().join("approvals");
    let r = insert();
    assert_eq!(store.grant_state(r.id()), GrantState::Expired);
    assert_eq!(store.grant_state("../x"), GrantState::Expired);
    store.request(&r).unwrap();
    store.decide(r.id(), Decision::Deny).unwrap();
    assert_eq!(store.grant_state(r.id()), GrantState::Expired);
    forge(
        &root,
        "grants",
        r.id(),
        SystemTime::now() - Duration::from_secs(3601),
    );
    assert_eq!(store.grant_state(r.id()), GrantState::Expired);
}

#[test]
fn a_used_marker_older_than_an_hour_is_expired_and_pruned() {
    let (dir, store) = store();
    let root = dir.path().join("approvals");
    let id = insert().id().to_owned();
    forge(
        &root,
        "used",
        &id,
        SystemTime::now() - Duration::from_secs(3601),
    );
    assert_eq!(store.grant_state(&id), GrantState::Expired);
    store.pending();
    assert!(!root.join("used").join(format!("{id}.json")).exists());
}

#[test]
fn a_time_far_in_the_future_is_expired_and_a_little_skew_is_not() {
    let (d, store) = store();
    let root = d.path().join("approvals");
    let id = approval_id(Path::new(W), "lite", "X");
    forge(
        &root,
        "grants",
        &id,
        SystemTime::now() + Duration::from_secs(3600),
    );
    assert_eq!(store.claim(&id).unwrap(), Claim::None);
    forge(
        &root,
        "grants",
        &id,
        SystemTime::now() + Duration::from_secs(60),
    );
    assert_eq!(store.claim(&id).unwrap(), Claim::Granted);
    let future = insert().with_time(SystemTime::now() + Duration::from_secs(24 * 3600));
    store.request(&future).unwrap();
    assert!(store.pending().is_empty());
}

#[test]
fn deciding_an_inbox_file_that_is_not_its_request_is_not_pending() {
    let (d, store) = store();
    let root = d.path().join("approvals");
    let a = insert();
    let b = ApprovalRequest::new(Path::new(W), "lite", "DELETE FROM t", Writes::Yes, vec![]);
    let inbox = |id: &str| root.join("inbox").join(format!("{id}.json"));
    std::fs::write(inbox(a.id()), "{").unwrap();
    assert!(matches!(
        store.decide(a.id(), Decision::Approve),
        Err(ApprovalError::NotPending { .. })
    ));
    // b's valid request filed under a's name.
    store.request(&b).unwrap();
    std::fs::rename(inbox(b.id()), inbox(a.id())).unwrap();
    assert!(matches!(
        store.decide(a.id(), Decision::Approve),
        Err(ApprovalError::NotPending { .. })
    ));
    assert!(
        !root
            .join("grants")
            .join(format!("{}.json", a.id()))
            .exists()
    );
    assert_eq!(store.verdict(&a), Verdict::Unknown);
}

#[test]
fn a_retry_replaces_an_expired_request_that_was_never_pruned() {
    let (_d, store) = store();
    let old = insert().with_time(SystemTime::now() - Duration::from_secs(25 * 3600));
    store.request(&old).unwrap();
    let fresh = insert();
    store.request(&fresh).unwrap();
    let pending = store.pending();
    assert_eq!(pending.len(), 1, "the retry survives the prune");
    assert_eq!(ms(pending[0].time()), ms(fresh.time()));
}

#[test]
fn a_denial_clears_the_marker_of_an_earlier_run() {
    let (_dir, store) = store();
    let r = insert();
    store.request(&r).unwrap();
    store.decide(r.id(), Decision::Approve).unwrap();
    store.claim(r.id()).unwrap();
    store.request(&r).unwrap();
    store.decide(r.id(), Decision::Deny).unwrap();
    assert_eq!(store.grant_state(r.id()), GrantState::Expired);
    assert!(
        !store
            .dir()
            .join("used")
            .join(format!("{}.json", r.id()))
            .exists()
    );
}

#[test]
fn a_denial_carries_its_trimmed_reason_to_the_claim_once() {
    let (_d, store) = store();
    let r = insert();
    store.request(&r).unwrap();
    store
        .decide_with_reason(r.id(), Decision::Deny, Some("  wrong table, use t2 \n"))
        .unwrap();
    assert_eq!(
        store.claim_with_reason(r.id()).unwrap(),
        (Claim::Denied, Some("wrong table, use t2".to_owned()))
    );
    assert_eq!(
        store.claim_with_reason(r.id()).unwrap(),
        (Claim::None, None)
    );
}

#[test]
fn a_blank_reason_is_none_and_a_long_one_is_cut_at_a_char_boundary() {
    let (_d, store) = store();
    let blank = insert();
    store.request(&blank).unwrap();
    store
        .decide_with_reason(blank.id(), Decision::Deny, Some("   "))
        .unwrap();
    assert_eq!(
        store.claim_with_reason(blank.id()).unwrap(),
        (Claim::Denied, None)
    );
    let long = ApprovalRequest::new(Path::new(W), "lite", "DELETE FROM t", Writes::Yes, vec![]);
    store.request(&long).unwrap();
    store
        .decide_with_reason(long.id(), Decision::Deny, Some(&"é".repeat(2000)))
        .unwrap();
    let (claim, reason) = store.claim_with_reason(long.id()).unwrap();
    assert_eq!(claim, Claim::Denied);
    assert_eq!(reason, Some("é".repeat(MAX_DENY_REASON_CHARS)));
}

#[test]
fn an_approval_keeps_no_reason_and_an_old_denial_file_reads_without_one() {
    let (d, store) = store();
    let r = insert();
    store.request(&r).unwrap();
    store
        .decide_with_reason(r.id(), Decision::Approve, Some("fine"))
        .unwrap();
    assert_eq!(
        store.claim_with_reason(r.id()).unwrap(),
        (Claim::Granted, None)
    );
    // A denial written before reasons existed has no `reason` field.
    forge(
        &d.path().join("approvals"),
        "denied",
        r.id(),
        SystemTime::now(),
    );
    assert_eq!(
        store.claim_with_reason(r.id()).unwrap(),
        (Claim::Denied, None)
    );
}

#[test]
fn the_document_kinds_round_trip_through_a_request_file() {
    let (d, store) = store();
    let kinds = vec![Unguarded::UpdateAllDocuments, Unguarded::DeleteAllDocuments];
    let r = ApprovalRequest::new(
        Path::new(W),
        "shop",
        "db.t.deleteMany({})",
        Writes::Yes,
        kinds.clone(),
    );
    store.request(&r).unwrap();
    assert_eq!(store.pending()[0].unguarded(), kinds);
    let file = d
        .path()
        .join("approvals")
        .join("inbox")
        .join(format!("{}.json", r.id()));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains(r#""unguarded":["update_all_documents","delete_all_documents"]"#),
        "{text}"
    );
}

#[test]
fn flush_keys_round_trips_through_a_request_file() {
    let (d, store) = store();
    let r = ApprovalRequest::new(
        Path::new(W),
        "cache",
        "FLUSHALL",
        Writes::Yes,
        vec![Unguarded::FlushKeys],
    );
    store.request(&r).unwrap();
    assert_eq!(store.pending()[0].unguarded(), [Unguarded::FlushKeys]);
    let file = d
        .path()
        .join("approvals/inbox")
        .join(format!("{}.json", r.id()));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains(r#""unguarded":["flush_keys"]"#), "{text}");
}
