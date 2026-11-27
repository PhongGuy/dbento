//! The approval inbox: requests filed by the MCP server, decided in the app, claimed once.
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dbento_core::{
    ApprovalError, ApprovalRequest, ApprovalStore, ApprovalTarget, Claim, ConnectionConfig,
    Decision, GrantState, MAX_DENY_REASON_CHARS, Unguarded, Verdict, Workspace, Writes,
    approval_id,
};
use serde_json::json;
use sha2::{Digest, Sha256};

const W: &str = "/w";

/// Connection `name` of the dbento.toml `text`, for a workspace at /w.
fn conn(text: &str, name: &str) -> ConnectionConfig {
    Workspace::from_text(PathBuf::from(W), &Path::new(W).join("dbento.toml"), text)
        .unwrap()
        .declared(name)
        .unwrap()
        .clone()
}

/// The target of a SQLite `lite` at `path`, plus any `extra` lines.
fn lite_at(path: &str, extra: &str) -> ApprovalTarget {
    ApprovalTarget::of(&conn(
        &format!("dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"{path}\"\n{extra}"),
        "lite",
    ))
}

fn lite() -> ApprovalTarget {
    lite_at("a.db", "")
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn insert() -> ApprovalRequest {
    ApprovalRequest::new(
        Path::new(W),
        "lite",
        lite(),
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

/// Writes a grant, denial or used marker by hand at format version `v`.
fn forge_v(root: &Path, sub: &str, id: &str, at: SystemTime, v: u32) {
    let body = json!({"v": v, "id": id, "at_ms": ms(at)}).to_string();
    std::fs::write(root.join(sub).join(format!("{id}.json")), body).unwrap();
}

/// The store's current file format, as forged files must carry it.
const FORMAT: u32 = 2;

fn forge(root: &Path, sub: &str, id: &str, at: SystemTime) {
    forge_v(root, sub, id, at, FORMAT);
}

#[test]
fn the_id_binds_workspace_connection_exact_text_and_target() {
    let ws = Path::new(W);
    let id = approval_id(ws, "lite", "DELETE FROM t", &lite());
    assert_eq!(
        id,
        "d8f341cca98e01448bfac7617d8b7122de484624605ead079bf4a68c965030fe"
    );
    let input = json!({"v": 2, "workspace": "/w", "connection": "lite", "sql": "DELETE FROM t",
                       "target": lite().fingerprint()});
    assert_eq!(id, hex(input.to_string().as_bytes()));
    for other in [
        approval_id(Path::new("/w2"), "lite", "DELETE FROM t", &lite()),
        approval_id(ws, "lite2", "DELETE FROM t", &lite()),
        approval_id(ws, "lite", "DELETE FROM t ", &lite()),
        approval_id(ws, "lite", "delete from t", &lite()),
        approval_id(ws, "lite", "DELETE FROM t", &lite_at("b.db", "")),
        approval_id(
            ws,
            "lite",
            "DELETE FROM t",
            &lite_at("a.db", "read_only = true\n"),
        ),
    ] {
        assert_ne!(id, other);
    }
    assert_eq!(
        id,
        approval_id(
            ws,
            "lite",
            "DELETE FROM t",
            &lite_at("a.db", "color = \"red\"\n")
        )
    );
    assert_eq!(
        insert().id(),
        approval_id(ws, "lite", "INSERT INTO t VALUES (1)", &lite())
    );
}

#[test]
fn a_target_is_the_hashed_binding_and_the_display() {
    let c = conn(
        "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"a.db\"\n",
        "lite",
    );
    let t = ApprovalTarget::of(&c);
    assert_eq!(
        t.fingerprint(),
        "1be41e82ba6713978921dd5cfc084c0a3af4755174f4421bff2b55b55afd3f38"
    );
    assert_eq!(t.fingerprint(), hex(c.session_binding().as_bytes()));
    assert_eq!(t.display(), "a.db");
    assert_eq!(t, lite_at("a.db", "color = \"red\"\n"));
    assert_ne!(t, lite_at("b.db", ""));
}

#[test]
fn the_request_file_holds_the_documented_fields() {
    let (d, store) = store();
    let r = ApprovalRequest::new(
        Path::new(W),
        "lite",
        lite(),
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
        json!({"v": FORMAT, "id": r.id(), "at_ms": 1_700_000_000_123u64, "workspace": "/w",
               "connection": "lite", "sql": "UPDATE t SET a = 1", "writes": "yes",
               "unguarded": ["update_without_where"],
               "target": "1be41e82ba6713978921dd5cfc084c0a3af4755174f4421bff2b55b55afd3f38",
               "target_display": "a.db"})
    );
}

#[test]
fn a_filed_request_reads_back_with_its_target() {
    let (_d, store) = store();
    let r = ApprovalRequest::new(
        Path::new(W),
        "lite",
        lite_at("b.db", ""),
        "DELETE FROM t",
        Writes::Yes,
        vec![],
    );
    store.request(&r).unwrap();
    let pending = store.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].target(), &lite_at("b.db", ""));
    assert_eq!(pending[0].target().display(), "b.db");
    assert_eq!(r.target(), &lite_at("b.db", ""));
}

#[test]
fn a_grant_for_one_target_never_serves_another() {
    let (_d, store) = store();
    let x = ApprovalRequest::new(
        Path::new(W),
        "lite",
        lite(),
        "DELETE FROM t",
        Writes::Yes,
        vec![],
    );
    store.request(&x).unwrap();
    store.decide(x.id(), Decision::Approve).unwrap();
    let y = approval_id(Path::new(W), "lite", "DELETE FROM t", &lite_at("b.db", ""));
    assert_eq!(store.claim(&y).unwrap(), Claim::None);
    assert_eq!(store.claim(x.id()).unwrap(), Claim::Granted);
}

#[test]
fn a_request_whose_target_was_edited_is_dropped() {
    let (d, store) = store();
    let r = insert();
    store.request(&r).unwrap();
    let file = d
        .path()
        .join("approvals/inbox")
        .join(format!("{}.json", r.id()));
    let text = std::fs::read_to_string(&file)
        .unwrap()
        .replace(lite().fingerprint(), lite_at("b.db", "").fingerprint());
    std::fs::write(&file, text).unwrap();
    assert!(store.pending().is_empty());
    assert!(!file.exists());
    assert!(matches!(
        store.decide(r.id(), Decision::Approve),
        Err(ApprovalError::NotPending { .. })
    ));
}

#[test]
fn a_request_without_its_target_fields_is_dropped() {
    let (d, store) = store();
    let r = insert();
    store.request(&r).unwrap();
    let file = d
        .path()
        .join("approvals/inbox")
        .join(format!("{}.json", r.id()));
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    v.as_object_mut().unwrap().remove("target_display");
    std::fs::write(&file, v.to_string()).unwrap();
    assert!(store.pending().is_empty());
}

#[test]
fn a_request_is_pending_until_decided_and_filing_it_twice_keeps_the_first() {
    let (_d, store) = store();
    let first_at = SystemTime::now() - Duration::from_secs(30);
    let r = ApprovalRequest::new(
        Path::new(W),
        "lite",
        lite(),
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
            Decision::Deny | _ => (Verdict::Denied, Claim::Denied),
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
    let unknown = approval_id(Path::new(W), "lite", "X", &lite());
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
    let id = approval_id(Path::new(W), "lite", "X", &lite());
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
    let fresh = ApprovalRequest::new(
        Path::new(W),
        "lite",
        lite(),
        "DELETE FROM t",
        Writes::Yes,
        vec![],
    )
    .with_time(SystemTime::now() - day + Duration::from_secs(60));
    store.request(&stale).unwrap();
    store.request(&fresh).unwrap();
    let old_grant = approval_id(Path::new(W), "lite", "G", &lite());
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
    let b = ApprovalRequest::new(
        Path::new(W),
        "lite",
        lite(),
        "DELETE FROM t",
        Writes::Yes,
        vec![],
    )
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
        lite(),
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
    let id = approval_id(Path::new(W), "lite", "X", &lite());
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
    let b = ApprovalRequest::new(
        Path::new(W),
        "lite",
        lite(),
        "DELETE FROM t",
        Writes::Yes,
        vec![],
    );
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
    let long = ApprovalRequest::new(
        Path::new(W),
        "lite",
        lite(),
        "DELETE FROM t",
        Writes::Yes,
        vec![],
    );
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
        lite(),
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
        lite(),
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

#[test]
fn a_v1_request_is_pruned_and_cannot_be_decided() {
    let (d, store) = store();
    let v1 = json!({"v": 1, "workspace": "/w", "connection": "lite", "sql": "DELETE FROM t"});
    let id = hex(v1.to_string().as_bytes());
    let body = json!({"v": 1, "id": id, "at_ms": ms(SystemTime::now()), "workspace": "/w",
                      "connection": "lite", "sql": "DELETE FROM t", "writes": "yes", "unguarded": []});
    let file = d.path().join("approvals/inbox").join(format!("{id}.json"));
    std::fs::write(&file, body.to_string()).unwrap();
    assert!(store.pending().is_empty());
    assert!(!file.exists());
    std::fs::write(&file, body.to_string()).unwrap();
    assert!(matches!(
        store.decide(&id, Decision::Approve),
        Err(ApprovalError::NotPending { .. })
    ));
}

#[test]
fn a_v1_grant_never_claims() {
    let (d, store) = store();
    let id = insert().id().to_owned();
    forge_v(
        &d.path().join("approvals"),
        "grants",
        &id,
        SystemTime::now(),
        1,
    );
    assert_eq!(store.grant_state(&id), GrantState::Expired);
    assert_eq!(store.claim(&id).unwrap(), Claim::None);
}

#[test]
fn a_v1_denial_never_claims() {
    let (d, store) = store();
    let id = insert().id().to_owned();
    forge_v(
        &d.path().join("approvals"),
        "denied",
        &id,
        SystemTime::now(),
        1,
    );
    assert_eq!(store.claim(&id).unwrap(), Claim::None);
}

#[test]
fn a_v1_verdict_reads_unknown() {
    let (d, store) = store();
    let r = insert().with_time(SystemTime::now() - Duration::from_secs(5));
    let body = json!({"v": 1, "id": r.id(), "at_ms": ms(SystemTime::now()), "verdict": "approved"});
    std::fs::write(
        d.path()
            .join("approvals/decided")
            .join(format!("{}.json", r.id())),
        body.to_string(),
    )
    .unwrap();
    assert_eq!(store.verdict(&r), Verdict::Unknown);
}

#[test]
fn a_v1_used_marker_does_not_read_ran() {
    let (d, store) = store();
    let id = insert().id().to_owned();
    forge_v(
        &d.path().join("approvals"),
        "used",
        &id,
        SystemTime::now(),
        1,
    );
    assert_eq!(store.grant_state(&id), GrantState::Expired);
}

#[test]
fn a_request_file_keeps_the_binding_hashed() {
    let (d, store) = store();
    let pg = conn(
        "dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"db.internal\"\n\
                   database = \"d\"\nuser = \"u\"\nsecret = { env = \"DBENTO_SECRET_T_PGPW\" }\n\
                   settings = { search_path = \"app\" }\n",
        "pg",
    );
    let r = ApprovalRequest::new(
        Path::new(W),
        "pg",
        ApprovalTarget::of(&pg),
        "DELETE FROM t",
        Writes::Yes,
        vec![],
    );
    store.request(&r).unwrap();
    let text = std::fs::read_to_string(
        d.path()
            .join("approvals/inbox")
            .join(format!("{}.json", r.id())),
    )
    .unwrap();
    assert!(
        text.contains(r#""target_display":"u@db.internal:5432/d""#),
        "{text}"
    );
    for hidden in [
        "DBENTO_SECRET_T_PGPW",
        "search_path",
        "session_binding",
        "secret",
    ] {
        assert!(!text.contains(hidden), "{hidden} in {text}");
    }
}

fn grant_json(store: &ApprovalStore, id: &str) -> serde_json::Value {
    let path = store.dir().join("grants").join(format!("{id}.json"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn delete_at(path: &str) -> ApprovalRequest {
    ApprovalRequest::new(
        Path::new(W),
        "lite",
        lite_at(path, ""),
        "DELETE FROM t",
        Writes::Yes,
        vec![],
    )
}

#[test]
fn an_approval_records_the_call_and_a_denial_does_not() {
    let (_d, store) = store();
    let x = delete_at("a.db");
    store.request(&x).unwrap();
    store.decide(x.id(), Decision::Approve).unwrap();
    let call = hex(
        json!({"v": 2, "workspace": "/w", "connection": "lite", "sql": "DELETE FROM t"})
            .to_string()
            .as_bytes(),
    );
    assert_eq!(
        call,
        "9fcb06c2842ff6e587b27d22a2c549586c3286b077cc8975035e82dbe93f9737"
    );
    let g = grant_json(&store, x.id());
    assert_eq!((g["v"].clone(), g["call"].clone()), (json!(2), json!(call)));
    let y = delete_at("b.db");
    store.request(&y).unwrap();
    store.decide(y.id(), Decision::Deny).unwrap();
    let denial =
        std::fs::read_to_string(store.dir().join("denied").join(format!("{}.json", y.id())))
            .unwrap();
    assert!(!denial.contains("call"), "{denial}");
}

#[test]
fn changed_grant_finds_the_grant_for_the_old_target() {
    let (_d, store) = store();
    let x = delete_at("a.db");
    store.request(&x).unwrap();
    store.decide(x.id(), Decision::Approve).unwrap();
    let y = delete_at("b.db");
    assert_eq!(store.changed_grant(&y).as_deref(), Some(x.id()));
    assert_eq!(
        store.changed_grant(&x),
        None,
        "its own grant is not a changed one"
    );
    let other_sql = ApprovalRequest::new(
        Path::new(W),
        "lite",
        lite_at("b.db", ""),
        "DELETE FROM u",
        Writes::Yes,
        vec![],
    );
    assert_eq!(store.changed_grant(&other_sql), None);
    let other_conn = ApprovalRequest::new(
        Path::new(W),
        "lite2",
        lite_at("b.db", ""),
        "DELETE FROM t",
        Writes::Yes,
        vec![],
    );
    assert_eq!(store.changed_grant(&other_conn), None);
    assert_eq!(store.claim(x.id()).unwrap(), Claim::Granted);
    assert_eq!(store.changed_grant(&y), None, "a claimed grant is gone");
}

#[test]
fn changed_grant_ignores_expired_callless_and_malformed_grants() {
    let (d, store) = store();
    let root = d.path().join("approvals");
    let x = delete_at("a.db");
    store.request(&x).unwrap();
    store.decide(x.id(), Decision::Approve).unwrap();
    let path = root.join("grants").join(format!("{}.json", x.id()));
    let mut g = grant_json(&store, x.id());
    g["at_ms"] = json!(ms(SystemTime::now() - Duration::from_secs(3601)));
    std::fs::write(&path, g.to_string()).unwrap();
    std::fs::write(root.join("grants").join("junk.json"), "{").unwrap();
    std::fs::write(
        root.join("grants").join(format!("{}.json", "f".repeat(64))),
        "{",
    )
    .unwrap();
    let y = delete_at("b.db");
    assert_eq!(store.changed_grant(&y), None, "expired");
    forge(&root, "grants", x.id(), SystemTime::now());
    assert_eq!(store.changed_grant(&y), None, "a grant without a call");
    g["at_ms"] = json!(ms(SystemTime::now()));
    g["v"] = json!(1);
    std::fs::write(&path, g.to_string()).unwrap();
    assert_eq!(store.changed_grant(&y), None, "a v1 grant");
}

#[test]
fn changed_grant_without_a_grants_folder_is_none() {
    let (d, store) = store();
    let x = delete_at("a.db");
    store.request(&x).unwrap();
    store.decide(x.id(), Decision::Approve).unwrap();
    std::fs::remove_dir_all(d.path().join("approvals/grants")).unwrap();
    assert_eq!(store.changed_grant(&delete_at("b.db")), None);
}

#[test]
fn of_two_live_changed_grants_the_newest_is_named() {
    let (_d, store) = store();
    let (a, b) = (delete_at("a.db"), delete_at("b.db"));
    for r in [&a, &b] {
        store.request(r).unwrap();
        store.decide(r.id(), Decision::Approve).unwrap();
    }
    let mut g = grant_json(&store, a.id());
    g["at_ms"] = json!(ms(SystemTime::now() - Duration::from_secs(60)));
    std::fs::write(
        store.dir().join("grants").join(format!("{}.json", a.id())),
        g.to_string(),
    )
    .unwrap();
    assert_eq!(
        store.changed_grant(&delete_at("c.db")).as_deref(),
        Some(b.id())
    );
}
