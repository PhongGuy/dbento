//! The yes-or-no prompt asked before an unguarded statement runs: an `UPDATE` or `DELETE`
//! without `WHERE`, a `DROP` or a `TRUNCATE`.

use std::io::{self, BufRead, IsTerminal, Write};

use dbento_core::Unguarded;

use crate::interrupt::Interrupt;

/// The phrase for the prompt and refusal, and the JSON `reason`, for an unguarded write.
pub(crate) fn unguarded_words(kind: Unguarded) -> (&'static str, &'static str) {
    let reason = match kind {
        Unguarded::DeleteWithoutWhere => "delete_without_where",
        Unguarded::UpdateWithoutWhere => "update_without_where",
        Unguarded::Drop => "drop",
        Unguarded::Truncate => "truncate",
        _ => "unguarded_write",
    };
    (kind.label(), reason)
}

/// Whether stdin is a person at a terminal. Debug builds also accept the test seam
/// `DBENTO_TEST_ASSUME_TTY`, so integration tests can answer through a pipe.
fn stdin_is_terminal() -> bool {
    io::stdin().is_terminal()
        || (cfg!(debug_assertions) && std::env::var_os("DBENTO_TEST_ASSUME_TTY").is_some())
}

/// True when `yes` is set, or a terminal answers `y` or `yes` to `question` on stderr.
/// Without a terminal, on any other answer, a read failure or a Ctrl-C, the answer is no.
pub(crate) async fn confirm(question: &str, yes: bool, interrupt: &Interrupt) -> bool {
    if yes {
        return true;
    }
    if !stdin_is_terminal() {
        return false;
    }
    let mut stderr = io::stderr();
    if write!(stderr, "{question} [y/N] ")
        .and_then(|()| stderr.flush())
        .is_err()
    {
        return false;
    }
    // The read blocks a pool thread that a Ctrl-C cannot wake, so the press wins the race
    // and the read is abandoned; `main` shuts the runtime down without waiting for it.
    let read = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        io::stdin().lock().read_line(&mut line).map(|_| line)
    });
    let line = tokio::select! {
        read = read => match read {
            Ok(Ok(line)) => line,
            _ => return false,
        },
        () = interrupt.wait() => {
            eprintln!();
            return false;
        }
    };
    let answer = line.trim().to_ascii_lowercase();
    answer == "y" || answer == "yes"
}
