//! The session's serving task: it runs one script at a time over the socket task, splitting a
//! unit's frames into one result per statement. Draining lives here, so it happens whether or
//! not the caller is polling, and the driver only talks to it by channel.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use super::cancel::SurrealCancel;
use super::classify::surreal_classification;
use super::decode::Wire;
use super::rpc::{Frame, Frames, Rpc};
use super::shape::Shape;
use super::units::{Unit, units};
use super::wire::{WireError, statement_error};
use crate::session::{Classification, ExecuteError, Limits, Outcome, Prepared, Writes};
use crate::value::{BatchBuilder, CellRef, ColumnInfo, ResultBatch};

/// Rows that fix a result's columns, unless a limit is reached first.
const SAMPLE_ROWS: usize = 100;

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
}

/// The session as the task holds it.
struct Served {
    rpc: Rpc,
    rules: Rules,
    cancel: Arc<SurrealCancel>,
    /// An error no caller has heard, for the next settle or close.
    owed: Option<ExecuteError>,
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
    mut requests: mpsc::UnboundedReceiver<Request>,
) {
    let mut s = Served {
        rpc,
        rules,
        cancel,
        owed: None,
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
    extra: u32,
}

impl Run {
    fn rolled_back(&self) -> bool {
        self.fence.is_some() && self.writes != Writes::No
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
                    ..
                } => {
                    let mut out = Out {
                        reply: Some(reply),
                        events,
                        limits,
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
        let sql = &text[unit.range.clone()];
        let mut frames = self.rpc.stream(id, sql, None);
        let opened = loop {
            let frame = tokio::time::timeout(self.rpc.call_bound(), frames.next()).await;
            match frame {
                Ok(Frame::Begin { statements }) => break Ok(statements),
                Ok(Frame::Refused(e)) => break Err(statement_error(&e)),
                Ok(Frame::Closed(why) | Frame::Unreadable { message: why }) => {
                    break Err(self.closed(why));
                }
                Ok(_) => {}
                Err(_) => break Err(self.closed(self.rpc.unanswered("query_stream"))),
            }
        };
        let statements = match opened {
            Ok(statements) => statements,
            Err(e) => {
                self.cancel.end();
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
            fence: None,
            transaction: unit.transaction,
            ended: false,
            end_error: None,
            extra: 0,
        });
        self.result(current, out).await
    }

    // Reads the rest of the unit and counts the statements the lexer did not see.
    async fn finish(&mut self, mut run: Run) {
        if !run.ended {
            self.read_past(&mut run, None).await;
        }
        self.cancel.end();
    }

    // Reads frames until statement `upto` finishes, or the unit ends when `None`, within the
    // drain bound. Past the bound the socket closes, which stops the statement.
    async fn read_past(&mut self, run: &mut Run, upto: Option<u64>) {
        let drained = tokio::time::timeout(self.rules.drain_bound, async {
            let mut extras = Vec::new();
            loop {
                match run.frames.next().await {
                    Frame::Finished { index, .. } if upto == Some(index) => break,
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
                self.owed = Some(ExecuteError::StoppedAtDrainCap);
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

    // Nobody reads the rest, so it drains under the bound.
    async fn abandon(&mut self, run: &mut Run) {
        if !run.ended {
            self.read_past(run, None).await;
        }
    }

    // Later statements of a unit run whatever an earlier one did, unless a transaction ends them.
    fn later_note(run: &Run, error: ExecuteError) -> ExecuteError {
        let later = run.index + 1 < run.statements && !run.transaction;
        match error {
            ExecuteError::Database {
                code,
                sqlstate,
                message,
                detail,
                hint,
                position,
            } if later => ExecuteError::Database {
                code,
                sqlstate,
                message: format!("{message}; the later statements of this unit ran"),
                detail,
                hint,
                position,
            },
            other => other,
        }
    }

    // A statement failed: the rest of the unit drains first, so the note about it holds.
    async fn failed(&mut self, run: &mut Run, out: &mut Out, e: ExecuteError) -> After {
        if !run.ended {
            self.read_past(run, None).await;
        }
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
            match run.frames.next().await {
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
                    return self.unreadable(run, out, &message).await;
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
            let e = Self::later_note(run, statement_error(e));
            return self.failed(run, out, e).await;
        }
        out.columns(columns);
        let mut builder = BatchBuilder::new(shape.width());
        let mut delivered = 0u64;
        loop {
            while let Some(row) = rows.pop_front() {
                let cells = shape.cells(&row);
                let refs: Vec<CellRef<'_>> = cells.iter().map(|c| c.as_ref()).collect();
                let closed = builder.push_typed_row(&refs, |i| cells[i].is_number());
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
            match run.frames.next().await {
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
                    return self.unreadable(run, out, &message).await;
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

    // The statement ran, but a value in its reply cannot be read; the unit drains.
    async fn unreadable(&mut self, run: &mut Run, out: &mut Out, message: &str) -> After {
        let e = ExecuteError::database(
            "reply",
            format!("Dbento cannot read this reply: {message}; the statement ran"),
        );
        self.failed(run, out, e).await
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
                let e = Self::later_note(run, statement_error(&e));
                return self.failed(run, out, e).await;
            }
            None => {}
        }
        run.index += 1;
        // The last result waits for `end`, so its outcome can count what followed it.
        if run.index >= run.expected {
            if !run.ended {
                self.read_past(run, None).await;
            }
            if let Some(e) = run.end_error.take() {
                let e = Self::later_note(run, statement_error(&e));
                out.fail(e).await;
                return After::Stop;
            }
            self.cancel.end();
        }
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
}
