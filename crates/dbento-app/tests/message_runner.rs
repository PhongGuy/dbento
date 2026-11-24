//! A tab runner answers stream reads, groups and pending lists on a real Valkey.
#![cfg(feature = "redis")]
use std::time::Duration;

use crate::support::{here, redis_seed as seed, redis_spec, run_cmd, until_event};
use dbento_app::runner::{
    Command, Event, MessageAnswer, MessageCall, MessageReply, Runner, RunnerSpec, spawn_on,
};
use dbento_core::CellRef;
use dbento_core::message::{Direction, MessagePage, MessageRead, Position, SourceRef};
use dbento_testkit::{RdUser, RedisDb, redis_db};

fn spec(db: &RedisDb) -> (tempfile::TempDir, RunnerSpec) {
    redis_spec(
        db,
        db.connection_toml("rd", RdUser::Reader, "read_only = true"),
    )
}
async fn call(r: &Runner, call: MessageCall) -> MessageAnswer {
    let (tx, rx) = async_channel::bounded(1);
    r.send(Command::Messages { call, reply: tx });
    tokio::time::timeout(Duration::from_secs(30), rx.recv())
        .await
        .unwrap()
        .unwrap()
}
fn page(answer: MessageAnswer) -> MessagePage {
    match answer {
        Ok(MessageReply::Page(p)) => p,
        other => panic!("expected a page: {other:?}"),
    }
}
fn text(p: &MessagePage, row: usize, col: usize) -> String {
    match p.batch.cell(row, col) {
        CellRef::Text(t) => t.to_owned(),
        other @ (CellRef::Null | CellRef::Bytes(_) | _) => panic!("row {row} col {col}: {other:?}"),
    }
}
fn browse(db: &RedisDb, key: &str, from: Position, want: u32) -> MessageRead {
    let mut r = MessageRead::browse(SourceRef::redis(db.index(), key), from, Direction::Backward);
    r.want = want;
    r
}

#[tokio::test(flavor = "multi_thread")]
async fn a_backward_read_from_the_end_answers_the_newest_entries_first() {
    let db = redis_db().await;
    let (_dir, spec) = spec(&db);
    let r = spawn_on(&here(), spec);
    let p = page(
        call(
            &r,
            MessageCall::Read(browse(&db, "big:stream", Position::End, 10)),
        )
        .await,
    );
    let names: Vec<&str> = p.columns.iter().map(|c| c.name()).collect();
    assert_eq!(names, ["id", "time", "fields"]);
    assert_eq!((p.batch.len(), p.length), (10, Some(3000)));
    assert_eq!(
        (text(&p, 0, 0), text(&p, 9, 0)),
        ("1-3000".to_owned(), "1-2991".to_owned())
    );
    let before = Position::Before(p.prev.unwrap());
    let p = page(call(&r, MessageCall::Read(browse(&db, "big:stream", before, 10))).await);
    assert_eq!(text(&p, 0, 0), "1-2990");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_follow_read_waits_for_the_entry_written_after_the_newest() {
    let db = redis_db().await;
    seed(&db, "XADD mr:f 1-1 a 1").await;
    let (_dir, spec) = spec(&db);
    let r = spawn_on(&here(), spec);
    let first = page(
        call(
            &r,
            MessageCall::Read(browse(&db, "mr:f", Position::End, 10)),
        )
        .await,
    );
    let follow = MessageRead::follow(
        SourceRef::redis(db.index(), "mr:f"),
        Position::After(first.next.unwrap()),
        Duration::from_secs(1),
    );
    let (answer, _) = tokio::join!(call(&r, MessageCall::Read(follow)), async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        seed(&db, "XADD mr:f 2-1 b 2").await;
    });
    let p = page(answer);
    assert_eq!((p.batch.len(), text(&p, 0, 0)), (1, "2-1".to_owned()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stream_call_sent_during_a_parked_run_closes_unanswered() {
    let db = redis_db().await;
    let (_dir, spec) = spec(&db);
    let r = spawn_on(&here(), spec);
    let events = r.events();
    // 3000 rows park after the default 200 first rows.
    r.send(run_cmd(1, "XRANGE big:stream - +", None));
    until_event(&events, |e| matches!(e, Event::Paused { .. })).await;
    let (tx, rx) = async_channel::bounded(1);
    let call = MessageCall::Read(browse(&db, "big:stream", Position::End, 1));
    r.send(Command::Messages { call, reply: tx });
    let got = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("the reply closes");
    assert!(got.is_err(), "dropped, not answered: {got:?}");
}
