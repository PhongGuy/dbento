use super::*;
use crate::surreal::decode::decode;

fn map(pairs: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = Vec::new();
    head(&mut w, MAP, pairs.len() as u64);
    for (key, value) in pairs {
        write_param(&Param::Text(key), &mut w);
        w.extend(*value);
    }
    w
}

fn int(n: i64) -> Vec<u8> {
    let mut w = Vec::new();
    write_param(&Param::Int(n), &mut w);
    w
}

fn text(s: &str) -> Vec<u8> {
    let mut w = Vec::new();
    write_param(&Param::Text(s), &mut w);
    w
}

fn pair() -> (HashMap<i64, Route>, Gauge) {
    (HashMap::new(), Gauge::default())
}

fn stream(routes: &mut HashMap<i64, Route>, id: i64) -> mpsc::UnboundedReceiver<(Frame, usize)> {
    let (tx, rx) = mpsc::unbounded_channel();
    routes.insert(id, Route::Stream(tx));
    rx
}

#[test]
fn a_request_is_a_definite_map_the_decoder_reads_back() {
    let bytes = request(
        7,
        "query",
        &[
            Param::Text("RETURN $a"),
            Param::Object(vec![("a", Param::Int(-5))]),
        ],
        Some([9; 16]),
    );
    let Wire::Object(entries) = decode(&bytes).unwrap() else {
        panic!();
    };
    let keys: Vec<_> = entries.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, ["id", "method", "params", "txn"]);
    assert_eq!(entries[0].1, Wire::Int(7));
    assert_eq!(entries[1].1, Wire::Text("query".to_owned()));
    assert_eq!(entries[3].1, Wire::Uuid([9; 16]));
    let Wire::Array(params) = &entries[2].1 else {
        panic!();
    };
    assert_eq!(
        params[1],
        Wire::Object(vec![("a".to_owned(), Wire::Int(-5))])
    );
}

#[test]
fn none_null_and_integer_widths_encode_as_cbor() {
    let param = |p: Param<'_>| {
        let mut w = Vec::new();
        write_param(&p, &mut w);
        w
    };
    assert_eq!(param(Param::None), [0xc6, 0xf6]);
    assert_eq!(param(Param::Int(23)), [0x17]);
    assert_eq!(param(Param::Int(24)), [0x18, 24]);
    assert_eq!(param(Param::Int(70_000)), [0x1a, 0, 1, 0x11, 0x70]);
    assert_eq!(param(Param::Int(-1)), [0x20]);
    assert_eq!(
        decode(&param(Param::Int(i64::MIN))).unwrap(),
        Wire::Int(i64::MIN.into())
    );
    assert_eq!(
        decode(&param(Param::Int(i64::MAX))).unwrap(),
        Wire::Int(i64::MAX.into())
    );
}

#[test]
fn a_reply_finds_its_call_by_id_and_an_unknown_id_is_ignored() {
    let (mut routes, gauge) = pair();
    let (reply, mut wait) = oneshot::channel();
    routes.insert(4, Route::Call(reply));
    let frame = map(&[("id", &int(9)), ("result", &int(1))]);
    route(&frame, &mut routes, &gauge).unwrap();
    assert!(wait.try_recv().is_err(), "another call's reply");
    let frame = map(&[("id", &int(4)), ("result", &int(1))]);
    route(&frame, &mut routes, &gauge).unwrap();
    assert_eq!(wait.try_recv().unwrap(), Ok(Wire::Int(1)));
    assert!(routes.is_empty());
}

#[test]
fn a_call_error_is_the_servers() {
    let (mut routes, gauge) = pair();
    let (reply, mut wait) = oneshot::channel();
    routes.insert(1, Route::Call(reply));
    let error = map(&[("code", &int(-32000)), ("message", &text("no"))]);
    let frame = map(&[("error", &error), ("id", &int(1))]);
    route(&frame, &mut routes, &gauge).unwrap();
    match wait.try_recv().unwrap() {
        Err(RpcError::Server(e)) => assert_eq!(e.message, "no"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_reply_without_an_id_is_a_dropped_notification_or_the_end_of_the_session() {
    let (mut routes, gauge) = pair();
    let live = map(&[("result", &map(&[("action", &text("CREATE"))]))]);
    assert_eq!(route(&live, &mut routes, &gauge), Ok(()));
    let error = map(&[("code", &int(-32700)), ("message", &text("Parse error"))]);
    let failed = map(&[("error", &error)]);
    let why = route(&failed, &mut routes, &gauge).unwrap_err();
    assert!(why.contains("Parse error"), "{why}");
}

#[test]
fn a_frame_with_no_readable_id_ends_the_session() {
    let (mut routes, gauge) = pair();
    let why = route(&[0xff, 0xff], &mut routes, &gauge).unwrap_err();
    assert!(why.contains("cannot read"), "{why}");
}

// 300 nested arrays in a `rows` frame: past the decoder's bound, with a readable envelope.
#[test]
fn a_frame_too_deep_to_decode_still_reaches_its_stream() {
    let (mut routes, gauge) = pair();
    let mut rx = stream(&mut routes, 3);
    let mut deep = vec![0x81; 300];
    deep.push(0x01);
    let result = map(&[
        ("index", &int(0)),
        ("stream", &text("rows")),
        ("values", &deep),
    ]);
    let frame = map(&[("id", &int(3)), ("result", &result)]);
    route(&frame, &mut routes, &gauge).unwrap();
    match rx.try_recv().unwrap().0 {
        Frame::Unreadable { message } => assert!(message.contains("nesting"), "{message}"),
        other => panic!("{other:?}"),
    }
    assert!(routes.contains_key(&3), "the stream goes on");
}

// The socket is still fine, so a call's undecodable reply fails that call, not the session.
#[test]
fn a_call_reply_too_deep_to_decode_fails_only_that_call() {
    let (mut routes, gauge) = pair();
    let (reply, mut wait) = oneshot::channel();
    routes.insert(4, Route::Call(reply));
    let mut deep = vec![0x81; 300];
    deep.push(0x01);
    let frame = map(&[("id", &int(4)), ("result", &deep)]);
    route(&frame, &mut routes, &gauge).unwrap();
    match wait.try_recv().unwrap() {
        Err(RpcError::Unreadable(message)) => assert!(message.contains("nesting"), "{message}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn stream_frames_parse_by_kind_and_end_removes_the_route() {
    let (mut routes, gauge) = pair();
    let mut rx = stream(&mut routes, 2);
    let send = |result: Vec<u8>, routes: &mut HashMap<i64, Route>| {
        let frame = map(&[("id", &int(2)), ("result", &result)]);
        route(&frame, routes, &gauge).unwrap();
    };
    send(
        map(&[("statements", &int(2)), ("stream", &text("begin"))]),
        &mut routes,
    );
    send(
        map(&[
            ("index", &int(1)),
            ("stream", &text("value")),
            ("value", &int(5)),
        ]),
        &mut routes,
    );
    send(
        map(&[
            ("index", &int(1)),
            ("single", &[0xf5]),
            ("stream", &text("finished")),
        ]),
        &mut routes,
    );
    send(map(&[("stream", &text("end"))]), &mut routes);
    assert!(matches!(
        rx.try_recv().unwrap().0,
        Frame::Begin { statements: 2 }
    ));
    assert!(matches!(
        rx.try_recv().unwrap().0,
        Frame::Value {
            index: 1,
            value: Wire::Int(5)
        }
    ));
    assert!(matches!(
        rx.try_recv().unwrap().0,
        Frame::Finished {
            index: 1,
            single: true,
            error: None
        }
    ));
    assert!(matches!(
        rx.try_recv().unwrap().0,
        Frame::End { error: None }
    ));
    assert!(routes.is_empty());
}

#[test]
fn a_call_level_stream_error_is_refused_and_removes_the_route() {
    let (mut routes, gauge) = pair();
    let mut rx = stream(&mut routes, 2);
    let error = map(&[("code", &int(-32000)), ("message", &text("bad"))]);
    let frame = map(&[("error", &error), ("id", &int(2))]);
    route(&frame, &mut routes, &gauge).unwrap();
    assert!(matches!(rx.try_recv().unwrap().0, Frame::Refused(_)));
    assert!(routes.is_empty());
}

fn frames_on(routes: &mut HashMap<i64, Route>, id: i64, gauge: &Arc<Gauge>) -> Frames {
    Frames {
        rx: stream(routes, id),
        gauge: Arc::clone(gauge),
        closed: Arc::default(),
    }
}

fn rows_frame(id: i64) -> Vec<u8> {
    let result = map(&[
        ("index", &int(0)),
        ("stream", &text("rows")),
        ("values", &[0x80]),
    ]);
    map(&[("id", &int(id)), ("result", &result)])
}

// Past the buffer bound the task stops reading, so a dropped reader must take its bytes with it.
#[test]
fn a_dropped_reader_releases_what_it_left_and_what_follows_counts_for_nothing() {
    let mut routes = HashMap::new();
    let gauge = Arc::new(Gauge::default());
    let frames = frames_on(&mut routes, 3, &gauge);
    route(&rows_frame(3), &mut routes, &gauge).unwrap();
    route(&rows_frame(3), &mut routes, &gauge).unwrap();
    assert!(gauge.bytes.load(Ordering::Acquire) > 0);
    drop(frames);
    assert_eq!(gauge.bytes.load(Ordering::Acquire), 0);
    route(&rows_frame(3), &mut routes, &gauge).unwrap();
    assert_eq!(gauge.bytes.load(Ordering::Acquire), 0, "a failed send");
    let end = map(&[
        ("id", &int(3)),
        ("result", &map(&[("stream", &text("end"))])),
    ]);
    route(&end, &mut routes, &gauge).unwrap();
    assert!(
        routes.is_empty(),
        "the stream's end still removes its route"
    );
}

#[test]
fn reading_takes_a_frames_bytes_and_discard_ends_the_reader() {
    let mut routes = HashMap::new();
    let gauge = Arc::new(Gauge::default());
    let mut frames = frames_on(&mut routes, 3, &gauge);
    route(&rows_frame(3), &mut routes, &gauge).unwrap();
    frames.discard();
    frames.discard();
    assert_eq!(gauge.bytes.load(Ordering::Acquire), 0);
}

#[test]
fn the_gauge_goes_over_at_the_bound_and_back_under_as_frames_are_taken() {
    let gauge = Gauge::default();
    gauge.add(MAX_BUFFERED - 1);
    assert!(!gauge.over());
    gauge.add(1);
    assert!(gauge.over());
    gauge.take(1);
    assert!(!gauge.over());
}

// The wait must see a take that lands before it is first polled, or the task stops reading for good.
#[test]
fn room_resolves_for_a_take_before_or_after_the_wait_starts() {
    use futures_util::FutureExt as _;
    let gauge = Gauge::default();
    gauge.add(MAX_BUFFERED);
    let early = gauge.room();
    gauge.take(1);
    assert_eq!(early.now_or_never(), Some(()));
    gauge.add(1);
    let mut late = std::pin::pin!(gauge.room());
    assert_eq!(late.as_mut().now_or_never(), None);
    gauge.take(1);
    assert_eq!(late.as_mut().now_or_never(), Some(()));
}
