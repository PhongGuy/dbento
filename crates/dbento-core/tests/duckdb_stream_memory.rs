#![cfg(all(feature = "duckdb", target_os = "linux"))]
mod support;
use dbento_core::{Limits, SessionOptions};
use support::duckdb_fixture;

fn rss_mb() -> u64 {
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap();
    let pages: u64 = statm.split_whitespace().nth(1).unwrap().parse().unwrap();
    pages * 4096 / (1024 * 1024)
}

#[tokio::test]
async fn five_million_rows_stream_in_flat_memory() {
    let fx = duckdb_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let mut script = s.script("SELECT range AS x FROM range(5000000)");
    let p = script.next().await.unwrap().unwrap();
    let mut stream = p.execute(Limits::default()).await.unwrap();
    let first = stream.next_batch().await.unwrap().unwrap();
    let base = rss_mb();
    let (mut rows, mut peak) = (first.len() as u64, base);
    while let Some(batch) = stream.next_batch().await.unwrap() {
        rows += batch.len() as u64;
        peak = peak.max(rss_mb());
    }
    assert_eq!(rows, 5_000_000);
    println!("rss after first batch {base} MB, peak {peak} MB");
    assert!(peak - base < 256, "grew {} MB", peak - base);
}
