#![cfg(unix)]

use std::fs;
use std::time::{Duration, Instant};

use dbento_core::{CancelFlag, GitError, GitOptions, git_available_with};

use crate::support::Sandbox;

/// A fake git that records its argv and environment, then reports a version.
fn recording(sb: &Sandbox) -> GitOptions {
    let body = "for a in \"$@\"; do printf '%s\\n' \"$a\"; done > \"$ARGS_OUT\"\n\
                env > \"$ENV_OUT\"\necho 'git version 2.56.1'";
    sb.options()
        .env("PATH", sb.fake_git("rec", body))
        .env("ARGS_OUT", sb.path().join("args"))
        .env("ENV_OUT", sb.path().join("env"))
}

fn lines(sb: &Sandbox, name: &str) -> Vec<String> {
    fs::read_to_string(sb.path().join(name))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn slow(sb: &Sandbox) -> GitOptions {
    sb.options()
        .env("PATH", sb.fake_git("slow", "exec sleep 30"))
}

#[test]
fn git_available_reads_the_real_version_and_misses_an_empty_path() {
    let sb = Sandbox::new();
    let v = git_available_with(&sb.options()).expect("git is installed");
    assert!(v.major >= 2 && v.text.starts_with("git version "), "{v:?}");
    let empty = sb.path().join("empty-bin");
    fs::create_dir(&empty).unwrap();
    let err = git_available_with(&sb.options().env("PATH", &empty)).unwrap_err();
    assert!(matches!(err, GitError::Missing), "{err:?}");
}

#[test]
fn every_call_carries_the_fixed_flags_and_an_empty_hooks_dir() {
    let sb = Sandbox::new();
    let v = git_available_with(&recording(&sb)).expect("fake version");
    assert_eq!((v.major, v.minor, v.patch), (2, 56, 1));
    let args = lines(&sb, "args");
    for flag in [
        "--no-optional-locks",
        "--literal-pathspecs",
        "core.fsmonitor=false",
        "protocol.ext.allow=never",
        "core.symlinks=false",
    ] {
        assert!(
            args.iter().any(|a| a == flag),
            "{flag} missing from {args:?}"
        );
    }
    assert_eq!(args.last().map(String::as_str), Some("--version"));
    let hooks = args
        .iter()
        .find_map(|a| a.strip_prefix("core.hooksPath="))
        .expect("pinned");
    assert_eq!(fs::read_dir(hooks).expect("hooks dir exists").count(), 0);
}

#[test]
fn the_environment_is_scrubbed_and_the_fixed_values_win() {
    let sb = Sandbox::new();
    let opts = recording(&sb)
        .env("GIT_DIR", "/elsewhere")
        .env("GIT_WORK_TREE", "/elsewhere")
        .env("GIT_INDEX_FILE", "/x")
        .env("GIT_CONFIG_PARAMETERS", "'core.hookspath'='/evil'")
        .env("LC_ALL", "de_DE.UTF-8")
        .env("GIT_TERMINAL_PROMPT", "1");
    git_available_with(&opts).expect("fake version");
    let env = lines(&sb, "env");
    for gone in [
        "GIT_DIR=",
        "GIT_WORK_TREE=",
        "GIT_INDEX_FILE=",
        "GIT_CONFIG_PARAMETERS=",
    ] {
        assert!(
            !env.iter().any(|l| l.starts_with(gone)),
            "{gone} reached git"
        );
    }
    for fixed in ["LC_ALL=C", "GIT_TERMINAL_PROMPT=0", "GIT_OPTIONAL_LOCKS=0"] {
        assert!(env.iter().any(|l| l == fixed), "{fixed} missing");
    }
}

#[test]
fn team_env_sends_the_token_as_a_header_and_debug_hides_it() {
    let sb = Sandbox::new();
    git_available_with(&recording(&sb).team_env("https://pl.example/", "plk_s3cret")).unwrap();
    let env = lines(&sb, "env");
    for want in [
        "GIT_CONFIG_COUNT=3",
        "GIT_CONFIG_KEY_0=http.https://pl.example/.extraHeader",
        "GIT_CONFIG_VALUE_0=Authorization: Bearer plk_s3cret",
        "GIT_CONFIG_KEY_1=credential.helper",
        "GIT_CONFIG_VALUE_1=",
        "GIT_CONFIG_KEY_2=http.followRedirects",
        "GIT_CONFIG_VALUE_2=false",
        "GIT_TRACE_REDACT=1",
    ] {
        assert!(env.iter().any(|l| l == want), "{want} missing");
    }
    assert!(
        !lines(&sb, "args").iter().any(|a| a.contains("plk_")),
        "token in argv"
    );
    let opts = GitOptions::new()
        .team_env("https://pl.example", "plk_s3cret")
        .env("NOTE", "saw plk_abc here")
        .env("X_AUTH", "Authorization: Basic Zm9v");
    let text = format!("{opts:?}");
    for leak in ["s3cret", "plk_abc", "Zm9v"] {
        assert!(!text.contains(leak), "{leak} leaked: {text}");
    }
    assert!(
        text.contains("<redacted>") && text.contains("plk_[redacted]"),
        "{text}"
    );
}

#[test]
fn a_call_past_its_limit_or_cancelled_is_stopped() {
    let sb = Sandbox::new();
    let begun = Instant::now();
    let err = git_available_with(&slow(&sb).timeout(Duration::from_millis(300))).unwrap_err();
    assert!(matches!(err, GitError::Timeout(_)), "{err:?}");
    let flag = CancelFlag::new();
    flag.cancel();
    let err = git_available_with(&slow(&sb).cancel(flag)).unwrap_err();
    assert!(matches!(err, GitError::Cancelled), "{err:?}");
    assert!(
        begun.elapsed() < Duration::from_secs(8),
        "both stopped within the grace"
    );
}

#[test]
fn failed_stderr_keeps_its_last_fifty_lines_with_tokens_masked() {
    let sb = Sandbox::new();
    let body =
        "i=1\nwhile [ $i -le 80 ]; do echo \"line $i plk_abcDEF123\" >&2; i=$((i+1)); done\nexit 1";
    let err = git_available_with(&sb.options().env("PATH", sb.fake_git("loud", body))).unwrap_err();
    let GitError::Failed { stderr, command } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(command, "git --version");
    let kept: Vec<&str> = stderr.lines().collect();
    assert_eq!((kept.len(), kept[0]), (50, "line 31 plk_[redacted]"));
    assert!(!err.to_string().contains("abcDEF123"), "{err}");
}
