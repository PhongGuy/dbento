//! Its own test binary: twenty containers start at once while other sockets churn through the
//! ephemeral port range, the way a full suite's connections do.
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dbento_testkit::pg_container;
use futures_util::future::join_all;

/// Keeps a rolling window of outbound loopback connections open until `stop` is set, so new
/// ephemeral ports are taken while the containers' host ports are being bound.
fn churn(stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    let listener = TcpListener::bind("127.0.0.1:0").expect("churn listener");
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let mut held = std::collections::VecDeque::new();
        let mut accepted = std::collections::VecDeque::new();
        while !stop.load(Ordering::SeqCst) {
            if let Ok(s) = TcpStream::connect(addr) {
                held.push_back(s);
            }
            while let Ok((s, _)) = listener.accept() {
                accepted.push_back(s);
            }
            // The accepted end closes first, so TIME_WAIT lands on the listener's port and the
            // outbound port is free again; the range never runs dry.
            if accepted.len() > 300 {
                accepted.pop_front();
            }
            if held.len() > 400 {
                held.pop_front();
            }
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
    })
}

#[tokio::test]
async fn twenty_containers_start_in_parallel() {
    let stop = Arc::new(AtomicBool::new(false));
    let churner = churn(stop.clone());
    let dbs = join_all((0..20).map(|_| pg_container())).await;
    stop.store(true, Ordering::SeqCst);
    churner.join().unwrap();
    for db in &dbs {
        assert_eq!(db.scalar("SELECT 1").await.as_deref(), Some("1"));
    }
}
