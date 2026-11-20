//! One git call: fixed flags and environment, a time limit, a cancel flag, and the raw result.

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use super::{GitError, GitOptions, redact_token};

/// Flags that keep every call from taking optional locks, reading pathspecs as globs, starting
/// an fsmonitor daemon, running an `ext::` helper or writing symlinks, whatever the config says.
/// `core.hooksPath` joins them per process, pointing at [`no_hooks`].
const PREFIX: [&str; 8] = [
    "--no-optional-locks",
    "--literal-pathspecs",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "protocol.ext.allow=never",
    "-c",
    "core.symlinks=false",
];

/// Variables that would point discovery away from the workspace or inject config.
const SCRUBBED: [&str; 9] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
    "GIT_CONFIG_PARAMETERS",
];

/// Set last, so neither the caller's environment nor `GitOptions::env` can override them.
const FIXED: [(&str, &str); 4] = [
    ("GIT_OPTIONAL_LOCKS", "0"),
    ("GIT_NO_LAZY_FETCH", "1"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("LC_ALL", "C"),
];

/// The most stderr lines a call keeps: enough for a full page of Potluck findings.
const STDERR_LINES: usize = 50;

/// The least time the pipe readers get after git exits.
const PIPE_GRACE: Duration = Duration::from_millis(250);

/// How long a stopped git and the processes it started get to exit after SIGTERM.
#[cfg(unix)]
const TERM_GRACE: Duration = Duration::from_secs(2);

/// Which limit a call runs under.
pub(crate) enum Reach {
    /// Only the repository on disk: [`GitOptions::timeout`].
    Local,
    /// Talks to the server: [`GitOptions::network_timeout`].
    #[expect(dead_code, reason = "fetch, push and clone use it")]
    Network,
}

/// What a finished git call produced.
pub(crate) struct Output {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    /// Lossy, trimmed, cut to its last [`STDERR_LINES`] lines, tokens masked.
    pub(crate) stderr: String,
}

/// Run `git <PREFIX> <args>` in `cwd` and return whatever it exited with.
pub(crate) fn git<S: AsRef<OsStr>>(
    args: &[S],
    cwd: &Path,
    reach: Reach,
    opts: &GitOptions,
) -> Result<Output, GitError> {
    let limit = match reach {
        Reach::Local => opts.timeout,
        Reach::Network => opts.network_timeout,
    };
    let mut argv: Vec<OsString> = PREFIX.iter().map(OsString::from).collect();
    let mut hooks = OsString::from("core.hooksPath=");
    hooks.push(no_hooks()?);
    argv.extend([OsString::from("-c"), hooks]);
    argv.extend(args.iter().map(|a| a.as_ref().to_owned()));
    spawn(&argv, cwd, limit, opts)
}

/// Like [`git`], with a non-zero exit turned into `Failed`.
pub(crate) fn git_ok<S: AsRef<OsStr>>(
    args: &[S],
    cwd: &Path,
    reach: Reach,
    opts: &GitOptions,
) -> Result<Output, GitError> {
    let out = git(args, cwd, reach, opts)?;
    if out.status.success() {
        Ok(out)
    } else {
        Err(GitError::Failed {
            command: command_line(args),
            stderr: out.stderr,
        })
    }
}

/// `git` followed by `args`, for error messages.
pub(crate) fn command_line<S: AsRef<OsStr>>(args: &[S]) -> String {
    let mut line = String::from("git");
    for arg in args {
        line.push(' ');
        line.push_str(&arg.as_ref().to_string_lossy());
    }
    line
}

/// An empty private folder for `core.hooksPath`, made once per process, so no hook a clone
/// planted (or a repo config points at) ever runs on this machine.
fn no_hooks() -> Result<&'static Path, GitError> {
    static NO_HOOKS: OnceLock<TempDir> = OnceLock::new();
    if let Some(dir) = NO_HOOKS.get() {
        return Ok(dir.path());
    }
    let made = tempfile::Builder::new()
        .prefix("dbento-no-hooks")
        .tempdir()?;
    // A racing thread's folder may win; the loser's is dropped and deleted.
    Ok(NO_HOOKS.get_or_init(|| made).path())
}

/// Spawn git with the built `argv` under the fixed environment, then poll it until it exits,
/// the limit passes or the cancel flag is set; the last two stop its whole process group.
fn spawn(
    argv: &[OsString],
    cwd: &Path,
    limit: Duration,
    opts: &GitOptions,
) -> Result<Output, GitError> {
    // A missing cwd also spawns as NotFound; it must not read as git being absent.
    if !cwd.is_dir() {
        return Err(GitError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{} is not a folder", cwd.display()),
        )));
    }
    let mut cmd = Command::new("git");
    cmd.args(argv)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Env, then scrub, then fixed: setting a key after its removal would undo the scrub.
    for (key, value) in &opts.env {
        cmd.env(key, value);
    }
    for name in SCRUBBED {
        cmd.env_remove(name);
    }
    cmd.envs(FIXED);
    // Git leads its own group, so a stop reaches its helpers too.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);

    let deadline = Instant::now() + limit;
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(GitError::Missing),
        Err(e) => return Err(e.into()),
    };
    // Each pipe drains on its own thread so a full pipe never stalls git; the threads are
    // never joined, because a grandchild may keep a pipe open long after git is gone.
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if opts.cancel.as_ref().is_some_and(|c| c.is_cancelled()) {
            stop_group(&mut child);
            return Err(GitError::Cancelled);
        }
        if Instant::now() >= deadline {
            stop_group(&mut child);
            return Err(GitError::Timeout(limit));
        }
        thread::sleep(Duration::from_millis(5));
    };
    // A git that exits just before the deadline still gets `PIPE_GRACE` for its readers;
    // only a pipe held open past both (by a grandchild) is a timeout.
    let collect = |rx: mpsc::Receiver<std::io::Result<Vec<u8>>>| {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining.max(PIPE_GRACE)) {
            Ok(bytes) => bytes.map_err(GitError::from),
            Err(_) => Err(GitError::Timeout(limit)),
        }
    };
    let stdout = collect(stdout)?;
    let stderr = collect(stderr)?;
    Ok(Output {
        status,
        stdout,
        stderr: tidy_stderr(&stderr),
    })
}

/// Git's stderr as text fit to show or log: trimmed, its last [`STDERR_LINES`] lines, tokens masked.
fn tidy_stderr(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.trim().lines().collect();
    let kept = &lines[lines.len().saturating_sub(STDERR_LINES)..];
    redact_token(&kept.join("\n"))
}

/// Read `pipe` to its end on a new thread and send the bytes once.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> mpsc::Receiver<std::io::Result<Vec<u8>>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = match pipe {
            Some(mut pipe) => pipe.read_to_end(&mut bytes).map(|_| bytes),
            None => Ok(bytes),
        };
        let _ = tx.send(result);
    });
    rx
}

/// Stop `child` and every process it started: SIGTERM to its group so git can remove its
/// locks, then SIGKILL to whatever outlives `TERM_GRACE`. Reaps the child.
#[cfg(unix)]
fn stop_group(child: &mut Child) {
    if signal_group(child, "-TERM") {
        let deadline = Instant::now() + TERM_GRACE;
        while Instant::now() < deadline {
            match child.try_wait() {
                // Helpers share the grace: wait for the group to empty, then KILL any member
                // that ignored TERM (an emptied group's ESRCH is harmless).
                Ok(Some(_)) => {
                    while Instant::now() < deadline && signal_group(child, "-0") {
                        thread::sleep(Duration::from_millis(20));
                    }
                    signal_group(child, "-KILL");
                    return;
                }
                Ok(None) => thread::sleep(Duration::from_millis(5)),
                Err(_) => break,
            }
        }
        // Git is still unreaped here, so its pid still names the group it leads.
        if !signal_group(child, "-KILL") {
            let _ = child.kill();
        }
        let _ = child.wait();
        return;
    }
    // Without the `kill` helper git is killed at once.
    let _ = child.kill();
    let _ = child.wait();
}

/// Elsewhere there are no process groups to signal, so only git itself is killed.
#[cfg(not(unix))]
fn stop_group(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Send `signal` to the process group `child` leads, through the `kill` helper (the crate
/// forbids the unsafe a direct `killpg` would need).
#[cfg(unix)]
fn signal_group(child: &Child, signal: &str) -> bool {
    Command::new("kill")
        .args([signal, "--", &format!("-{}", child.id())])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}
