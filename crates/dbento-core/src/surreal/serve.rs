//! The session's serving task: it runs one script at a time over the socket task, splitting a
//! unit's frames into one result per statement. Drain, cut, park and the fence live here, so
//! they happen whether or not the caller is polling, and the driver only talks to it by channel.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use super::cancel::{Stop, SurrealCancel};
use super::classify::surreal_classification;
use super::decode::Wire;
use super::login::{Missing, Place, pre_check};
use super::rpc::{Frame, Frames, Param, Rpc, RpcError};
use super::shape::Shape;
use super::units::{Unit, UnitKind, UseTarget, units};
use super::wire::{Reading, STRING_CAST, WireError, call_error, field, statement_error};
use crate::session::{
    Classification, ExecuteError, Limits, OnTruncate, Outcome, Prepared, ReadOnlyEnforcement,
    TruncatedBy, Writes,
};
use crate::value::{BatchBuilder, CellRef, ColumnInfo, ResultBatch};

/// Rows that fix a result's columns, unless a limit is reached first.
const SAMPLE_ROWS: usize = 100;
/// How long a parked fenced result may hold its transaction.
pub(crate) const PARK_BOUND: Duration = Duration::from_secs(300);

pub(crate) enum Request {
    Script {
        text: String,
        ctl: mpsc::UnboundedReceiver<Ctl>,
    },
    Settle {
        done: oneshot::Sender<Result<(), ExecuteError>>,
    },
    Close {
        done: oneshot::Sender<Result<(), ExecuteError>>,
    },
}

type NextReply = oneshot::Sender<Option<Result<Prepared, ExecuteError>>>;
type ColumnsReply = oneshot::Sender<Result<Vec<ColumnInfo>, ExecuteError>>;

pub(crate) enum Ctl {
    Next {
        reply: NextReply,
    },
    IsLast {
        reply: oneshot::Sender<bool>,
    },
    Execute {
        reply: ColumnsReply,
        events: mpsc::Sender<Event>,
        limits: Limits,
        resume: Option<mpsc::UnboundedReceiver<Option<u64>>>,
    },
}

pub(crate) enum Event {
    Batch(ResultBatch),
    Done(Outcome),
    Parked(Outcome),
    Failed(ExecuteError),
}

/// The session's rules the task enforces on every unit.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Rules {
    pub(crate) drain_bound: Duration,
    /// The statement deadline, armed on every unit sent.
    pub(crate) timeout: Option<Duration>,
    /// Set when every unit runs fenced, in an RPC transaction that is always cancelled.
    pub(crate) read_only: Option<ReadOnlyEnforcement>,
    /// A VIEWER login made the session read-only.
    pub(crate) viewer: bool,
    pub(crate) park_bound: Duration,
}

impl Rules {
    fn reading(&self) -> Reading {
        Reading {
            read_only: self.read_only.is_some(),
            viewer: self.viewer,
        }
    }
}

/// The session as the task holds it.
struct Served {
    rpc: Rpc,
    rules: Rules,
    cancel: Arc<SurrealCancel>,
    /// Where RPC `use` has put the session, for the next `USE` unit's pre-check.
    place: Place,
    /// Errors no caller has heard, for the next settles or close.
    owed: Owed,
    /// Dbento closed the socket itself, so later calls read as a plain closed session.
    quiet: bool,
    /// The caller has heard why the session ended.
    told: AtomicBool,
}

/// Serves `requests` until the session drops them, then ends the socket, which stops whatever
/// runs. A `CancelHandle` keeps its own `Rpc` clone, so dropping this task's copy is not enough.
pub(crate) async fn serve(
    rpc: Rpc,
    rules: Rules,
    cancel: Arc<SurrealCancel>,
    place: Place,
    mut requests: mpsc::UnboundedReceiver<Request>,
) {
    let mut s = Served {
        rpc,
        rules,
        cancel,
        place,
        owed: Owed::default(),
        quiet: false,
        told: AtomicBool::new(false),
    };
    while let Some(request) = requests.recv().await {
        match request {
            Request::Script { text, mut ctl } => s.script(&text, &mut ctl).await,
            Request::Settle { done } => {
                s.cancel.end();
                let _ = done.send(s.owed.take().map_or(Ok(()), Err));
            }
            Request::Close { done } => {
                s.rpc.close("the session was closed");
                let _ = done.send(s.owed.take().map_or(Ok(()), Err));
                return;
            }
        }
    }
    s.rpc.close("the session was dropped");
}

/// What no caller has heard yet: a statement's error, then the one that closed the session.
/// Each settle or close takes one, and each slot keeps its first.
#[derive(Default)]
struct Owed {
    statement: Option<ExecuteError>,
    closing: Option<ExecuteError>,
}

impl Owed {
    fn keep(&mut self, e: ExecuteError) {
        let slot = if e.closes_session() {
            &mut self.closing
        } else {
            &mut self.statement
        };
        slot.get_or_insert(e);
    }

    fn take(&mut self) -> Option<ExecuteError> {
        self.statement.take().or_else(|| self.closing.take())
    }
}

/// One unit on the wire.
struct Run {
    frames: Frames,
    id: i64,
    text: String,
    /// The statement the next result reads.
    index: u64,
    /// Results this unit yields: the lower of the lexer's count and the server's.
    expected: u64,
    /// The server's count.
    statements: u64,
    writes: Writes,
    fence: Option<[u8; 16]>,
    transaction: bool,
    ended: bool,
    end_error: Option<WireError>,
    /// The first error of a statement only the server counted, which no result reports.
    extra_error: Option<WireError>,
    extra: u32,
}

impl Run {
    fn rolled_back(&self) -> bool {
        self.fence.is_some() && self.writes != Writes::No
    }

    fn outcome(&self, rows: u64, by: Option<TruncatedBy>) -> Outcome {
        Outcome {
            rows,
            truncated: by.is_some(),
            rows_affected: None,
            truncated_by: by,
            extra_results: 0,
            rolled_back: self.rolled_back(),
        }
    }
}

/// Where the caller is in the script.
enum After {
    /// The unit has more statements, or it ended and the next unit may start.
    Next,
    /// The script ends here.
    Stop,
}

/// The caller's end of one result.
struct Out {
    reply: Option<ColumnsReply>,
    events: mpsc::Sender<Event>,
    limits: Limits,
    resume: Option<mpsc::UnboundedReceiver<Option<u64>>>,
}

impl Out {
    fn columns(&mut self, columns: Vec<ColumnInfo>) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(Ok(columns));
        }
    }

    // An error goes to `execute` until the columns have gone, and to the stream after.
    async fn fail(&mut self, e: ExecuteError) {
        match self.reply.take() {
            Some(reply) => drop(reply.send(Err(e))),
            None => drop(self.events.send(Event::Failed(e)).await),
        }
    }

    async fn send(&self, event: Event) -> bool {
        self.events.send(event).await.is_ok()
    }
}

impl Served {
    fn gone(&self) -> Option<ExecuteError> {
        self.rpc.ended().map(|why| self.closed(why))
    }

    fn close_quietly(&mut self, why: &str) {
        self.quiet = true;
        self.rpc.close(why);
    }

    // The first caller hears why the session ended, and any later one that it did.
    fn closed(&self, why: String) -> ExecuteError {
        if self.quiet || self.told.swap(true, Ordering::Relaxed) {
            ExecuteError::SessionClosed
        } else {
            ExecuteError::SessionEnded { reason: why }
        }
    }

    async fn script(&mut self, text: &str, ctl: &mut mpsc::UnboundedReceiver<Ctl>) {
        let units = units(text);
        let count = units.len();
        let mut queue = units.iter().enumerate();
        // The first statement of a unit, waiting for `Execute`, and its stream id.
        let mut handed: Option<(usize, &Unit, i64, Classification)> = None;
        let mut run: Option<Run> = None;
        // The caller holds a later statement of `run`, unread.
        let mut later = false;
        while let Some(msg) = ctl.recv().await {
            match msg {
                Ctl::IsLast { reply } => {
                    let last = !later
                        && handed
                            .as_ref()
                            .is_none_or(|(at, unit, ..)| at + 1 == count && unit.statements == 1);
                    let _ = reply.send(last);
                }
                Ctl::Next { reply } => {
                    if let Some(e) = self.gone() {
                        let _ = reply.send(Some(Err(e)));
                        handed = None;
                        queue = [].iter().enumerate();
                        continue;
                    }
                    if let Some(current) = run.as_mut() {
                        if later {
                            self.skip_statement(current).await;
                            later = false;
                        }
                        // A later statement confirms nothing: the unit's first one did.
                        if current.index < current.expected && !current.ended {
                            let class = surreal_classification(&current.text);
                            later = true;
                            let _ = reply.send(Some(Ok(Prepared {
                                sql: current.text.clone(),
                                classification: Classification {
                                    writes: class.writes,
                                    unguarded: Vec::new(),
                                    transaction_control: false,
                                },
                            })));
                            continue;
                        }
                        if let Some(done) = run.take() {
                            self.finish(done).await;
                        }
                    }
                    handed = queue.next().map(|(at, unit)| {
                        let id = self.rpc.next_id();
                        self.cancel.begin(id);
                        let class = surreal_classification(&text[unit.range.clone()]);
                        (at, unit, id, class)
                    });
                    if handed.is_none() {
                        self.cancel.end();
                    }
                    let _ = reply.send(handed.as_ref().map(|(_, unit, _, class)| {
                        Ok(Prepared {
                            sql: text[unit.range.clone()].to_owned(),
                            classification: class.clone(),
                        })
                    }));
                }
                Ctl::Execute {
                    reply,
                    events,
                    limits,
                    resume,
                } => {
                    let mut out = Out {
                        reply: Some(reply),
                        events,
                        limits,
                        resume,
                    };
                    let after = if later {
                        later = false;
                        match run.as_mut() {
                            Some(current) => self.result(current, &mut out).await,
                            None => After::Stop,
                        }
                    } else if let Some((_, unit, id, class)) = handed.take() {
                        self.first(unit, id, &class, text, &mut run, &mut out).await
                    } else {
                        out.fail(ExecuteError::SessionClosed).await;
                        After::Stop
                    };
                    if let After::Stop = after {
                        if let Some(done) = run.take() {
                            self.finish(done).await;
                        }
                        queue = [].iter().enumerate();
                    } else if run
                        .as_ref()
                        .is_some_and(|r| r.index >= r.expected || r.ended)
                        && let Some(done) = run.take()
                    {
                        self.finish(done).await;
                    }
                }
            }
        }
        // The script was dropped: nobody reads the rest, but the session must be clean.
        if let Some(mut abandoned) = run.take() {
            self.abandon(&mut abandoned).await;
            self.finish(abandoned).await;
        }
        self.cancel.end();
    }

    // A `USE` the unit splitter could not turn into an RPC must not run as text. A read-only
    // session also refuses a sure write, and transaction control, which the fence owns.
    fn refusal(&self, unit: &Unit, class: &Classification) -> Option<ExecuteError> {
        if unit.refused_use {
            return Some(ExecuteError::Unsupported {
                reason: "USE must be a statement of its own with literal names, before any LET \
                         and outside a transaction"
                    .to_owned(),
            });
        }
        self.rules.read_only?;
        let because = if self.rules.viewer {
            "this login is a VIEWER, so the session is read-only"
        } else {
            "the session is read-only"
        };
        if class.writes == Writes::Yes {
            return Some(ExecuteError::ReadOnlyViolation {
                message: format!("{because}; this statement would write"),
            });
        }
        class
            .transaction_control
            .then(|| ExecuteError::TransactionControl {
                reason: format!("{because}; each unit runs in Dbento's own transaction"),
            })
    }

    // Sends the unit and waits for `begin`, which carries the server's statement count.
    async fn first(
        &mut self,
        unit: &Unit,
        id: i64,
        class: &Classification,
        text: &str,
        run: &mut Option<Run>,
        out: &mut Out,
    ) -> After {
        // A stop aimed at this unit between `Next` and `Execute` means it never starts.
        if let Some(stop) = self.cancel.stopped(id) {
            out.fail(stop.error()).await;
            return After::Stop;
        }
        if let Some(refused) = self.refusal(unit, class) {
            out.fail(refused).await;
            return After::Stop;
        }
        if let UnitKind::Use(target) = &unit.kind {
            return match self.apply_use(target).await {
                Ok(()) => {
                    out.columns(Vec::new());
                    let done = Outcome {
                        rows: 0,
                        truncated: false,
                        rows_affected: None,
                        truncated_by: None,
                        extra_results: 0,
                        rolled_back: false,
                    };
                    out.send(Event::Done(done)).await;
                    After::Next
                }
                Err(e) => {
                    out.fail(e).await;
                    After::Stop
                }
            };
        }
        // Each unit gets its own fence, so a parked result holds one for at most its bound.
        let fence = match self.rules.read_only {
            None => None,
            Some(_) => match self.open_fence().await {
                Ok(fence) => Some(fence),
                Err(e) => {
                    out.fail(e).await;
                    return After::Stop;
                }
            },
        };
        let sql = &text[unit.range.clone()];
        if let Some(after) = self.rules.timeout {
            self.cancel.arm(id, after);
        }
        let mut frames = self.rpc.stream(id, sql, fence);
        let opened = loop {
            let frame = tokio::time::timeout(self.rpc.call_bound(), frames.next()).await;
            match frame {
                Ok(Frame::Begin { statements }) => break Ok(statements),
                Ok(Frame::Refused(e)) => {
                    break Err(self.heard(call_error(&e, sql, self.rules.reading())));
                }
                Ok(Frame::Unreadable { message }) => break Err(unreadable(&message)),
                Ok(Frame::Closed(why)) => break Err(self.closed(why)),
                Ok(_) => {}
                Err(_) => break Err(self.closed(self.rpc.unanswered("query_stream"))),
            }
        };
        // A stop sent before the server registered the stream was refused; send it again.
        if opened.is_ok() && self.cancel.stopped(id).is_some() {
            self.rpc.query_cancel(id);
        }
        let statements = match opened {
            Ok(statements) => statements,
            Err(e) => {
                self.cancel.end();
                self.close_fence(fence).await;
                out.fail(e).await;
                return After::Stop;
            }
        };
        let current = run.insert(Run {
            frames,
            id,
            text: sql.to_owned(),
            index: 0,
            expected: statements.min(unit.statements as u64),
            statements,
            writes: class.writes,
            fence,
            transaction: unit.transaction,
            ended: false,
            end_error: None,
            extra_error: None,
            extra: 0,
        });
        self.result(current, out).await
    }

    // A `USE` unit is a pre-check and then RPC `use`, which keeps an absent part as it is; the
    // reply says where the session now is.
    async fn apply_use(&mut self, target: &UseTarget) -> Result<(), ExecuteError> {
        pre_check(&self.rpc, &self.place, target)
            .await
            .map_err(|e| match e {
                Missing::Target(message) => ExecuteError::database("use", message),
                Missing::Server(e) => self.heard(statement_error(&e, self.rules.reading())),
                Missing::Closed(why) => self.closed(why),
            })?;
        let params = [&target.namespace, &target.database]
            .map(|part| part.as_deref().map_or(Param::None, Param::Text));
        let reply = self
            .rpc
            .call("use", &params, None)
            .await
            .map_err(|e| match e {
                RpcError::Server(e) => self.heard(statement_error(&e, self.rules.reading())),
                RpcError::Unreadable(why) => unreadable(&why),
                RpcError::Closed(why) => self.closed(why),
            })?;
        if let Some(Wire::Text(namespace)) = field(&reply, "namespace") {
            self.place.namespace.clone_from(namespace);
        }
        if let Some(Wire::Text(database)) = field(&reply, "database") {
            self.place.database.clone_from(database);
        }
        Ok(())
    }

    async fn open_fence(&mut self) -> Result<[u8; 16], ExecuteError> {
        match self.rpc.call("begin", &[], None).await {
            Ok(Wire::Uuid(fence)) => Ok(fence),
            Ok(other) => Err(ExecuteError::database(
                "begin",
                format!("the server answered begin with {other:?}"),
            )),
            Err(RpcError::Server(e)) => Err(self.heard(statement_error(&e, self.rules.reading()))),
            Err(RpcError::Unreadable(why)) => Err(unreadable(&why)),
            Err(RpcError::Closed(why)) => Err(self.closed(why)),
        }
    }

    // The reply is not needed: a closed socket has already rolled the fence back, and a call
    // past its bound closes the socket.
    async fn close_fence(&mut self, fence: Option<[u8; 16]>) {
        if let Some(fence) = fence {
            let _ = self.rpc.call("cancel", &[Param::Uuid(fence)], None).await;
        }
    }

    // Reads the rest of the unit, counts the statements the lexer did not see, and closes the
    // fence. An error its `end` carried, or one of a statement only the server counted, that no
    // caller heard is owed to the next settle.
    async fn finish(&mut self, mut run: Run) {
        if !run.ended {
            self.read_past(&mut run, None).await;
        }
        self.close_fence(run.fence.take()).await;
        if self.cancel.stopped(run.id) != Some(Stop::Quiet) {
            if let Some(e) = run.end_error.take().filter(|e| !e.undone()) {
                let e = self.end_error(&run, &e);
                self.owed.keep(e);
            } else if let Some(e) = run.extra_error.take() {
                let e = self.unseen(&run, &e);
                self.owed.keep(e);
            }
        }
        self.cancel.end();
    }

    // The next frame, unless a stop went unanswered past the drain bound: then the socket
    // closes, since only that ends a statement the server will not stop.
    async fn frame(&self, run: &mut Run) -> Frame {
        tokio::select! {
            biased;
            frame = run.frames.next() => frame,
            () = self.cancel.past_bound(run.id) => {
                let why = "the server did not stop the statement";
                self.rpc.close(why);
                Frame::Closed(why.to_owned())
            }
        }
    }

    // Reads frames until statement `upto` finishes, or the unit ends when `None`, within the
    // drain bound. Past the bound the socket closes, which stops the statement.
    async fn read_past(&mut self, run: &mut Run, upto: Option<u64>) {
        let drained = tokio::time::timeout(self.rules.drain_bound, async {
            let mut extras = Vec::new();
            loop {
                match self.frame(run).await {
                    Frame::Finished { index, .. } if upto == Some(index) => break,
                    Frame::Finished {
                        index,
                        error: Some(e),
                        ..
                    } if index >= run.expected
                        && run.extra_error.is_none()
                        && !e.undone()
                        && !e.stopped() =>
                    {
                        run.extra_error = Some(e);
                    }
                    Frame::Rows { index, .. } | Frame::Value { index, .. }
                        if index >= run.expected && !extras.contains(&index) =>
                    {
                        extras.push(index);
                    }
                    Frame::End { error } => {
                        run.ended = true;
                        run.end_error = error;
                        break;
                    }
                    Frame::Refused(_) | Frame::Closed(_) => {
                        run.ended = true;
                        break;
                    }
                    _ => {}
                }
            }
            extras.len()
        })
        .await;
        match drained {
            Ok(extras) => run.extra += u32::try_from(extras).unwrap_or(u32::MAX),
            Err(_) => {
                self.close_quietly("the statement was stopped at the drain bound");
                self.owed.keep(ExecuteError::StoppedAtDrainCap);
                run.ended = true;
            }
        }
    }

    // A later statement the caller skipped still runs on the server; only its frames go.
    async fn skip_statement(&mut self, run: &mut Run) {
        let index = run.index;
        self.read_past(run, Some(index)).await;
        run.index += 1;
    }

    // Nobody reads the rest: a read is stopped, since nothing it does is wanted, and a unit that
    // may write runs on, because stopping it would stop the write.
    async fn abandon(&mut self, run: &mut Run) {
        if run.ended {
            return;
        }
        if run.writes == Writes::No {
            self.give_up(run);
        } else {
            self.read_past(run, None).await;
        }
    }

    // Stops the stream and throws away its frames, so nothing unread, such as the reply to the
    // fence's `cancel`, can stall behind them.
    fn give_up(&self, run: &mut Run) {
        self.cancel.stop(run.id, Stop::Quiet);
        run.frames.discard();
        run.ended = true;
    }

    // A statement's or the unit's error. "Stopped" after a stop of ours is that stop; with none it
    // is the server's send timeout, which ends a result left unread too long.
    fn end_error(&self, run: &Run, e: &WireError) -> ExecuteError {
        if e.stopped() {
            return match self.cancel.stopped(run.id) {
                Some(stop) => stop.error(),
                None => ExecuteError::ResultExpired,
            };
        }
        Self::later_note(run, self.heard(statement_error(e, self.rules.reading())))
    }

    // An error that ends the server's session for this login closes the socket, which the
    // server leaves open, and is the one the caller hears.
    fn heard(&self, error: ExecuteError) -> ExecuteError {
        if error.closes_session() {
            self.told.store(true, Ordering::Relaxed);
            self.rpc.close("the login's session on the server expired");
        }
        error
    }

    // The error of a statement the lexer folded into the one before it, which has no result.
    fn unseen(&self, run: &Run, e: &WireError) -> ExecuteError {
        if e.stopped() {
            return self.end_error(run, e);
        }
        let mut error = self.heard(statement_error(e, self.rules.reading()));
        if let ExecuteError::Database { message, .. } = &mut error {
            message
                .push_str("; this failed in a statement Dbento read as part of the one before it");
        }
        error
    }

    // Later statements of a unit run whatever an earlier one did, unless a transaction ends them.
    fn later_note(run: &Run, mut error: ExecuteError) -> ExecuteError {
        let later = run.index + 1 < run.statements && !run.transaction;
        if later && let ExecuteError::Database { message, .. } = &mut error {
            message.push_str(if run.fence.is_some() {
                "; the later statements of this unit ran, then rolled back"
            } else {
                "; the later statements of this unit ran"
            });
        }
        error
    }

    // A statement failed: the rest of the unit drains first, so the note about it holds.
    async fn failed(&mut self, run: &mut Run, out: &mut Out, e: ExecuteError) -> After {
        if !run.ended {
            self.read_past(run, None).await;
        }
        // The caller hears this error for the unit, not the one its `end` repeats.
        run.end_error = None;
        out.fail(e).await;
        After::Stop
    }

    /// Delivers the statement at `run.index` as one result: a sample of its first rows fixes
    /// the columns, then the rest stream through.
    async fn result(&mut self, run: &mut Run, out: &mut Out) -> After {
        let cap = out
            .limits
            .max_rows
            .map_or(SAMPLE_ROWS, |max| SAMPLE_ROWS.min(max as usize + 1));
        let mut rows: VecDeque<Wire> = VecDeque::new();
        let mut single = false;
        let mut sampled = 0u64;
        // `Some` once this statement's end has been read.
        let mut finished: Option<Option<WireError>> = None;
        while finished.is_none()
            && rows.len() < cap
            && out.limits.max_bytes.is_none_or(|max| sampled < max)
        {
            match self.frame(run).await {
                Frame::Rows {
                    index,
                    values,
                    bytes,
                } if index == run.index => {
                    rows.extend(values);
                    sampled += bytes as u64;
                }
                Frame::Value { index, value } if index == run.index => {
                    single = true;
                    if !matches!(value, Wire::None) {
                        rows.push_back(value);
                    }
                }
                Frame::Finished {
                    index,
                    single: one,
                    error,
                } if index == run.index => {
                    single |= one;
                    finished = Some(error);
                }
                Frame::End { error } => {
                    run.ended = true;
                    finished = Some(error);
                }
                Frame::Refused(e) => {
                    run.ended = true;
                    finished = Some(Some(e));
                }
                Frame::Unreadable { message } => {
                    let e = Self::later_note(run, unreadable(&message));
                    return self.failed(run, out, e).await;
                }
                Frame::Closed(why) => {
                    run.ended = true;
                    let e = self.closed(why);
                    out.fail(e).await;
                    return After::Stop;
                }
                _ => {}
            }
        }
        let sample = rows.make_contiguous();
        let (shape, columns) = Shape::of(sample, single, finished.is_some());
        if rows.is_empty()
            && let Some(Some(e)) = &finished
            && !e.undone()
        {
            let e = self.end_error(run, e);
            return self.failed(run, out, e).await;
        }
        out.columns(columns);
        let mut builder = BatchBuilder::new(shape.width());
        let mut delivered = 0u64;
        loop {
            // The limit is read afresh for every row, since `extend` moves it while parked.
            while let Some(row) = rows.front() {
                if let Some(by) = out.limits.cut(delivered, builder.total_bytes()) {
                    match self.cut(run, out, &mut builder, delivered, by).await {
                        Cut::Resume => continue,
                        Cut::Over(after) => return after,
                    }
                }
                let cells = shape.cells(row);
                let refs: Vec<CellRef<'_>> = cells.iter().map(|c| c.as_ref()).collect();
                let closed = builder.push_typed_row(&refs, |i| cells[i].is_number());
                drop(refs);
                drop(cells);
                rows.pop_front();
                delivered += 1;
                if let Some(batch) = closed
                    && !out.send(Event::Batch(batch)).await
                {
                    return self.dropped(run).await;
                }
                if builder.is_full() && !out.send(Event::Batch(builder.finish())).await {
                    return self.dropped(run).await;
                }
            }
            if let Some(end) = finished.take() {
                return self.ended(run, out, builder, delivered, end).await;
            }
            match self.frame(run).await {
                Frame::Rows { index, values, .. } if index == run.index => rows.extend(values),
                Frame::Value { index, value } if index == run.index => rows.push_back(value),
                Frame::Finished { index, error, .. } if index == run.index => {
                    finished = Some(error);
                }
                Frame::End { error } => {
                    run.ended = true;
                    finished = Some(error);
                }
                Frame::Refused(e) => {
                    run.ended = true;
                    finished = Some(Some(e));
                }
                Frame::Unreadable { message } => {
                    if !builder.is_empty() {
                        let _ = out.send(Event::Batch(builder.finish())).await;
                    }
                    let e = Self::later_note(run, unreadable(&message));
                    return self.failed(run, out, e).await;
                }
                Frame::Closed(why) => {
                    run.ended = true;
                    if !builder.is_empty() {
                        let _ = out.send(Event::Batch(builder.finish())).await;
                    }
                    let e = self.closed(why);
                    out.fail(e).await;
                    return After::Stop;
                }
                _ => {}
            }
        }
    }

    // The caller dropped the stream mid-result.
    async fn dropped(&mut self, run: &mut Run) -> After {
        self.abandon(run).await;
        After::Stop
    }

    async fn ended(
        &mut self,
        run: &mut Run,
        out: &mut Out,
        mut builder: BatchBuilder,
        delivered: u64,
        end: Option<WireError>,
    ) -> After {
        if !builder.is_empty() && !out.send(Event::Batch(builder.finish())).await {
            return self.dropped(run).await;
        }
        let mut rolled_back = run.rolled_back();
        match end {
            Some(e) if e.undone() => rolled_back = true,
            Some(e) => {
                let e = self.end_error(run, &e);
                return self.failed(run, out, e).await;
            }
            None => {}
        }
        // The last result waits for `end`, so its outcome can count what followed it.
        if run.index + 1 >= run.expected {
            if !run.ended {
                self.read_past(run, None).await;
            }
            if let Some(e) = run.end_error.take() {
                let e = self.end_error(run, &e);
                out.fail(e).await;
                return After::Stop;
            }
            // Closed before `Done`, so a caller that has its result sees nothing of the unit.
            self.close_fence(run.fence.take()).await;
            self.cancel.end();
        }
        run.index += 1;
        let done = Outcome {
            rows: delivered,
            truncated: false,
            rows_affected: None,
            truncated_by: None,
            extra_results: run.extra,
            rolled_back,
        };
        out.send(Event::Done(done)).await;
        After::Next
    }

    // A result reached its limit. Rows of a read can park; anything else reports `Done` at once
    // and then drains, unless `Close` may end the session because the statement only reads.
    async fn cut(
        &mut self,
        run: &mut Run,
        out: &mut Out,
        builder: &mut BatchBuilder,
        delivered: u64,
        by: TruncatedBy,
    ) -> Cut {
        if !builder.is_empty() && !out.send(Event::Batch(builder.finish())).await {
            return Cut::Over(self.dropped(run).await);
        }
        let reads = run.writes == Writes::No;
        match out.limits.on_truncate {
            OnTruncate::Park if reads && by == TruncatedBy::Rows => {
                if !out
                    .send(Event::Parked(run.outcome(delivered, Some(by))))
                    .await
                {
                    return Cut::Over(self.dropped(run).await);
                }
                self.park(run, out).await
            }
            OnTruncate::Close if reads => {
                out.send(Event::Done(run.outcome(delivered, Some(by))))
                    .await;
                self.close_quietly("the session was closed at a cut result");
                run.ended = true;
                Cut::Over(After::Stop)
            }
            _ => {
                out.send(Event::Done(run.outcome(delivered, Some(by))))
                    .await;
                // One read alone is stopped; its table may be far longer than the drain bound.
                if reads && run.statements == 1 {
                    self.give_up(run);
                } else {
                    let index = run.index;
                    self.read_past(run, Some(index)).await;
                }
                run.index += 1;
                if self.rpc.ended().is_some() {
                    return Cut::Over(After::Stop);
                }
                Cut::Over(After::Next)
            }
        }
    }

    // Holds a parked read until `extend` raises or lifts the row limit, the stream goes, or a
    // stop ends it; a fenced one also expires at its bound, as it holds a transaction open.
    // Biased, so a stop made before a resume wins even if the server has finished.
    async fn park(&mut self, run: &mut Run, out: &mut Out) -> Cut {
        let bound = run.fence.map(|_| self.rules.park_bound);
        let Some(resume) = out.resume.as_mut() else {
            return Cut::Over(self.dropped(run).await);
        };
        let woke = tokio::select! {
            biased;
            () = self.cancel.until_stopped(run.id) => None,
            more = resume.recv() => Some(more),
            () = sleep_or_never(bound) => {
                self.give_up(run);
                out.fail(ExecuteError::ResultExpired).await;
                return Cut::Over(After::Stop);
            }
        };
        match woke {
            Some(Some(more)) => {
                out.limits.max_rows =
                    more.and_then(|more| out.limits.max_rows.map(|max| max.saturating_add(more)));
                Cut::Resume
            }
            Some(None) => Cut::Over(self.dropped(run).await),
            // The stop's error waits in the channel for the caller's `extend` and `next_batch`.
            None => {
                self.read_past(run, None).await;
                run.end_error = None;
                let e = self
                    .cancel
                    .stopped(run.id)
                    .map_or(ExecuteError::Cancelled, Stop::error);
                out.fail(e).await;
                Cut::Over(After::Stop)
            }
        }
    }
}

/// What a cut left the result doing.
enum Cut {
    /// The limit moved, so the same row is tested again.
    Resume,
    Over(After),
}

// The statement ran, but a value in its reply cannot be read.
fn unreadable(message: &str) -> ExecuteError {
    ExecuteError::Database {
        code: "reply".to_owned(),
        sqlstate: None,
        class: None,
        state: None,
        message: format!("Dbento cannot read this reply: {message}; the statement ran"),
        detail: None,
        hint: Some(STRING_CAST.to_owned()),
        position: None,
    }
}

async fn sleep_or_never(bound: Option<Duration>) {
    match bound {
        Some(bound) => tokio::time::sleep(bound).await,
        None => std::future::pending().await,
    }
}
