//! The sync state machine: given what happened, it decides when the next cycle or poll runs.
//! It is pure; the caller passes the time in and carries out the steps.

use std::time::{Duration, Instant, SystemTime};

use super::Role;
use crate::git::{ConflictFile, Refusal};

/// Where a bound folder's sync stands.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SyncState {
    /// Local saves are on the server and the server's commits are local.
    Synced,
    /// Saving local changes as a commit.
    Committing,
    /// Bringing the server's commits in.
    Pulling,
    /// Sending local commits to the server.
    Pushing,
    /// The server cannot be reached; saves stay local.
    Offline,
    /// The account may only read; the folder follows the server.
    ReadOnly,
    /// The server refused the token; saves are committed locally until the next sign-in.
    SignedOut,
    /// No usable git was found.
    GitMissing,
    /// The repository was locked or the server kept moving; the next trigger retries.
    Busy,
    /// The server refused the push; the saves stay local until they change.
    Refused(Refusal),
    /// The server's side won these paths; the local sides are kept for review.
    Conflicted(Vec<ConflictFile>),
    /// The cycle failed for this reason; the next trigger retries.
    Failed(String),
}

/// What a bound folder's sync reports to its viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SyncStatus {
    /// Where the sync stands.
    pub state: SyncState,
    /// Local commits not yet on the server.
    pub unpushed: u32,
    /// When the folder last matched the server.
    pub last_synced: Option<SystemTime>,
    /// Why a read-only folder is not following the server, when it is not.
    pub note: Option<String>,
}

/// The machine's intervals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Timing {
    /// How long the folder must stay quiet after a change before a cycle runs.
    pub(crate) debounce: Duration,
    /// How often the server is polled when no notice says it moved.
    pub(crate) poll_every: Duration,
    /// How long an open notice stream may stay silent before polls resume.
    pub(crate) silence: Duration,
}

/// Something the machine is told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Input {
    /// A file in the folder changed.
    Changed,
    /// The viewer asked to sync now.
    Flush,
    /// The viewer asked to retry whatever held the sync.
    Retry,
    /// The server's branch moved.
    RemoteMoved,
    /// The notice stream opened.
    StreamUp,
    /// The notice stream showed it is still open.
    StreamAlive,
    /// The notice stream closed.
    StreamDown,
    /// The notice stream was refused the token.
    SignedOut,
    /// The account's role changed.
    Role(Role),
    /// Conflicts were resolved; these are left.
    Settled(Vec<ConflictFile>),
    /// The running cycle or poll ended.
    Ended(Ended),
}

/// How a cycle or poll ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ended {
    Synced,
    ReadOnly,
    Conflicted(Vec<ConflictFile>),
    Refused(Refusal),
    Busy,
    Offline,
    SignedOut,
    GitMissing,
    Failed(String),
    /// A poll found the server's branch where it was.
    PollSame,
}

/// Which steps a cycle runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cycle {
    /// Commit, pull and push.
    Sync,
    /// Follow the server and never commit.
    PullOnly,
    /// Commit locally and talk to no server.
    CommitOnly,
}

/// What the caller should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Run(Cycle),
    Poll,
    /// Nothing until this time, or until the next input when `None`.
    Wait(Option<Instant>),
}

/// What stops cycles from running on their own after an end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hold {
    None,
    Refused,
    SignedOut,
    GitMissing,
    Failed,
}

/// Which trigger arrived, since holds release on different ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trigger {
    Change,
    Retry,
    Remote,
}

/// What is running now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Running {
    Idle,
    Cycle,
    Poll,
}

/// The sync state machine for one bound folder.
#[derive(Debug)]
pub(crate) struct Machine {
    role: Role,
    timing: Timing,
    state: SyncState,
    hold: Hold,
    running: Running,
    /// The folder has changes no cycle has committed yet.
    dirty: bool,
    /// The folder changed after the running cycle started, so its end leaves it dirty.
    dirtied_while_running: bool,
    /// A cycle runs as soon as the machine is asked.
    due_now: bool,
    /// A cycle runs at this time, once the folder has been quiet long enough.
    debounce_at: Option<Instant>,
    stream_up: bool,
    last_alive: Instant,
    next_poll: Instant,
}

impl Machine {
    /// A machine in `Pulling` with its first cycle due at once.
    pub(crate) fn new(role: Role, timing: Timing, now: Instant) -> Machine {
        Machine {
            role,
            timing,
            state: SyncState::Pulling,
            hold: Hold::None,
            running: Running::Idle,
            dirty: false,
            dirtied_while_running: false,
            due_now: true,
            debounce_at: None,
            stream_up: false,
            last_alive: now,
            next_poll: now + timing.poll_every,
        }
    }

    /// Where the sync stands.
    pub(crate) fn state(&self) -> &SyncState {
        &self.state
    }

    /// Take in one input at `now`.
    pub(crate) fn on(&mut self, input: Input, now: Instant) {
        match input {
            Input::Changed => {
                self.mark_dirty();
                if self.trigger(Trigger::Change) {
                    self.debounce_at = Some(now + self.timing.debounce);
                }
            }
            Input::Flush => {
                self.mark_dirty();
                if self.trigger(Trigger::Change) {
                    self.due_now = true;
                }
            }
            Input::Retry => {
                if self.trigger(Trigger::Retry) {
                    self.due_now = true;
                }
            }
            Input::RemoteMoved => {
                if self.trigger(Trigger::Remote) {
                    self.due_now = true;
                }
            }
            Input::StreamUp => {
                self.stream_up = true;
                self.last_alive = now;
            }
            Input::StreamAlive => self.last_alive = now,
            Input::StreamDown => self.stream_up = false,
            Input::SignedOut => {
                self.hold = Hold::SignedOut;
                self.state = SyncState::SignedOut;
            }
            Input::Role(role) => self.set_role(role),
            Input::Settled(rest) => {
                if matches!(self.state, SyncState::Conflicted(_)) {
                    self.state = if rest.is_empty() {
                        self.settled_state()
                    } else {
                        SyncState::Conflicted(rest)
                    };
                }
            }
            Input::Ended(ended) => self.end(ended),
        }
    }

    /// The next step at `now`. A returned `Run` or `Poll` is marked running until its `Ended`.
    pub(crate) fn next(&mut self, now: Instant) -> Action {
        if self.running != Running::Idle {
            return Action::Wait(None);
        }
        let cycle = self.allowed_cycle();
        if cycle.is_none() {
            self.due_now = false;
            self.debounce_at = None;
        }
        let poll_at = self.poll_at();
        if let Some(cycle) = cycle
            && (self.due_now || self.debounce_at.is_some_and(|at| at <= now))
        {
            self.due_now = false;
            self.debounce_at = None;
            self.running = Running::Cycle;
            self.dirtied_while_running = false;
            return Action::Run(cycle);
        }
        if let Some(at) = poll_at
            && at <= now
        {
            self.next_poll = now + self.timing.poll_every;
            self.running = Running::Poll;
            return Action::Poll;
        }
        let wake = match (self.debounce_at, poll_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        Action::Wait(wake)
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
        if self.running == Running::Cycle {
            self.dirtied_while_running = true;
        }
    }

    /// Whether a trigger of this kind schedules a cycle; one that a hold drops does not.
    fn trigger(&mut self, trigger: Trigger) -> bool {
        let releases = match self.hold {
            Hold::None | Hold::Failed => true,
            Hold::Refused => trigger != Trigger::Remote,
            Hold::GitMissing => trigger == Trigger::Retry,
            // Signed out, local changes still commit; only a retry goes back to the server.
            Hold::SignedOut if trigger == Trigger::Change => return true,
            Hold::SignedOut => trigger == Trigger::Retry,
        };
        if releases {
            self.hold = Hold::None;
        }
        releases
    }

    /// The cycle the current hold and role allow, if any.
    fn allowed_cycle(&self) -> Option<Cycle> {
        match (self.hold, self.role) {
            (Hold::None, Role::Read) => Some(Cycle::PullOnly),
            (Hold::None, Role::Write) => Some(Cycle::Sync),
            (Hold::SignedOut, Role::Write) if self.dirty => Some(Cycle::CommitOnly),
            _ => None,
        }
    }

    /// When the next poll is due, or `None` while polls are stopped.
    fn poll_at(&self) -> Option<Instant> {
        match self.hold {
            Hold::SignedOut | Hold::GitMissing => None,
            Hold::None | Hold::Refused | Hold::Failed if self.stream_up => {
                Some(self.next_poll.max(self.last_alive + self.timing.silence))
            }
            Hold::None | Hold::Refused | Hold::Failed => Some(self.next_poll),
        }
    }

    fn set_role(&mut self, role: Role) {
        let was = self.role;
        self.role = role;
        match role {
            Role::Read => {
                if self.state == SyncState::Synced {
                    self.state = SyncState::ReadOnly;
                }
            }
            Role::Write => {
                let promoted = was == Role::Read && self.state == SyncState::ReadOnly;
                if promoted && self.trigger(Trigger::Remote) {
                    self.due_now = true;
                }
            }
        }
    }

    /// The state a folder with nothing outstanding is in.
    fn settled_state(&self) -> SyncState {
        match self.role {
            Role::Read => SyncState::ReadOnly,
            Role::Write => SyncState::Synced,
        }
    }

    fn end(&mut self, ended: Ended) {
        let was = std::mem::replace(&mut self.running, Running::Idle);
        let keeps_dirty = matches!(
            ended,
            Ended::Busy | Ended::Offline | Ended::GitMissing | Ended::Failed(_) | Ended::PollSame
        );
        if was == Running::Cycle && !keeps_dirty {
            self.dirty = self.dirtied_while_running;
        }
        let (state, hold) = match ended {
            Ended::PollSame => {
                if matches!(self.state, SyncState::Busy | SyncState::Offline) {
                    self.due_now = true;
                }
                return;
            }
            Ended::Synced => (SyncState::Synced, Hold::None),
            Ended::ReadOnly => (SyncState::ReadOnly, Hold::None),
            Ended::Conflicted(files) => (SyncState::Conflicted(files), Hold::None),
            Ended::Busy => (SyncState::Busy, Hold::None),
            Ended::Offline => (SyncState::Offline, Hold::None),
            Ended::SignedOut => (SyncState::SignedOut, Hold::SignedOut),
            Ended::Refused(refusal) => (SyncState::Refused(refusal), Hold::Refused),
            Ended::GitMissing => (SyncState::GitMissing, Hold::GitMissing),
            Ended::Failed(why) => (SyncState::Failed(why), Hold::Failed),
        };
        // A hold drops what was scheduled before it; only a releasing trigger re-arms a cycle.
        if matches!(hold, Hold::Refused | Hold::GitMissing | Hold::Failed) {
            self.due_now = false;
            self.debounce_at = None;
        }
        self.state = state;
        self.hold = hold;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{ConflictKind, parse_refusal};

    fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }
    fn timing() -> Timing {
        Timing {
            debounce: s(4),
            poll_every: s(60),
            silence: s(45),
        }
    }
    fn conflict() -> Vec<ConflictFile> {
        vec![ConflictFile {
            path: "dbento.toml".into(),
            kind: ConflictKind::BothChanged,
        }]
    }
    /// A machine whose first cycle ran and ended as `end` at `t0`.
    fn after_start(role: Role, end: Ended) -> (Machine, Instant) {
        let t0 = Instant::now();
        let mut m = Machine::new(role, timing(), t0);
        assert!(
            matches!(m.next(t0), Action::Run(_)),
            "a first cycle runs at start"
        );
        m.on(Input::Ended(end), t0);
        (m, t0)
    }

    #[test]
    fn the_first_cycle_runs_at_start_then_the_machine_waits_for_the_poll() {
        let t0 = Instant::now();
        let mut m = Machine::new(Role::Write, timing(), t0);
        assert_eq!(m.state(), &SyncState::Pulling);
        assert_eq!(m.next(t0), Action::Run(Cycle::Sync));
        assert_eq!(m.next(t0), Action::Wait(None), "one cycle at a time");
        m.on(Input::Ended(Ended::Synced), t0);
        assert_eq!(m.state(), &SyncState::Synced);
        assert_eq!(m.next(t0), Action::Wait(Some(t0 + s(60))));
    }

    #[test]
    fn a_change_waits_for_four_quiet_seconds_and_flush_skips_the_wait() {
        let (mut m, t0) = after_start(Role::Write, Ended::Synced);
        m.on(Input::Changed, t0);
        assert_eq!(m.next(t0 + s(3)), Action::Wait(Some(t0 + s(4))));
        m.on(Input::Changed, t0 + s(3));
        assert_eq!(m.next(t0 + s(6)), Action::Wait(Some(t0 + s(7))));
        assert_eq!(m.next(t0 + s(7)), Action::Run(Cycle::Sync));
        m.on(Input::Ended(Ended::Synced), t0 + s(7));
        m.on(Input::Flush, t0 + s(8));
        assert_eq!(m.next(t0 + s(8)), Action::Run(Cycle::Sync));
    }

    #[test]
    fn a_reader_only_pulls_and_a_demotion_makes_a_reader() {
        let (mut m, t0) = after_start(Role::Read, Ended::ReadOnly);
        assert_eq!(m.state(), &SyncState::ReadOnly);
        m.on(Input::Changed, t0);
        assert_eq!(m.next(t0 + s(4)), Action::Run(Cycle::PullOnly));
        let (mut w, t1) = after_start(Role::Write, Ended::Synced);
        w.on(Input::Role(Role::Read), t1);
        assert_eq!(w.state(), &SyncState::ReadOnly);
        w.on(Input::Flush, t1);
        assert_eq!(w.next(t1), Action::Run(Cycle::PullOnly));
    }

    #[test]
    fn a_refusal_holds_until_a_change_or_a_retry() {
        let refusal = parse_refusal(
            "remote: potluck-reject secret_value refs/heads/main dbento.toml password",
        )
        .unwrap();
        let (mut m, t0) = after_start(Role::Write, Ended::Refused(refusal.clone()));
        assert_eq!(m.state(), &SyncState::Refused(refusal.clone()));
        m.on(Input::RemoteMoved, t0);
        assert_eq!(
            m.next(t0),
            Action::Wait(Some(t0 + s(60))),
            "a notice does not retry a refused push"
        );
        m.on(Input::Changed, t0);
        assert_eq!(m.next(t0 + s(4)), Action::Run(Cycle::Sync));
        m.on(Input::Ended(Ended::Refused(refusal)), t0 + s(4));
        m.on(Input::Retry, t0 + s(5));
        assert_eq!(m.next(t0 + s(5)), Action::Run(Cycle::Sync));
    }

    #[test]
    fn signed_out_commits_locally_and_pushes_only_after_a_retry() {
        let (mut m, t0) = after_start(Role::Write, Ended::SignedOut);
        assert_eq!(m.state(), &SyncState::SignedOut);
        m.on(Input::RemoteMoved, t0);
        assert_eq!(
            m.next(t0),
            Action::Wait(None),
            "no pull and no poll while signed out"
        );
        m.on(Input::Changed, t0);
        assert_eq!(m.next(t0 + s(4)), Action::Run(Cycle::CommitOnly));
        m.on(Input::Ended(Ended::SignedOut), t0 + s(4));
        assert_eq!(m.next(t0 + s(500)), Action::Wait(None));
        m.on(Input::Flush, t0 + s(500));
        assert_eq!(
            m.next(t0 + s(500)),
            Action::Run(Cycle::CommitOnly),
            "flush marks the folder dirty"
        );
        m.on(Input::Ended(Ended::SignedOut), t0 + s(500));
        m.on(Input::Retry, t0 + s(501));
        assert_eq!(m.next(t0 + s(501)), Action::Run(Cycle::Sync));
        let (mut n, t1) = after_start(Role::Write, Ended::Synced);
        n.on(Input::SignedOut, t1);
        assert_eq!(n.state(), &SyncState::SignedOut, "the stream signs out too");
        assert_eq!(n.next(t1 + s(120)), Action::Wait(None));
    }

    #[test]
    fn busy_reruns_on_a_notice_and_offline_on_a_poll_that_reaches_the_server() {
        let (mut m, t0) = after_start(Role::Write, Ended::Busy);
        assert_eq!(m.state(), &SyncState::Busy);
        m.on(Input::RemoteMoved, t0);
        assert_eq!(m.next(t0), Action::Run(Cycle::Sync));
        m.on(Input::Ended(Ended::Offline), t0);
        assert_eq!(m.state(), &SyncState::Offline);
        assert_eq!(m.next(t0 + s(60)), Action::Poll);
        m.on(Input::Ended(Ended::PollSame), t0 + s(60));
        assert_eq!(m.next(t0 + s(60)), Action::Run(Cycle::Sync));
    }

    #[test]
    fn polls_run_while_the_stream_is_down_or_silent() {
        let (mut m, t0) = after_start(Role::Write, Ended::Synced);
        assert_eq!(m.next(t0 + s(60)), Action::Poll);
        m.on(Input::Ended(Ended::PollSame), t0 + s(60));
        assert_eq!(m.state(), &SyncState::Synced);
        assert_eq!(m.next(t0 + s(60)), Action::Wait(Some(t0 + s(120))));
        m.on(Input::StreamUp, t0 + s(61));
        m.on(Input::StreamAlive, t0 + s(100));
        assert_eq!(m.next(t0 + s(125)), Action::Wait(Some(t0 + s(145))));
        assert_eq!(m.next(t0 + s(145)), Action::Poll);
    }

    #[test]
    fn git_missing_waits_for_a_retry_and_a_failure_reruns_on_any_trigger() {
        let (mut m, t0) = after_start(Role::Write, Ended::GitMissing);
        m.on(Input::Changed, t0);
        assert_eq!(m.next(t0 + s(10)), Action::Wait(None));
        m.on(Input::Retry, t0 + s(10));
        assert_eq!(m.next(t0 + s(10)), Action::Run(Cycle::Sync));
        let (mut f, t1) = after_start(Role::Write, Ended::Failed("off main".into()));
        assert_eq!(f.state(), &SyncState::Failed("off main".into()));
        f.on(Input::RemoteMoved, t1);
        assert_eq!(f.next(t1), Action::Run(Cycle::Sync));
    }

    #[test]
    fn a_conflict_does_not_hold_and_clears_once_settled() {
        let (mut m, t0) = after_start(Role::Write, Ended::Conflicted(conflict()));
        assert_eq!(m.state(), &SyncState::Conflicted(conflict()));
        m.on(Input::Changed, t0);
        assert_eq!(m.next(t0 + s(4)), Action::Run(Cycle::Sync));
        m.on(Input::Ended(Ended::Conflicted(conflict())), t0 + s(4));
        m.on(Input::Settled(Vec::new()), t0 + s(5));
        assert_eq!(m.state(), &SyncState::Synced);
    }
}
