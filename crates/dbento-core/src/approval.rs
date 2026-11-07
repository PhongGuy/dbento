//! The approval inbox: one-shot grants for a write the MCP server may not run on its own.
//!
//! The server files `inbox/<id>.json`, the app answers with `grants/<id>.json` or
//! `denied/<id>.json`, and the server claims the answer by renaming it away, so each answer
//! is used once; a claimed grant leaves a `used/<id>.json` marker. The first answer wins and
//! leaves `decided/<id>.json`, so a window that lost the race can show what won. The id is a
//! SHA-256 over workspace, connection and exact SQL, so an answer covers only the call that
//! was shown. Every write is a temp file renamed into place.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::session::{Unguarded, Writes};

const VERSION: u32 = 1;
/// A grant or denial, and any stray file, is discarded after this long.
const DECISION_TTL: Duration = Duration::from_secs(3600);
/// An unanswered request is discarded after this long.
const REQUEST_TTL: Duration = Duration::from_secs(24 * 3600);
/// The longest deny reason kept, in characters; a longer one is cut.
pub const MAX_DENY_REASON_CHARS: usize = 500;
/// How far ahead of this clock a file's time may lie before it reads as expired, so a skewed
/// or hand-edited time cannot keep a grant or request alive forever.
const MAX_CLOCK_SKEW: Duration = Duration::from_secs(5 * 60);

const INBOX: &str = "inbox";
const GRANTS: &str = "grants";
const DENIED: &str = "denied";
/// Markers of grants a call has claimed, so the app can tell "ran" from "expired".
const USED: &str = "used";
/// The winning answer to each request, kept after the claim for any window that lost.
const DECIDED: &str = "decided";

/// Lowercase hex SHA-256 of the compact JSON `{"v":1,"workspace":..,"connection":..,"sql":..}`.
pub fn approval_id(workspace: &Path, connection: &str, sql: &str) -> String {
    // A struct, not a map, so the key order is fixed by declaration.
    #[derive(Serialize)]
    struct Binding<'a> {
        v: u32,
        workspace: &'a str,
        connection: &'a str,
        sql: &'a str,
    }
    let workspace = workspace.to_string_lossy();
    let json = serde_json::to_string(&Binding {
        v: VERSION,
        workspace: &workspace,
        connection,
        sql,
    })
    .expect("a struct of strings serializes");
    Sha256::digest(json.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A write the MCP server asks the user to approve.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    id: String,
    at: SystemTime,
    workspace: PathBuf,
    connection: String,
    sql: String,
    writes: Writes,
    unguarded: Vec<Unguarded>,
}

impl ApprovalRequest {
    /// A request filed now; `id` is `approval_id(workspace, connection, sql)`.
    pub fn new(
        workspace: &Path,
        connection: &str,
        sql: &str,
        writes: Writes,
        unguarded: Vec<Unguarded>,
    ) -> Self {
        ApprovalRequest {
            id: approval_id(workspace, connection, sql),
            at: to_time(millis(SystemTime::now())),
            workspace: workspace.to_path_buf(),
            connection: connection.to_owned(),
            sql: sql.to_owned(),
            writes,
            unguarded,
        }
    }

    /// The same request filed at `at` (millisecond precision).
    pub fn with_time(mut self, at: SystemTime) -> Self {
        self.at = to_time(millis(at));
        self
    }

    /// The id binding workspace, connection and SQL.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// When the request was filed.
    pub fn time(&self) -> SystemTime {
        self.at
    }

    /// The workspace folder the call came from.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// The connection's name in that workspace.
    pub fn connection(&self) -> &str {
        &self.connection
    }

    /// The exact statement a grant would run.
    pub fn sql(&self) -> &str {
        &self.sql
    }

    /// Whether the statement writes, as classified.
    pub fn writes(&self) -> Writes {
        self.writes
    }

    /// The data changes that touch every row of their table.
    pub fn unguarded(&self) -> &[Unguarded] {
        &self.unguarded
    }
}

/// The user's answer to a request.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Run the call once.
    Approve,
    /// Refuse the call once.
    Deny,
}

/// What `claim` found for an id.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    /// The user approved the call; it may run once.
    Granted,
    /// The user refused the call.
    Denied,
    /// No answer yet.
    None,
}

/// What became of an approval grant, as the app reports it.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantState {
    /// The grant is live and waits for the AI to retry the call.
    Waiting,
    /// The call claimed the grant and ran.
    Ran,
    /// No live grant or claim marker: denied, stale, unknown or never approved.
    Expired,
}

/// The stored answer to a request, as `verdict` reads it.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Someone approved the call.
    Approved,
    /// Someone refused the call.
    Denied,
    /// No answer to this request can be read: undecided, expired, or not yet written.
    Unknown,
}

/// A failure of the approval store.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ApprovalError {
    /// The folder or a file in it could not be created, written or renamed.
    #[error("approvals folder {}: {message}", path.display())]
    Unavailable {
        /// The folder or file that failed.
        path: PathBuf,
        /// The operating system's message.
        message: String,
    },
    /// No request with this id waits for a decision.
    #[error("no pending approval request {id}")]
    NotPending {
        /// The id asked for.
        id: String,
    },
    /// The id is not 64 lowercase hex chars.
    #[error("not an approval id: {id:?}")]
    InvalidId {
        /// The text given as an id.
        id: String,
    },
}

/// The approvals folder shared by the MCP server and the app.
#[derive(Debug, Clone)]
pub struct ApprovalStore {
    dir: PathBuf,
}

impl ApprovalStore {
    /// Creates `dir`, `inbox/`, `grants/`, `denied/`, `used/`, `decided/` as needed and sets
    /// each to 0700.
    pub fn open(dir: &Path) -> Result<ApprovalStore, ApprovalError> {
        if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| unavailable(parent, e))?;
        }
        for folder in [
            dir.to_path_buf(),
            dir.join(INBOX),
            dir.join(GRANTS),
            dir.join(DENIED),
            dir.join(USED),
            dir.join(DECIDED),
        ] {
            private_dir(&folder).map_err(|e| unavailable(&folder, e))?;
        }
        Ok(ApprovalStore {
            dir: dir.to_path_buf(),
        })
    }

    /// The approvals folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Writes `inbox/<id>.json` atomically. A live request for the id is kept as is; an
    /// expired or unreadable one is replaced, so a retry is never pruned for its old time.
    pub fn request(&self, request: &ApprovalRequest) -> Result<(), ApprovalError> {
        let raw = RawRequest::from(request);
        let target = self.file(INBOX, &request.id);
        match self.write(INBOX, &raw, &target, false) {
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                let live = read_request(&target, &request.id)
                    .is_some_and(|r| !expired(SystemTime::now(), r.at, REQUEST_TTL));
                if live {
                    return Ok(());
                }
                self.write(INBOX, &raw, &target, true)
                    .map_err(|e| unavailable(&target, e))
            }
            r => r.map_err(|e| unavailable(&target, e)),
        }
    }

    /// Takes a denial, else a grant, by renaming it away; expired ones are deleted, not used.
    pub fn claim(&self, id: &str) -> Result<Claim, ApprovalError> {
        self.claim_with_reason(id).map(|(claim, _)| claim)
    }

    /// As `claim`, with the reason the user gave for a denial, if any.
    pub fn claim_with_reason(&self, id: &str) -> Result<(Claim, Option<String>), ApprovalError> {
        check_id(id)?;
        if let Some(denial) = self.take(DENIED, id)? {
            // A denial wins durably: a grant racing beside it must not serve a later call.
            remove_quietly(&self.file(GRANTS, id));
            return Ok((Claim::Denied, denial.reason));
        }
        if self.take(GRANTS, id)?.is_some() {
            // Bookkeeping only: a failed marker must never block a write the user approved.
            let _ = self.write(USED, &RawDecision::now(id), &self.file(USED, id), true);
            return Ok((Claim::Granted, None));
        }
        Ok((Claim::None, None))
    }

    /// Waiting for a live `grants/<id>.json`, Ran for a live `used/<id>.json`, else Expired;
    /// a malformed id reads Expired without touching the disk. Never fails.
    pub fn grant_state(&self, id: &str) -> GrantState {
        if !valid_id(id) {
            return GrantState::Expired;
        }
        let now = SystemTime::now();
        let live = |sub| {
            read_decision(&self.file(sub, id), id).is_some_and(|at| !expired(now, at, DECISION_TTL))
        };
        if live(GRANTS) {
            GrantState::Waiting
        } else if live(USED) {
            GrantState::Ran
        } else {
            GrantState::Expired
        }
    }

    /// Pending requests, oldest first (ties by id). Prunes expired and invalid requests,
    /// expired decisions, and stray files older than an hour; it never fails.
    pub fn pending(&self) -> Vec<ApprovalRequest> {
        let now = SystemTime::now();
        for sub in [GRANTS, DENIED, USED] {
            self.prune(sub, now, |path, id| {
                read_decision(path, id).is_some_and(|at| !expired(now, at, DECISION_TTL))
            });
        }
        self.prune(DECIDED, now, |path, id| {
            read_verdict(path, id).is_some_and(|(at, _)| !expired(now, at, DECISION_TTL))
        });
        let mut out = Vec::new();
        self.prune(INBOX, now, |path, id| match read_request(path, id) {
            Some(r) if !expired(now, r.at, REQUEST_TTL) => {
                out.push(r);
                true
            }
            _ => false,
        });
        out.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.id.cmp(&b.id)));
        out
    }

    /// Writes `grants/<id>.json` or `denied/<id>.json` and records the verdict. Of several
    /// decides on one request only the first succeeds; the rest fail with `NotPending`.
    pub fn decide(&self, id: &str, decision: Decision) -> Result<(), ApprovalError> {
        self.decide_with_reason(id, decision, None)
    }

    /// As `decide`; a denial also keeps `reason`, trimmed and cut to MAX_DENY_REASON_CHARS,
    /// for the claim to hand back. A blank reason, or any reason on an approval, is dropped.
    pub fn decide_with_reason(
        &self,
        id: &str,
        decision: Decision,
        reason: Option<&str>,
    ) -> Result<(), ApprovalError> {
        check_id(id)?;
        let (sub, opposite, kind) = match decision {
            Decision::Approve => (GRANTS, DENIED, RawVerdictKind::Approved),
            Decision::Deny => (DENIED, GRANTS, RawVerdictKind::Denied),
        };
        let target = self.file(sub, id);
        let record = self.file(DECIDED, id);
        // Both files are staged before the race so the winner publishes them right after
        // winning; renaming the request away is the race, and exactly one renamer wins it.
        let mut raw_answer = RawDecision::now(id);
        if decision == Decision::Deny {
            raw_answer.reason = reason
                .map(str::trim)
                .filter(|r| !r.is_empty())
                .map(|r| r.chars().take(MAX_DENY_REASON_CHARS).collect());
        }
        let answer = self
            .staged(sub, &raw_answer)
            .map_err(|e| unavailable(&target, e))?;
        let raw = RawVerdict {
            v: VERSION,
            id: id.to_owned(),
            at_ms: millis(SystemTime::now()),
            verdict: kind,
        };
        let verdict = self
            .staged(DECIDED, &raw)
            .map_err(|e| unavailable(&record, e))?;
        let request = self.file(INBOX, id);
        let taken = self.unique(INBOX, id, "decide");
        match std::fs::rename(&request, &taken) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(ApprovalError::NotPending { id: id.to_owned() });
            }
            Err(e) => return Err(unavailable(&request, e)),
        }
        // Only a file that reads as this id's request is one the user can have been shown.
        if read_request(&taken, id).is_none() {
            remove_quietly(&taken);
            return Err(ApprovalError::NotPending { id: id.to_owned() });
        }
        // Bookkeeping only: without it a losing window says "answered" without saying how.
        let recorded = verdict.persist(&record).is_ok();
        // A fresh answer for a call that ran before must not read as the old Ran. The old
        // marker goes first so a retry claiming a new grant keeps its own fresh marker.
        remove_quietly(&self.file(USED, id));
        if let Err(e) = answer.persist(&target) {
            // Nothing was answered: the request goes back and the verdict must not claim one.
            if recorded {
                remove_quietly(&record);
            }
            let _ = std::fs::rename(&taken, &request);
            return Err(unavailable(&target, e.error));
        }
        remove_quietly(&taken);
        remove_quietly(&self.file(opposite, id));
        Ok(())
    }

    /// The winning answer to `request`: a verdict recorded no earlier than the request was
    /// filed and younger than an hour, else Unknown. Never fails.
    pub fn verdict(&self, request: &ApprovalRequest) -> Verdict {
        let now = SystemTime::now();
        match read_verdict(&self.file(DECIDED, &request.id), &request.id) {
            Some((at, v))
                if millis(at) >= millis(request.at) && !expired(now, at, DECISION_TTL) =>
            {
                v
            }
            _ => Verdict::Unknown,
        }
    }

    fn file(&self, sub: &str, id: &str) -> PathBuf {
        self.dir.join(sub).join(format!("{id}.json"))
    }

    /// Writes `value` to a temp file in `sub` and renames it to `target`. Without `replace`,
    /// an existing target fails with `AlreadyExists`. Temp files are created 0600.
    fn write(
        &self,
        sub: &str,
        value: &impl Serialize,
        target: &Path,
        replace: bool,
    ) -> std::io::Result<()> {
        let tmp = self.staged(sub, value)?;
        if replace {
            tmp.persist(target).map_err(|e| e.error)?;
        } else {
            tmp.persist_noclobber(target).map_err(|e| e.error)?;
        }
        Ok(())
    }

    /// `value` written and synced to a 0600 temp file in `sub`, ready to rename into place.
    fn staged(
        &self,
        sub: &str,
        value: &impl Serialize,
    ) -> std::io::Result<tempfile::NamedTempFile> {
        let mut tmp = tempfile::NamedTempFile::new_in(self.dir.join(sub))?;
        serde_json::to_writer(&mut tmp, value)?;
        tmp.flush()?;
        tmp.as_file().sync_all()?;
        Ok(tmp)
    }

    /// A name in `sub` unique to this call, for taking `<id>.json` by renaming it there.
    fn unique(&self, sub: &str, id: &str, what: &str) -> PathBuf {
        static TAKES: AtomicU64 = AtomicU64::new(0);
        let n = TAKES.fetch_add(1, Ordering::Relaxed);
        self.dir
            .join(sub)
            .join(format!("{id}.json.{}.{n}.{what}", std::process::id()))
    }

    /// Renames `sub/<id>.json` to a name unique to this call, then reads and deletes it.
    /// Some only for a valid decision younger than an hour; the rename makes the take atomic.
    fn take(&self, sub: &str, id: &str) -> Result<Option<RawDecision>, ApprovalError> {
        let from = self.file(sub, id);
        let to = self.unique(sub, id, "claim");
        match std::fs::rename(&from, &to) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(unavailable(&from, e)),
        }
        let raw = read_raw_decision(&to, id);
        remove_quietly(&to);
        Ok(raw.filter(|r| !expired(SystemTime::now(), to_time(r.at_ms), DECISION_TTL)))
    }

    /// Visits each `<id>.json` in `sub`, deleting it unless `keep` says otherwise; any other
    /// file is deleted once its mtime is older than an hour.
    fn prune(&self, sub: &str, now: SystemTime, mut keep: impl FnMut(&Path, &str) -> bool) {
        let Ok(entries) = std::fs::read_dir(self.dir.join(sub)) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let id = name
                .to_str()
                .and_then(|n| n.strip_suffix(".json"))
                .filter(|id| valid_id(id));
            let drop = match id {
                Some(id) => !keep(&path, id),
                None => entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .is_ok_and(|m| expired(now, m, DECISION_TTL)),
            };
            if drop {
                remove_quietly(&path);
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
struct RawRequest {
    v: u32,
    id: String,
    at_ms: u64,
    workspace: String,
    connection: String,
    sql: String,
    writes: RawWrites,
    unguarded: Vec<RawUnguarded>,
}

#[derive(Serialize, Deserialize)]
struct RawDecision {
    v: u32,
    id: String,
    at_ms: u64,
    /// Why the user denied the call; absent on grants and on files from before reasons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

impl RawDecision {
    fn now(id: &str) -> RawDecision {
        RawDecision {
            v: VERSION,
            id: id.to_owned(),
            at_ms: millis(SystemTime::now()),
            reason: None,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct RawVerdict {
    v: u32,
    id: String,
    at_ms: u64,
    verdict: RawVerdictKind,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawVerdictKind {
    Approved,
    Denied,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawWrites {
    Yes,
    No,
    Unknown,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawUnguarded {
    UpdateWithoutWhere,
    DeleteWithoutWhere,
    Drop,
    Truncate,
    UpdateAllDocuments,
    DeleteAllDocuments,
    FlushKeys,
    /// A kind written by a newer version; skipped on read so the file still loads.
    #[serde(other)]
    Unknown,
}

impl From<&ApprovalRequest> for RawRequest {
    fn from(r: &ApprovalRequest) -> RawRequest {
        RawRequest {
            v: VERSION,
            id: r.id.clone(),
            at_ms: millis(r.at),
            workspace: r.workspace.to_string_lossy().into_owned(),
            connection: r.connection.clone(),
            sql: r.sql.clone(),
            writes: match r.writes {
                Writes::Yes => RawWrites::Yes,
                Writes::No => RawWrites::No,
                Writes::Unknown => RawWrites::Unknown,
            },
            unguarded: r
                .unguarded
                .iter()
                .map(|u| match u {
                    Unguarded::UpdateWithoutWhere => RawUnguarded::UpdateWithoutWhere,
                    Unguarded::DeleteWithoutWhere => RawUnguarded::DeleteWithoutWhere,
                    Unguarded::Drop => RawUnguarded::Drop,
                    Unguarded::Truncate => RawUnguarded::Truncate,
                    Unguarded::UpdateAllDocuments => RawUnguarded::UpdateAllDocuments,
                    Unguarded::DeleteAllDocuments => RawUnguarded::DeleteAllDocuments,
                    Unguarded::FlushKeys => RawUnguarded::FlushKeys,
                })
                .collect(),
        }
    }
}

/// Reads a request file, accepting it only when its name, its `id` field and the id
/// recomputed from its content agree, so what is shown is exactly what a grant covers.
fn read_request(path: &Path, id: &str) -> Option<ApprovalRequest> {
    let raw: RawRequest = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let workspace = PathBuf::from(raw.workspace);
    if raw.v != VERSION || raw.id != id || approval_id(&workspace, &raw.connection, &raw.sql) != id
    {
        return None;
    }
    Some(ApprovalRequest {
        id: raw.id,
        at: to_time(raw.at_ms),
        workspace,
        connection: raw.connection,
        sql: raw.sql,
        writes: match raw.writes {
            RawWrites::Yes => Writes::Yes,
            RawWrites::No => Writes::No,
            RawWrites::Unknown => Writes::Unknown,
        },
        unguarded: raw
            .unguarded
            .into_iter()
            .filter_map(|u| match u {
                RawUnguarded::UpdateWithoutWhere => Some(Unguarded::UpdateWithoutWhere),
                RawUnguarded::DeleteWithoutWhere => Some(Unguarded::DeleteWithoutWhere),
                RawUnguarded::Drop => Some(Unguarded::Drop),
                RawUnguarded::Truncate => Some(Unguarded::Truncate),
                RawUnguarded::UpdateAllDocuments => Some(Unguarded::UpdateAllDocuments),
                RawUnguarded::DeleteAllDocuments => Some(Unguarded::DeleteAllDocuments),
                RawUnguarded::FlushKeys => Some(Unguarded::FlushKeys),
                RawUnguarded::Unknown => None,
            })
            .collect(),
    })
}

/// The time of a valid grant or denial file for `id`.
fn read_decision(path: &Path, id: &str) -> Option<SystemTime> {
    read_raw_decision(path, id).map(|raw| to_time(raw.at_ms))
}

/// A valid grant or denial file for `id`, with any reason it carries.
fn read_raw_decision(path: &Path, id: &str) -> Option<RawDecision> {
    let raw: RawDecision = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (raw.v == VERSION && raw.id == id).then_some(raw)
}

/// The time and answer of a valid `decided/<id>.json`.
fn read_verdict(path: &Path, id: &str) -> Option<(SystemTime, Verdict)> {
    let raw: RawVerdict = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let verdict = match raw.verdict {
        RawVerdictKind::Approved => Verdict::Approved,
        RawVerdictKind::Denied => Verdict::Denied,
    };
    (raw.v == VERSION && raw.id == id).then(|| (to_time(raw.at_ms), verdict))
}

fn valid_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Refuses anything but 64 lowercase hex chars, so no id can name another path.
fn check_id(id: &str) -> Result<(), ApprovalError> {
    if valid_id(id) {
        Ok(())
    } else {
        Err(ApprovalError::InvalidId { id: id.to_owned() })
    }
}

/// True when `at` lies more than `ttl` before `now`, or more than MAX_CLOCK_SKEW after it.
fn expired(now: SystemTime, at: SystemTime, ttl: Duration) -> bool {
    match now.duration_since(at) {
        Ok(age) => age > ttl,
        Err(ahead) => ahead.duration() > MAX_CLOCK_SKEW,
    }
}

fn millis(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn to_time(ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms)
}

fn remove_quietly(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn unavailable(path: &Path, e: impl std::fmt::Display) -> ApprovalError {
    ApprovalError::Unavailable {
        path: path.to_path_buf(),
        message: e.to_string(),
    }
}

/// Creates `dir` if missing and sets it to 0700 even when it already existed looser.
#[cfg(unix)]
fn private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Err(e) if e.kind() != ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}
