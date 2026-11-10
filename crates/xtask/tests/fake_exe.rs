//! Stand-in executables for tests that run a fake tool by path.
#![cfg(unix)]

use std::path::Path;

/// Write `script` to `path` with mode 0755, ready to execute.
pub fn write_executable(path: &Path, script: &str) {
    // A child shell does the writing, so this multi-threaded process never holds a write fd
    // a concurrent fork could inherit and turn the exec into "Text file busy".
    let status = std::process::Command::new("sh")
        .args([
            "-c",
            r#"printf %s "$1" >"$2" && chmod 755 "$2""#,
            "sh",
            script,
        ])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success(), "writing {}: {status}", path.display());
}

// Other test threads fork while this one writes; a script must still run the moment it exists.
#[test]
fn a_fresh_script_runs_while_other_threads_spawn() {
    let workers: Vec<_> = (0..8)
        .map(|_| {
            std::thread::spawn(|| {
                let dir = tempfile::tempdir().unwrap();
                for i in 0..100 {
                    let path = dir.path().join(format!("fake-{i}"));
                    write_executable(&path, "#!/bin/sh\nexit 0\n");
                    let status = std::process::Command::new(&path).status();
                    assert!(status.as_ref().is_ok_and(|s| s.success()), "{status:?}");
                }
            })
        })
        .collect();
    for w in workers {
        w.join().unwrap();
    }
}
