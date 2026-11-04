//! The duckdb crate behaviours the driver relies on, pinned at =1.20000.1 (libduckdb v2.0.0).
//! A failure here after a crate bump means the driver's premises moved: re-plan, do not patch.
#![cfg(feature = "duckdb")]

use duckdb::arrow::array::Array;
use duckdb::arrow::datatypes::DataType;
use duckdb::{AccessMode, Config, Connection};

fn config() -> Config {
    Config::default().enable_autoload_extension(false).unwrap()
}

fn fresh() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let conn = Connection::open_with_flags(dir.path().join("a.duckdb"), config()).unwrap();
    (dir, conn)
}

/// The streamed result's columns as (name, Arrow type), the result read to its end.
fn shape(conn: &Connection, sql: &str) -> Vec<(String, DataType)> {
    let mut st = conn.prepare(sql).unwrap();
    drop(st.stream_arrow([]).unwrap());
    let schema = st.schema();
    while st.step().unwrap().is_some() {}
    schema
        .fields()
        .iter()
        .map(|f| (f.name().clone(), f.data_type().clone()))
        .collect()
}

#[test]
fn the_bundled_library_is_v2_0_0() {
    let (_dir, conn) = fresh();
    let v: String = conn
        .query_row("SELECT version()", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, "v2.0.0");
}

#[test]
fn status_results_are_a_count_or_a_success_column() {
    let (_dir, conn) = fresh();
    let count = vec![("Count".to_owned(), DataType::Int64)];
    assert_eq!(shape(&conn, "CREATE TABLE t(a INTEGER)"), count);
    assert_eq!(shape(&conn, "INSERT INTO t VALUES (1), (2)"), count);
    assert_eq!(shape(&conn, "UPDATE t SET a = a + 1"), count);
    assert_eq!(shape(&conn, "DELETE FROM t WHERE a = 3"), count);
    assert_eq!(
        shape(&conn, "SET threads = 2"),
        vec![("Success".to_owned(), DataType::Boolean)]
    );
    assert_eq!(
        shape(&conn, "INSERT INTO t VALUES (9) RETURNING a"),
        vec![("a".to_owned(), DataType::Int32)]
    );
}

#[test]
fn a_lone_prepare_runs_nothing_and_step_streams_every_chunk() {
    let (_dir, conn) = fresh();
    conn.execute_batch("CREATE TABLE t(a INTEGER)").unwrap();
    drop(conn.prepare("INSERT INTO t VALUES (1)").unwrap());
    let n: i64 = conn
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
    let mut st = conn.prepare("SELECT * FROM range(5000)").unwrap();
    drop(st.stream_arrow([]).unwrap());
    let mut rows = 0;
    while let Some(chunk) = st.step().unwrap() {
        rows += chunk.len();
    }
    assert_eq!(rows, 5000);
}

#[test]
fn the_open_settings_the_registry_uses_are_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.duckdb");
    drop(Connection::open_with_flags(&path, config()).unwrap());
    let home = dir.path().join("home");
    let home_text = home.to_str().unwrap();
    let cfg = config()
        .access_mode(AccessMode::ReadOnly)
        .unwrap()
        .with("home_directory", home_text)
        .unwrap()
        .with("extension_directory", home_text)
        .unwrap()
        .enable_external_access(false)
        .unwrap()
        .with("lock_configuration", "true")
        .unwrap();
    let conn = Connection::open_with_flags(&path, cfg).unwrap();
    let setting = |name: &str| -> String {
        conn.query_row("SELECT CAST(current_setting(?) AS VARCHAR)", [name], |r| {
            r.get(0)
        })
        .unwrap()
    };
    assert_eq!(setting("home_directory"), home_text);
    assert_eq!(setting("extension_directory"), home_text);
    assert_eq!(setting("autoload_known_extensions"), "false");
    assert_eq!(setting("autoinstall_known_extensions"), "false");
    assert_eq!(setting("enable_external_access"), "false");
    assert_eq!(setting("lock_configuration"), "true");
}

/// Cannot finish within any test bound: 10^13 rows through a cross join.
const SPIN: &str = "SELECT count(*) FROM range(10000000) a, range(1000000) b";
/// `SPIN` as a write, which unlike a SELECT does its work inside execute.
const SPIN_WRITE: &str =
    "CREATE TABLE spun AS SELECT count(*) FROM range(10000000) a, range(1000000) b";

fn memory() -> Connection {
    Connection::open_in_memory_with_flags(config()).unwrap()
}

#[test]
fn an_interrupt_on_an_idle_connection_is_cleared_by_the_next_statement() {
    let c = memory();
    c.interrupt_handle().interrupt();
    let one: i64 = c.query_row("SELECT 1", [], |r| r.get(0)).unwrap();
    assert_eq!(one, 1);
}

#[test]
fn an_interrupt_between_prepare_and_execute_is_lost() {
    // Execution start clears the flag, so a stop must be fired again until the statement ends.
    let c = memory();
    let mut st = c.prepare("SELECT 42").unwrap();
    c.interrupt_handle().interrupt();
    drop(st.stream_arrow([]).unwrap());
    assert_eq!(st.step().unwrap().map(|chunk| chunk.len()), Some(1));
}

#[test]
fn an_interrupt_fails_a_running_execute_and_the_connection_survives() {
    let c = memory();
    let h = c.interrupt_handle();
    let firing = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        h.interrupt();
    });
    let mut st = c.prepare(SPIN_WRITE).unwrap();
    let Err(e) = st.stream_arrow([]) else {
        panic!("the execute was not interrupted");
    };
    firing.join().unwrap();
    assert_eq!(e.to_string(), "INTERRUPT Error: Interrupted!");
    drop(st);
    let seven: i64 = c.query_row("SELECT 7", [], |r| r.get(0)).unwrap();
    assert_eq!(seven, 7);
}

#[test]
fn a_select_executes_at_once_and_an_interrupt_fails_its_first_step() {
    let c = memory();
    let mut st = c.prepare(SPIN).unwrap();
    drop(st.stream_arrow([]).unwrap());
    let h = c.interrupt_handle();
    let firing = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        h.interrupt();
    });
    let Err(e) = st.step() else {
        panic!("the first step was not interrupted");
    };
    firing.join().unwrap();
    assert_eq!(e.to_string(), "INTERRUPT Error: Interrupted!");
}

#[test]
fn an_interrupt_after_execute_holds_for_the_first_step() {
    // The guard fires once after `executed`, so nothing may clear the flag from there on.
    let c = memory();
    let mut st = c.prepare(SPIN).unwrap();
    drop(st.stream_arrow([]).unwrap());
    c.interrupt_handle().interrupt();
    let Err(e) = st.step() else {
        panic!("the interrupt was lost");
    };
    assert_eq!(e.to_string(), "INTERRUPT Error: Interrupted!");
}

#[test]
fn a_select_reports_a_runtime_error_at_its_first_step() {
    let c = memory();
    let mut st = c.prepare("SELECT error('boom')").unwrap();
    drop(st.stream_arrow([]).unwrap());
    let Err(e) = st.step() else {
        panic!("the error was not raised");
    };
    assert_eq!(e.to_string(), "Invalid Input Error: boom");
}

#[test]
fn a_write_is_whole_at_execute_before_any_step() {
    let (_dir, c) = fresh();
    c.execute_batch("CREATE TABLE t(a BIGINT)").unwrap();
    let other = c.try_clone().unwrap();
    let mut st = c
        .prepare("INSERT INTO t SELECT range FROM range(2000000) RETURNING a")
        .unwrap();
    drop(st.stream_arrow([]).unwrap());
    let seen: i64 = other
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(seen, 2_000_000);
}

#[test]
fn an_interrupt_between_steps_fails_the_next_step() {
    let c = memory();
    let mut st = c.prepare("SELECT * FROM range(100000000)").unwrap();
    drop(st.stream_arrow([]).unwrap());
    assert!(st.step().unwrap().is_some());
    c.interrupt_handle().interrupt();
    let Err(e) = st.step() else {
        panic!("the pending interrupt did not fail the fetch");
    };
    assert_eq!(e.to_string(), "INTERRUPT Error: Interrupted!");
}

#[test]
fn an_interrupt_reaches_only_its_own_clone() {
    let c = memory();
    let other = c.try_clone().unwrap();
    let h = c.interrupt_handle();
    let firing = std::thread::spawn(move || {
        for _ in 0..20 {
            h.interrupt();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    });
    let n: i64 = other
        .query_row("SELECT count(*) FROM range(300000000)", [], |r| r.get(0))
        .unwrap();
    firing.join().unwrap();
    assert_eq!(n, 300_000_000);
}
