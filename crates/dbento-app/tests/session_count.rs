//! How many sessions the app opens at once on one connection. A listener that accepts and
//! never answers holds every Postgres connect open, so what it has seen is the peak.
use crate::support::{open_connection_row, started_custom, wait_real};
use gpui_kit::{AppContext as _, TestAppContext};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Accepts on 127.0.0.1 and keeps every socket, counting them.
struct Sink {
    port: u16,
    seen: Arc<AtomicUsize>,
    _held: Arc<Mutex<Vec<TcpStream>>>,
}

fn sink() -> Sink {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(AtomicUsize::new(0));
    let held = Arc::new(Mutex::new(Vec::new()));
    let (s, h) = (Arc::clone(&seen), Arc::clone(&held));
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            h.lock().unwrap().push(stream);
            s.fetch_add(1, Ordering::SeqCst);
        }
    });
    Sink {
        port,
        seen,
        _held: held,
    }
}

#[gpui_kit::test]
fn record_the_sessions_tabs_and_the_explorer_open_at_once(cx: &mut TestAppContext) {
    const TABS: usize = 3;
    let sink = sink();
    let toml = format!(
        "dbento = 1\n[connections.hang]\nengine = \"postgres\"\nhost = \"127.0.0.1\"\nport = {}\ndatabase = \"d\"\nuser = \"u\"\nsecret = {{ env = \"DBENTO_SECRET_TEST_HANG_PW\" }}\n",
        sink.port
    );
    let s = started_custom(cx, &toml, |svc| {
        svc.env = Arc::new(|v| (v == "DBENTO_SECRET_TEST_HANG_PW").then(|| "pw".to_owned()));
    });
    // The row opens the first tab and selects the connection, so the explorer loads it too.
    open_connection_row(cx, s.win, "hang");
    for _ in 1..TABS {
        let shell = s.shell.clone();
        cx.update_window(s.win, |_, w, cx| {
            shell.update(cx, |sh, cx| sh.open_connection_named("hang", w, cx))
        })
        .unwrap();
        cx.run_until_parked();
    }
    wait_real(cx, "a connect per tab", |_| {
        sink.seen.load(Ordering::SeqCst) >= TABS
    });
    // Later openers (catalog, tab model) get two seconds to arrive before the peak is read.
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(20));
    }
    let peak = sink.seen.load(Ordering::SeqCst);
    eprintln!("sessions at once on one connection with {TABS} tabs: {peak}");
}
