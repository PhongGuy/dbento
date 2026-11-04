use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use ::duckdb::{Config, Connection};

use super::{Mode, lock_message, open};
use crate::session::ConnectError;

/// A DuckDB file at `dir/name` holding `t(a INTEGER)` with one row, closed again.
fn seed(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    let home = dir.join("seed-home");
    let cfg = Config::default()
        .enable_autoload_extension(false)
        .unwrap()
        .with("home_directory", home.to_str().unwrap())
        .unwrap();
    let c = Connection::open_with_flags(&path, cfg).unwrap();
    c.execute_batch("CREATE TABLE t(a INTEGER); INSERT INTO t VALUES (1)")
        .unwrap();
    path
}

fn count(c: &Connection) -> i64 {
    c.query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap()
}

fn open_message(r: Result<super::Lease, ConnectError>) -> String {
    match r {
        Err(ConnectError::Open { message }) => message,
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("opened"),
    }
}

#[test]
fn two_leases_on_one_file_share_one_instance() {
    let dir = tempfile::tempdir().unwrap();
    let path = seed(dir.path(), "a.duckdb");
    let a = open(&path, Mode::ReadWrite).unwrap();
    let b = open(&path, Mode::ReadWrite).unwrap();
    a.conn().execute_batch("INSERT INTO t VALUES (2)").unwrap();
    assert_eq!(count(b.conn()), 2);
    drop(a);
    b.conn().execute_batch("INSERT INTO t VALUES (3)").unwrap();
    drop(b);
    let again = open(&path, Mode::Locked).unwrap();
    assert_eq!(
        count(again.conn()),
        3,
        "committed rows survive the instance closing"
    );
}

#[cfg(unix)]
#[test]
fn a_symlinked_path_reaches_the_same_instance() {
    let dir = tempfile::tempdir().unwrap();
    let path = seed(dir.path(), "a.duckdb");
    let link = dir.path().join("link.duckdb");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    let a = open(&path, Mode::ReadWrite).unwrap();
    let b = open(&link, Mode::ReadWrite).unwrap();
    a.conn().execute_batch("INSERT INTO t VALUES (2)").unwrap();
    assert_eq!(count(b.conn()), 2);
}

#[test]
fn a_session_in_the_other_mode_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = seed(dir.path(), "a.duckdb");
    let writer = open(&path, Mode::ReadWrite).unwrap();
    let p = path.display();
    let want = format!(
        "`{p}` is already open read-write in this process; a read-only session needs it closed"
    );
    assert_eq!(open_message(open(&path, Mode::Locked)), want);
    drop(writer);
    let reader = open(&path, Mode::Locked).unwrap();
    let want = format!(
        "`{p}` is already open read-only in this process; a writable session needs it closed"
    );
    assert_eq!(open_message(open(&path, Mode::ReadWrite)), want);
    assert_eq!(count(open(&path, Mode::Locked).unwrap().conn()), 1);
    drop(reader);
}

#[test]
fn a_file_that_is_not_duckdb_is_an_open_error_with_duckdbs_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("junk.duckdb");
    std::fs::write(&path, b"not a database at all, just bytes").unwrap();
    let m = open_message(open(&path, Mode::ReadWrite));
    assert!(m.contains("not a valid DuckDB database file"), "{m}");
}

/// Each write path on a locked instance fails with the DuckDB text the error mapping relies on.
#[test]
fn a_locked_instance_refuses_writes_with_the_pinned_texts() {
    let dir = tempfile::tempdir().unwrap();
    let path = seed(dir.path(), "a.duckdb");
    let out = dir.path().join("out.csv");
    let lease = open(&path, Mode::Locked).unwrap();
    for (sql, marker) in [
        (
            "INSERT INTO t VALUES (2)".to_owned(),
            "attached in read-only mode",
        ),
        (
            format!("COPY (SELECT 1) TO '{}'", out.display()),
            "disabled by configuration",
        ),
        (
            "SET GLOBAL memory_limit = '1GB'".to_owned(),
            "the configuration has been locked",
        ),
    ] {
        let e = lease.conn().execute_batch(&sql).unwrap_err().to_string();
        assert!(e.contains(marker), "{sql}: {e}");
    }
    assert!(!out.exists());
}

#[test]
fn every_open_has_autoload_off_and_its_home_in_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let path = seed(dir.path(), "a.duckdb");
    for mode in [Mode::ReadWrite, Mode::Locked] {
        let lease = open(&path, mode).unwrap();
        let sql = "SELECT CAST(current_setting(?) AS VARCHAR)";
        let setting =
            |n: &str| -> String { lease.conn().query_row(sql, [n], |r| r.get(0)).unwrap() };
        assert_eq!(setting("autoload_known_extensions"), "false");
        assert_eq!(setting("autoinstall_known_extensions"), "false");
        let home = setting("home_directory");
        assert!(
            home.ends_with("dbento/duckdb") || home.ends_with("dbento-duckdb"),
            "{home}"
        );
        assert_eq!(setting("extension_directory"), home);
    }
}

const RW_HELD: &str = "IO Error: Could not set lock on file \"/x/a.duckdb\": Conflicting lock is held in /usr/bin/dbento (PID 7) by user me. See also https://duckdb.org/docs/stable/connect/concurrency";
const RO_HELD: &str = "IO Error: Could not set lock on file \"/x/a.duckdb\": Conflicting lock is held in /usr/bin/dbento (PID 7) by user me. However, you would be able to open this database in read-only mode, e.g. by using the -readonly parameter in the CLI. See also https://duckdb.org/docs/stable/connect/concurrency";

#[test]
fn the_two_lock_texts_name_what_the_other_process_holds() {
    let p = Path::new("/x/a.duckdb");
    assert_eq!(
        lock_message(p, RW_HELD).as_deref(),
        Some("`/x/a.duckdb` is open read-write in another process (for example the Dbento app)")
    );
    assert_eq!(
        lock_message(p, RO_HELD).as_deref(),
        Some(
            "`/x/a.duckdb` is open read-only in another process; DuckDB opens it for writing only when no other process has it"
        )
    );
    assert_eq!(lock_message(p, "IO Error: Cannot open file"), None);
}

/// The child side of the cross-process tests: opens the file the variable names, says
/// `ready`, and holds it until stdin closes.
#[test]
#[ignore = "run only as the child of a cross-process test"]
fn child_process_helper() {
    let Ok(spec) = std::env::var("DBENTO_DUCKDB_CHILD") else {
        return;
    };
    let (mode, path) = spec.split_once(':').unwrap();
    let mode = if mode == "ro" {
        Mode::Locked
    } else {
        Mode::ReadWrite
    };
    let lease = open(Path::new(path), mode).unwrap();
    println!("ready");
    let _ = std::io::stdin().read_to_end(&mut Vec::new());
    drop(lease);
}

struct Holder(Child, BufReader<std::process::ChildStdout>);

impl Holder {
    /// Re-runs this test binary as `child_process_helper`, returning once it holds `path`.
    fn start(mode: &str, path: &Path, home: Option<&Path>) -> Holder {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "duckdb::registry::tests::child_process_helper",
            "--ignored",
            "--nocapture",
        ])
        .env("DBENTO_DUCKDB_CHILD", format!("{mode}:{}", path.display()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
        if let Some(home) = home {
            cmd.env("HOME", home).env_remove("XDG_CACHE_HOME");
        }
        let mut child = cmd.spawn().unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        while !line.contains("ready") {
            line.clear();
            assert!(
                out.read_line(&mut line).unwrap() > 0,
                "the child exited first"
            );
        }
        Holder(child, out)
    }

    fn release(mut self) {
        drop(self.0.stdin.take());
        std::io::copy(&mut self.1, &mut std::io::sink()).unwrap();
        self.0.wait().unwrap();
    }
}

#[test]
fn a_file_another_process_holds_is_refused_naming_its_mode() {
    let dir = tempfile::tempdir().unwrap();
    let path = seed(dir.path(), "a.duckdb");
    let writer = Holder::start("rw", &path, None);
    let want = format!(
        "`{}` is open read-write in another process (for example the Dbento app)",
        path.display()
    );
    assert_eq!(open_message(open(&path, Mode::Locked)), want);
    assert_eq!(open_message(open(&path, Mode::ReadWrite)), want);
    writer.release();
    let reader = Holder::start("ro", &path, None);
    assert!(open(&path, Mode::Locked).is_ok(), "two processes may read");
    assert_eq!(
        open_message(open(&path, Mode::ReadWrite)),
        format!(
            "`{}` is open read-only in another process; DuckDB opens it for writing only when no other process has it",
            path.display()
        )
    );
    reader.release();
}

#[test]
fn an_open_leaves_no_duckdb_directory_in_home() {
    let dir = tempfile::tempdir().unwrap();
    let path = seed(dir.path(), "a.duckdb");
    let home = dir.path().join("home");
    std::fs::create_dir(&home).unwrap();
    Holder::start("rw", &path, Some(&home)).release();
    assert!(!home.join(".duckdb").exists(), "DuckDB wrote ~/.duckdb");
}
