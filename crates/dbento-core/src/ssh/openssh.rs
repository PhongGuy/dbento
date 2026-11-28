//! The system OpenSSH client: finding it on PATH, checking an alias before it reaches ssh, and
//! one bounded `ssh` call.

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// The most stdout one call may print; more is refused, never truncated.
const STDOUT_CAP: usize = 256 * 1024;

/// How much stderr a reader keeps; the rest is drained so ssh never blocks on it.
const STDERR_CAP: usize = 64 * 1024;

/// The stderr lines an error message keeps.
const STDERR_LINES: usize = 20;

/// The least time the pipe readers get after ssh exits.
const PIPE_GRACE: Duration = Duration::from_millis(250);

/// The client's file name in a PATH entry.
#[cfg(windows)]
const PROGRAM: &str = "ssh.exe";
#[cfg(not(windows))]
const PROGRAM: &str = "ssh";

/// The first absolute PATH entry holding an ssh: cfg(unix) `ssh`, a file with an exec bit;
/// cfg(windows) `ssh.exe`, a file. Relative entries are never searched.
pub(super) fn discover(path: Option<&OsStr>) -> Option<PathBuf> {
    std::env::split_paths(path?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(PROGRAM))
        .find(|candidate| runnable(candidate))
}

#[cfg(unix)]
fn runnable(candidate: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(candidate)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn runnable(candidate: &Path) -> bool {
    candidate.is_file()
}

/// ASCII alphanumerics and `.-_:` only, non-empty, no leading `-`, so ssh can neither read
/// the alias as an option nor split a user or port out of it.
pub(super) fn valid_alias(alias: &str) -> bool {
    !alias.is_empty()
        && !alias.starts_with('-')
        && alias
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':'))
}

/// One `program args..` call in `cwd`, stdin null, stopped at `limit`; stdout on exit 0.
///
/// Errors are text: "cannot start: ..", "exited with ..: <last 20 stderr lines>",
/// "did not answer within ..", "printed more than 256 KiB". Only ssh itself is killed.
pub(super) fn run(
    program: &Path,
    args: &[OsString],
    cwd: &Path,
    limit: Duration,
) -> Result<Vec<u8>, String> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // A GUI build would otherwise flash a console window for every resolve.
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000);

    let deadline = Instant::now() + limit;
    let mut child = cmd.spawn().map_err(|e| format!("cannot start: {e}"))?;
    // Each pipe drains on its own thread, never joined: a `Match exec` grandchild may hold a
    // pipe open long after ssh is gone.
    let stdout = drain(child.stdout.take(), STDOUT_CAP + 1);
    let stderr = drain(child.stderr.take(), STDERR_CAP);

    // Poll ssh and its stdout together, so an over-cap read is refused at once instead of
    // leaving ssh blocked on a full pipe until the deadline.
    let mut early = None;
    let status = loop {
        if early.is_none()
            && let Ok(read) = stdout.try_recv()
        {
            early = Some(within_cap(&mut child, read)?);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                stop(&mut child);
                return Err(format!("cannot wait for it: {e}"));
            }
        }
        if Instant::now() >= deadline {
            stop(&mut child);
            return Err(format!("did not answer within {limit:?}"));
        }
        thread::sleep(Duration::from_millis(5));
    };
    // An ssh that exits just before the deadline still gets `PIPE_GRACE` for its readers;
    // only a pipe held open past both is a timeout.
    let collect = |rx: mpsc::Receiver<std::io::Result<Vec<u8>>>| {
        let remaining = deadline.saturating_duration_since(Instant::now());
        rx.recv_timeout(remaining.max(PIPE_GRACE))
            .map_err(|_| format!("did not answer within {limit:?}"))
    };
    if !status.success() {
        let bytes = collect(stderr)?.unwrap_or_default();
        return Err(format!("exited with {status}: {}", tail(&bytes)));
    }
    match early {
        Some(out) => Ok(out),
        None => within_cap(&mut child, collect(stdout)?),
    }
}

/// A finished stdout read, refused (and ssh stopped) when it failed or passed the cap.
fn within_cap(child: &mut Child, read: std::io::Result<Vec<u8>>) -> Result<Vec<u8>, String> {
    match read {
        Ok(bytes) if bytes.len() <= STDOUT_CAP => Ok(bytes),
        Ok(_) => {
            stop(child);
            Err("printed more than 256 KiB".to_owned())
        }
        Err(e) => {
            stop(child);
            Err(format!("cannot read its output: {e}"))
        }
    }
}

/// Kill ssh and reap it; its grandchildren are left alone.
fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Read up to `keep` bytes of `pipe` on a new thread and send them once, then discard the
/// rest so the writer never blocks on a full pipe.
fn drain<R: Read + Send + 'static>(
    pipe: Option<R>,
    keep: usize,
) -> mpsc::Receiver<std::io::Result<Vec<u8>>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let Some(mut pipe) = pipe else {
            let _ = tx.send(Ok(Vec::new()));
            return;
        };
        let mut bytes = Vec::new();
        let limit = u64::try_from(keep).unwrap_or(u64::MAX);
        let read = (&mut pipe)
            .take(limit)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = tx.send(read);
        let _ = std::io::copy(&mut pipe, &mut std::io::sink());
    });
    rx
}

/// The last [`STDERR_LINES`] lines of ssh's stderr, lossy and trimmed.
fn tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.trim().lines().collect();
    lines[lines.len().saturating_sub(STDERR_LINES)..].join("\n")
}

#[cfg(all(test, unix))]
mod tests {
    use super::{discover, run, valid_alias};
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    #[expect(
        clippy::unwrap_used,
        reason = "a fixture that cannot be written fails the test"
    )]
    fn script(dir: &Path, body: &str, mode: u32) -> PathBuf {
        let p = dir.join("ssh");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        p
    }

    /// `run`, once more when a sibling test's fork still held the fresh script open.
    fn run_fresh(p: &Path, cwd: &Path, limit: Duration) -> Result<Vec<u8>, String> {
        let args: [OsString; 0] = [];
        let first = run(p, &args, cwd, limit);
        if let Err(e) = &first
            && e.contains("Text file busy")
        {
            return run(p, &args, cwd, limit);
        }
        first
    }

    #[test]
    fn stdout_comes_back_with_stdin_closed_and_cwd_set() {
        let d = tempfile::tempdir().unwrap();
        let p = script(d.path(), "cat\npwd -P", 0o755);
        let out = run_fresh(&p, d.path(), Duration::from_secs(10)).unwrap();
        let want = format!("{}\n", d.path().canonicalize().unwrap().display());
        assert_eq!(
            String::from_utf8(out).unwrap(),
            want,
            "stdin is empty and the call runs in cwd"
        );
    }

    #[test]
    fn a_hung_ssh_is_stopped_at_the_limit() {
        let d = tempfile::tempdir().unwrap();
        let p = script(d.path(), "sleep 5", 0o755);
        let start = Instant::now();
        let e = run_fresh(&p, d.path(), Duration::from_millis(300)).unwrap_err();
        assert!(e.contains("did not answer"), "{e}");
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "stopped at the limit: {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn a_grandchild_holding_the_pipe_does_not_hold_the_call() {
        let d = tempfile::tempdir().unwrap();
        let p = script(d.path(), "sleep 5 &\necho hostname x", 0o755);
        let start = Instant::now();
        let r = run_fresh(&p, d.path(), Duration::from_millis(500));
        assert!(r.is_err(), "a pipe held past the limit is a timeout: {r:?}");
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "the drain is never joined: {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn oversized_output_is_refused_without_waiting_out_the_limit() {
        let d = tempfile::tempdir().unwrap();
        let p = script(d.path(), "head -c 400000 /dev/zero\nsleep 5", 0o755);
        let start = Instant::now();
        let e = run_fresh(&p, d.path(), Duration::from_secs(10)).unwrap_err();
        assert!(e.contains("printed more than 256 KiB"), "{e}");
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "refused at the cap: {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn a_failing_ssh_reports_its_exit_and_stderr_tail() {
        let d = tempfile::tempdir().unwrap();
        let p = script(
            d.path(),
            "for i in $(seq 1 30); do echo line$i >&2; done\nexit 255",
            0o755,
        );
        let e = run_fresh(&p, d.path(), Duration::from_secs(10)).unwrap_err();
        assert!(e.starts_with("exited with"), "{e}");
        assert!(
            e.contains("line30") && e.contains("line11"),
            "the last 20 lines are kept: {e}"
        );
        assert!(!e.contains("line10"), "older lines are dropped: {e}");
    }

    #[test]
    fn discovery_takes_the_first_absolute_executable_ssh() {
        // A relative entry that climbs from the cwd to the root and back down to a tempdir
        // holding a good ssh, so only the relative-skip rule can refuse it.
        let rel = tempfile::tempdir().unwrap();
        script(rel.path(), "", 0o755);
        let cwd = std::env::current_dir().unwrap();
        let rel_entry = PathBuf::from("../".repeat(cwd.components().count()))
            .join(rel.path().strip_prefix("/").unwrap());
        assert!(
            rel_entry.is_relative() && rel_entry.join("ssh").is_file(),
            "the fixture reaches the script"
        );
        let not_exec = tempfile::tempdir().unwrap();
        script(not_exec.path(), "", 0o644);
        let a_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(a_dir.path().join("ssh")).unwrap();
        let good = tempfile::tempdir().unwrap();
        let ssh = script(good.path(), "", 0o755);
        let path =
            std::env::join_paths([&rel_entry, not_exec.path(), a_dir.path(), good.path()]).unwrap();
        assert_eq!(
            discover(Some(&path)),
            Some(ssh),
            "relative, non-executable and directory entries are skipped"
        );
        let only_rel = std::env::join_paths([&rel_entry]).unwrap();
        assert_eq!(
            discover(Some(&only_rel)),
            None,
            "a relative PATH entry is never searched"
        );
        assert_eq!(discover(None), None, "no PATH, no ssh");
    }

    #[test]
    fn only_a_plain_host_name_or_alias_passes_the_alias_check() {
        for ok in ["jump", "db-1.example.com", "fe80::1", "host_x", "A9"] {
            assert!(valid_alias(ok), "{ok}");
        }
        for bad in [
            "",
            "ops@b",
            "ssh://ops@b:99",
            "-oProxyCommand=x",
            "-",
            "a b",
            "a/b",
            "a%h",
            "a\\b",
            "jump\n",
            "h\u{e9}",
        ] {
            assert!(!valid_alias(bad), "{bad:?}");
        }
    }
}
