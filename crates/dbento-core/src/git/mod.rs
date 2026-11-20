//! Team mode's git layer: one hardened runner for every git call, and reading what a Potluck
//! server says when it refuses a push.

mod clone;
mod commit;
mod lock;
mod push;
mod rebase;
mod refusal;
pub(crate) mod run;

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub(crate) use clone::set_up;
pub use clone::{CloneError, clone};
pub use commit::commit_pending;
pub(crate) use commit::ensure_on_branch;
pub use lock::{RepoLock, repo_lock};
pub use push::{PushOutcome, push, remote_tip, squash_unpushed, unpushed};
pub(crate) use rebase::fetch;
pub use rebase::{ConflictFile, ConflictKind, Pulled, pull_rebase};
pub use refusal::{Finding, Refusal, parse_refusal, redact_token, rule_text};

/// How a team sync calls git: extra environment, time limits and an optional cancel flag.
#[derive(Clone)]
#[non_exhaustive]
pub struct GitOptions {
    env: Vec<(OsString, OsString)>,
    timeout: Duration,
    network_timeout: Duration,
    lock_wait: Duration,
    cancel: Option<CancelFlag>,
}

// The env may carry a team token in an injected config value, so the debug form hides every
// config value and anything naming an auth header, and masks known tokens elsewhere.
impl fmt::Debug for GitOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let env: Vec<(&OsStr, String)> = self
            .env
            .iter()
            .map(|(key, value)| {
                let value = value.to_string_lossy();
                let hidden = key.to_string_lossy().starts_with("GIT_CONFIG_VALUE_")
                    || value.to_ascii_lowercase().contains("authorization");
                let shown = if hidden {
                    "<redacted>".to_owned()
                } else {
                    redact_token(&value)
                };
                (key.as_os_str(), shown)
            })
            .collect();
        f.debug_struct("GitOptions")
            .field("env", &env)
            .field("timeout", &self.timeout)
            .field("network_timeout", &self.network_timeout)
            .field("lock_wait", &self.lock_wait)
            .field("cancel", &self.cancel)
            .finish()
    }
}

impl Default for GitOptions {
    fn default() -> GitOptions {
        GitOptions {
            env: Vec::new(),
            timeout: Duration::from_secs(10),
            network_timeout: Duration::from_secs(60),
            lock_wait: Duration::from_secs(10),
            cancel: None,
        }
    }
}

impl GitOptions {
    /// No extra environment, a 10 s local limit, a 60 s network limit, a 10 s lock wait and
    /// no cancel flag.
    pub fn new() -> GitOptions {
        GitOptions::default()
    }

    /// Set `key` in git's environment; a later call for the same key wins. The variables the
    /// runner always fixes, such as `LC_ALL`, cannot be overridden.
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> GitOptions {
        self.env
            .push((key.as_ref().to_owned(), value.as_ref().to_owned()));
        self
    }

    /// Stop each local git call after `limit`.
    pub fn timeout(mut self, limit: Duration) -> GitOptions {
        self.timeout = limit;
        self
    }

    /// Stop each git call that talks to the server after `limit`.
    pub fn network_timeout(mut self, limit: Duration) -> GitOptions {
        self.network_timeout = limit;
        self
    }

    /// Wait at most `wait` for another sync to release the repository.
    pub fn lock_wait(mut self, wait: Duration) -> GitOptions {
        self.lock_wait = wait;
        self
    }

    /// Stop a running call as soon as `flag` is cancelled.
    pub fn cancel(mut self, flag: CancelFlag) -> GitOptions {
        self.cancel = Some(flag);
        self
    }

    /// Send `token` as a bearer header to `public_url` only, with stored credentials and
    /// redirects off and git's traces redacted. The token goes through the environment, never
    /// the argv, where other users could read it.
    pub fn team_env(self, public_url: &str, token: &str) -> GitOptions {
        let header_key = format!("http.{}/.extraHeader", public_url.trim_end_matches('/'));
        let header = format!("Authorization: Bearer {token}");
        [
            ("GIT_CONFIG_COUNT", "3"),
            ("GIT_CONFIG_KEY_0", header_key.as_str()),
            ("GIT_CONFIG_VALUE_0", header.as_str()),
            ("GIT_CONFIG_KEY_1", "credential.helper"),
            ("GIT_CONFIG_VALUE_1", ""),
            ("GIT_CONFIG_KEY_2", "http.followRedirects"),
            ("GIT_CONFIG_VALUE_2", "false"),
            ("GIT_TRACE_REDACT", "1"),
        ]
        .into_iter()
        .fold(self, |opts, (key, value)| opts.env(key, value))
    }
}

/// A flag shared between a caller and a running git call; setting it stops the call.
#[derive(Debug, Clone, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    /// A flag that is not set.
    pub fn new() -> CancelFlag {
        CancelFlag::default()
    }

    /// Set the flag; a running call that watches it stops as soon as it next looks.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether `cancel` has been called on this flag or any clone of it.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// The installed git's version, as `git --version` reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GitVersion {
    /// The first number, such as 2 in 2.56.1.
    pub major: u32,
    /// The second number; 0 when git printed none.
    pub minor: u32,
    /// The third number; 0 when git printed none.
    pub patch: u32,
    /// The whole first line git printed, such as `git version 2.39.3 (Apple Git-146)`.
    pub text: String,
}

/// Why one git call did not produce an output.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GitError {
    /// No `git` program on the `PATH` git was started with.
    #[error("git is not installed or not on PATH")]
    Missing,
    /// The call outlived this limit and was stopped.
    #[error("git took longer than {0:?}")]
    Timeout(Duration),
    /// The cancel flag was set while the call ran, and it was stopped.
    #[error("git was cancelled")]
    Cancelled,
    /// Git exited non-zero.
    #[error("{command} failed: {stderr}")]
    Failed {
        /// `git` and the arguments of the call, without the runner's fixed flags.
        command: String,
        /// The last 50 lines of git's stderr, trimmed, with tokens masked.
        stderr: String,
    },
    /// Git succeeded but printed something this client cannot read.
    #[error("unexpected git output: {0}")]
    Unexpected(String),
    /// Starting or talking to git failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// The commit message of a sync's save.
pub const SAVE_MESSAGE: &str = "Save changes";

/// Why a step of a team sync did not finish.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SyncError {
    /// A git call failed.
    #[error(transparent)]
    Git(#[from] GitError),
    /// The folder is not cleanly on the bound branch; nothing was written.
    #[error("workspace is not on {expected}")]
    WrongBranch {
        /// The bound branch.
        expected: String,
        /// The branch checked out; `None` when `HEAD` is detached or a rebase, merge or
        /// cherry-pick is under way.
        found: Option<String>,
    },
    /// Another sync held the repository lock for the whole wait.
    #[error("another Dbento is syncing this workspace; try again")]
    Busy {
        /// The lock file.
        lock: PathBuf,
    },
    /// The local branch is missing commits the server's branch has.
    #[error("the local branch does not contain origin/{branch}")]
    Diverged {
        /// The bound branch.
        branch: String,
    },
    /// The server's branch moved on every push attempt.
    #[error("the server kept moving; gave up after {pushes} pushes")]
    Raced {
        /// How many pushes were tried.
        pushes: u32,
    },
    /// Reading or writing a file failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// [`git_available_with`] with [`GitOptions::new`].
pub fn git_available() -> Result<GitVersion, GitError> {
    git_available_with(&GitOptions::new())
}

/// The version of the git that `opts`' environment finds; `Missing` when there is none.
pub fn git_available_with(opts: &GitOptions) -> Result<GitVersion, GitError> {
    let out = run::git_ok(
        &["--version"],
        &std::env::temp_dir(),
        run::Reach::Local,
        opts,
    )?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next().unwrap_or("").trim();
    parse_version(line).ok_or_else(|| GitError::Unexpected(line.to_owned()))
}

/// `git version X.Y.Z...` read as the leading digits of its first three dot-separated parts,
/// so vendor suffixes such as `.windows.1` or ` (Apple Git-146)` parse; a missing part is 0.
fn parse_version(line: &str) -> Option<GitVersion> {
    let rest = line.strip_prefix("git version ")?;
    let mut parts = rest.split('.').map(|part| {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        digits.parse::<u32>().ok()
    });
    let major = parts.next().flatten()?;
    let minor = parts.next().flatten().unwrap_or(0);
    let patch = parts.next().flatten().unwrap_or(0);
    Some(GitVersion {
        major,
        minor,
        patch,
        text: line.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::parse_version;

    #[test]
    fn vendor_versions_parse_and_other_text_does_not() {
        let triple = |l: &str| parse_version(l).map(|v| (v.major, v.minor, v.patch));
        assert_eq!(triple("git version 2.56.1"), Some((2, 56, 1)));
        assert_eq!(
            triple("git version 2.39.3 (Apple Git-146)"),
            Some((2, 39, 3))
        );
        assert_eq!(triple("git version 2.45.1.windows.1"), Some((2, 45, 1)));
        assert_eq!(triple("git version 3"), Some((3, 0, 0)));
        assert_eq!(triple("hub version 2.1"), None);
        assert_eq!(triple("git version x.1"), None);
    }
}
