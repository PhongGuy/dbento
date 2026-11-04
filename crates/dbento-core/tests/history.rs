use dbento_core::{
    HISTORY_COMPACT_LINES, HISTORY_KEEP_BYTES, HISTORY_KEEP_LINES, HISTORY_MAX_MESSAGE_BYTES,
    HISTORY_MAX_SQL_BYTES, History, HistoryEntry, HistoryError, HistoryFilter, HistoryOutcome,
    HistorySource,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

const OK: HistoryOutcome = HistoryOutcome::Ok {
    rows: 3,
    truncated: false,
    rows_affected: None,
};

fn entry(ws: &str, conn: &str, sql: &str, outcome: HistoryOutcome) -> HistoryEntry {
    let at = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
    HistoryEntry::new(
        at,
        Path::new(ws),
        conn,
        sql,
        Duration::from_micros(4_321),
        outcome,
    )
}
fn file_in(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("sub/history.jsonl")
}
fn lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}
fn all(path: &Path) -> Vec<HistoryEntry> {
    History::open(path)
        .unwrap()
        .recent(&HistoryFilter::default(), usize::MAX)
}
fn sqls(entries: &[HistoryEntry]) -> Vec<&str> {
    entries.iter().map(HistoryEntry::sql).collect()
}
/// `n` valid lines whose SQL is `SELECT <i>` followed by `pad` x's, oldest first.
fn prefill(path: &Path, n: usize, pad: usize) {
    History::open(path)
        .unwrap()
        .append(&entry("/ws", "lite", "seed", OK))
        .unwrap();
    let template: serde_json::Value = serde_json::from_str(&lines(path)[0]).unwrap();
    let pad = "x".repeat(pad);
    let mut text = String::new();
    for i in 0..n {
        let mut v = template.clone();
        v["sql"] = format!("SELECT {i}{pad}").into();
        text += &v.to_string();
        text.push('\n');
    }
    fs::write(path, text).unwrap();
}
#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn an_appended_entry_is_one_v1_line_that_reads_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = file_in(&dir);
    let e = entry("/ws", "lite", "SELECT 1", OK);
    History::open(&path).unwrap().append(&e).unwrap();
    let ls = lines(&path);
    assert_eq!(ls.len(), 1);
    let v: serde_json::Value = serde_json::from_str(&ls[0]).unwrap();
    assert_eq!(v["v"], 1);
    assert_eq!(all(&path), vec![e]);
    #[cfg(unix)]
    assert_eq!(mode(&path), 0o600);
}

#[test]
fn every_outcome_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = file_in(&dir);
    let outcomes = [
        HistoryOutcome::Ok {
            rows: 0,
            truncated: true,
            rows_affected: Some(5),
        },
        HistoryOutcome::Failed {
            message: "no such table: x".into(),
        },
        HistoryOutcome::Cancelled,
        HistoryOutcome::Skipped,
    ];
    let mut h = History::open(&path).unwrap();
    for o in &outcomes {
        h.append(&entry("/ws", "lite", "SELECT 1", o.clone()))
            .unwrap();
    }
    let got: Vec<_> = all(&path)
        .into_iter()
        .rev()
        .map(|e| e.outcome().clone())
        .collect();
    assert_eq!(got, outcomes);
}

#[test]
fn recent_is_newest_first_filtered_and_limited() {
    let dir = tempfile::tempdir().unwrap();
    let path = file_in(&dir);
    let mut h = History::open(&path).unwrap();
    for (ws, conn, sql) in [
        ("/a", "lite", "SELECT apple"),
        ("/a", "pg", "select Banana"),
        ("/b", "lite", "SELECT apple pie"),
        ("/a", "lite", "DELETE FROM t"),
    ] {
        h.append(&entry(ws, conn, sql, OK)).unwrap();
    }
    let f = HistoryFilter::default;
    assert_eq!(
        sqls(&h.recent(&f(), 2)),
        ["DELETE FROM t", "SELECT apple pie"]
    );
    assert_eq!(
        sqls(&h.recent(&f().workspace(Path::new("/a")), 10)),
        ["DELETE FROM t", "select Banana", "SELECT apple"]
    );
    assert_eq!(
        sqls(&h.recent(&f().workspace(Path::new("/a")).connection("lite"), 10)),
        ["DELETE FROM t", "SELECT apple"]
    );
    assert_eq!(sqls(&h.recent(&f().text("BANANA"), 10)), ["select Banana"]);
    assert_eq!(
        sqls(&h.recent(&f().text("apple"), 10)),
        ["SELECT apple pie", "SELECT apple"]
    );
    assert!(h.recent(&f(), 0).is_empty());
}

#[test]
fn corrupt_unknown_and_torn_lines_are_skipped_and_appending_continues() {
    let dir = tempfile::tempdir().unwrap();
    let path = file_in(&dir);
    prefill(&path, 1, 0);
    let mut text = fs::read_to_string(&path).unwrap();
    text += "not json\n{\"v\":2,\"sql\":\"from the future\"}\n{\"v\":1,\"at_ms\":";
    fs::write(&path, text).unwrap();
    let mut h = History::open(&path).unwrap();
    assert_eq!(sqls(&h.recent(&HistoryFilter::default(), 10)), ["SELECT 0"]);
    h.append(&entry("/ws", "lite", "SELECT after", OK)).unwrap();
    assert_eq!(sqls(&all(&path)), ["SELECT after", "SELECT 0"]);
}

#[cfg(unix)]
#[test]
fn a_symlink_at_the_path_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("elsewhere.jsonl");
    fs::write(&target, "").unwrap();
    let link = dir.path().join("history.jsonl");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(matches!(History::open(&link), Err(HistoryError::Io { .. })));
    assert_eq!(fs::read_to_string(&target).unwrap(), "");
}

#[test]
fn long_sql_and_messages_are_cut_at_a_char_boundary() {
    let long = "é".repeat(HISTORY_MAX_SQL_BYTES);
    let failed = HistoryOutcome::Failed {
        message: "é".repeat(HISTORY_MAX_MESSAGE_BYTES),
    };
    let e = entry("/ws", "lite", &long, failed);
    assert!(e.sql().len() <= HISTORY_MAX_SQL_BYTES && e.sql().len() >= HISTORY_MAX_SQL_BYTES - 1);
    assert!(long.starts_with(e.sql()));
    let HistoryOutcome::Failed { message } = e.outcome() else {
        panic!()
    };
    assert!(
        message.len() <= HISTORY_MAX_MESSAGE_BYTES
            && message.len() >= HISTORY_MAX_MESSAGE_BYTES - 1
    );
}

#[test]
fn passing_the_line_cap_keeps_the_newest_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = file_in(&dir);
    prefill(&path, HISTORY_COMPACT_LINES, 0);
    History::open(&path)
        .unwrap()
        .append(&entry("/ws", "lite", "SELECT last", OK))
        .unwrap();
    assert_eq!(lines(&path).len(), HISTORY_KEEP_LINES);
    let got = all(&path);
    assert_eq!(got[0].sql(), "SELECT last");
    assert_eq!(got.last().unwrap().sql(), "SELECT 20001");
    #[cfg(unix)]
    assert_eq!(mode(&path), 0o600);
}

#[test]
fn passing_the_byte_cap_keeps_at_most_the_byte_budget() {
    let dir = tempfile::tempdir().unwrap();
    let path = file_in(&dir);
    prefill(&path, 600, 60_000); // about 36 MB, past HISTORY_COMPACT_BYTES
    History::open(&path)
        .unwrap()
        .append(&entry("/ws", "lite", "SELECT last", OK))
        .unwrap();
    assert!(fs::metadata(&path).unwrap().len() <= HISTORY_KEEP_BYTES);
    let got = all(&path);
    assert_eq!(got[0].sql(), "SELECT last");
    assert!(got.len() > 200);
}

#[test]
fn a_held_lock_skips_compaction_with_locked_but_not_the_append() {
    let dir = tempfile::tempdir().unwrap();
    let path = file_in(&dir);
    prefill(&path, HISTORY_COMPACT_LINES + 1, 0);
    let other = fs::File::open(&path).unwrap();
    other.lock_shared().unwrap();
    let mut h = History::open(&path).unwrap();
    assert!(matches!(h.compact(), Err(HistoryError::Locked { .. })));
    h.append(&entry("/ws", "lite", "SELECT while locked", OK))
        .unwrap();
    assert_eq!(lines(&path).len(), HISTORY_COMPACT_LINES + 2);
    other.unlock().unwrap();
    h.compact().unwrap();
    assert_eq!(lines(&path).len(), HISTORY_KEEP_LINES);
    assert_eq!(all(&path)[0].sql(), "SELECT while locked");
}

#[test]
fn an_appender_follows_a_compaction_to_the_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = file_in(&dir);
    prefill(&path, HISTORY_COMPACT_LINES, 0);
    let mut h1 = History::open(&path).unwrap();
    let mut h2 = History::open(&path).unwrap();
    h2.append(&entry("/ws", "lite", "from h2", OK)).unwrap(); // compacts
    h1.append(&entry("/ws", "lite", "from h1", OK)).unwrap();
    assert_eq!(lines(&path).len(), HISTORY_KEEP_LINES + 1);
    assert_eq!(sqls(&all(&path)[..2]), ["from h1", "from h2"]);
}

#[test]
fn concurrent_appenders_lose_nothing_across_a_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let path = file_in(&dir);
    prefill(&path, HISTORY_COMPACT_LINES, 0);
    // Each thread opens its own handle, so each holds its own open file description and
    // flock contends between them exactly as between processes.
    let threads: Vec<_> = (0..2)
        .map(|t| {
            let path = path.clone();
            std::thread::spawn(move || {
                let mut h = History::open(&path).unwrap();
                for i in 0..1000 {
                    h.append(&entry("/ws", "lite", &format!("t{t}-{i}"), OK))
                        .unwrap();
                }
            })
        })
        .collect();
    threads.into_iter().for_each(|t| t.join().unwrap());
    let got = all(&path);
    let written = lines(&path).iter().filter(|l| !l.is_empty()).count(); // a newline guard may add a blank line
    assert_eq!(got.len(), written, "no corrupt line");
    assert!(written < HISTORY_COMPACT_LINES, "a compaction ran");
    for t in 0..2 {
        for i in 0..1000 {
            let want = format!("t{t}-{i}");
            assert!(got.iter().any(|e| e.sql() == want), "{want} was lost");
        }
    }
}

#[test]
fn the_source_is_written_and_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = file_in(&dir);
    let app = entry("/ws", "lite", "SELECT 1", OK);
    assert_eq!(app.source(), HistorySource::App);
    let mcp = entry("/ws", "lite", "SELECT 2", OK).with_source(HistorySource::Mcp);
    let mut h = History::open(&path).unwrap();
    h.append(&app).unwrap();
    h.append(&mcp).unwrap();
    let v: Vec<serde_json::Value> = lines(&path)
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        (v[0]["source"].as_str(), v[0]["v"].as_u64()),
        (Some("app"), Some(1))
    );
    assert_eq!(
        (v[1]["source"].as_str(), v[1]["v"].as_u64()),
        (Some("mcp"), Some(1))
    );
    assert_eq!(all(&path), vec![mcp, app]);
}

#[test]
fn a_line_without_a_source_reads_as_app() {
    let dir = tempfile::tempdir().unwrap();
    let path = file_in(&dir);
    History::open(&path)
        .unwrap()
        .append(&entry("/ws", "lite", "SELECT 1", OK).with_source(HistorySource::Mcp))
        .unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&lines(&path)[0]).unwrap();
    v.as_object_mut().unwrap().remove("source");
    fs::write(&path, format!("{v}\n")).unwrap();
    let read = all(&path);
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].source(), HistorySource::App);
}
