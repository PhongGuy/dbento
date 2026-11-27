use crate::common;
use crate::common::{dbento, sqlite_ws};

fn json(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn run_takes_the_connection_from_the_queries_folder_or_conn() {
    let ws = sqlite_ws("CREATE TABLE t(a); INSERT INTO t VALUES (7)");
    for (rel, text) in [
        ("queries/lite/q.sql", "select a from t"),
        ("queries/loose.sql", "select a + 1 from t"),
    ] {
        let path = ws.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let out = dbento()
        .current_dir(ws.path().join("queries"))
        .args(["run", "lite/q.sql", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        json(&out)["statements"][0]["rows"],
        serde_json::json!([["7"]])
    );
    assert_eq!(json(&out)["connection"], "lite");
    let out = dbento()
        .current_dir(ws.path())
        .args(["run", "queries/loose.sql", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--conn"));
    let out = dbento()
        .current_dir(ws.path())
        .args([
            "run",
            "queries/loose.sql",
            "--conn",
            "lite",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert_eq!(
        json(&out)["statements"][0]["rows"],
        serde_json::json!([["8"]])
    );
}

#[test]
fn run_reports_a_missing_file_or_unknown_conn_as_exit_3() {
    let ws = sqlite_ws("");
    assert_eq!(
        dbento()
            .current_dir(ws.path())
            .args(["run", "nope.sql", "--conn", "lite"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(3)
    );
    std::fs::write(ws.path().join("x.sql"), "select 1").unwrap();
    assert_eq!(
        dbento()
            .current_dir(ws.path())
            .args(["run", "x.sql", "--conn", "other"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(3)
    );
}

const PG: &str = "dbento = 1\n[connections.envpg]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\nsecret = { env = \"DBENTO_SECRET_TEST_UNSET\" }\n[connections.lite]\nengine = \"sqlite\"\npath = \"data/app.db\"\n";

#[test]
fn secret_set_refuses_connections_without_a_keychain_secret() {
    let ws = common::write_tree(&[("dbento.toml", PG)]);
    for conn in ["lite", "envpg", "missing"] {
        let out = dbento()
            .current_dir(ws.path())
            .args(["secret", "set", conn])
            .write_stdin("pw\n")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(3), "{conn}");
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains("pw"),
            "{conn}"
        );
    }
}

#[test]
fn secret_set_takes_no_value_on_the_command_line() {
    let ws = common::write_tree(&[("dbento.toml", PG)]);
    let out = dbento()
        .current_dir(ws.path())
        .args(["secret", "set", "envpg", "hunter2"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let all = [out.stdout.clone(), out.stderr.clone()].concat();
    assert!(!String::from_utf8_lossy(&all).contains("hunter2"));
}

#[test]
fn query_resolves_the_secret_before_connecting() {
    let ws = common::write_tree(&[("dbento.toml", PG)]);
    let out = dbento()
        .current_dir(ws.path())
        .env_remove("DBENTO_SECRET_TEST_UNSET")
        .args(["query", "envpg", "select 1"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("DBENTO_SECRET_TEST_UNSET"));
}

#[cfg(unix)]
#[test]
fn run_binds_by_the_folder_given_not_by_a_symlink_elsewhere() {
    let toml = format!(
        "{}[connections.kc]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\nsecret = \"none\"\n",
        common::LITE_TOML
    );
    let ws = common::write_tree(&[
        ("dbento.toml", &toml),
        ("queries/lite/q.sql", "select 41 + 1"),
    ]);
    std::fs::create_dir_all(ws.path().join("queries/kc")).unwrap();
    std::os::unix::fs::symlink("../lite/q.sql", ws.path().join("queries/kc/alias.sql")).unwrap();
    std::fs::create_dir(ws.path().join("data")).unwrap();
    rusqlite::Connection::open(ws.path().join("data/app.db")).unwrap();
    let out = dbento()
        .current_dir(ws.path())
        .args(["run", "queries/lite/q.sql", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(json(&out)["connection"], "lite");
    assert_eq!(
        json(&out)["statements"][0]["rows"],
        serde_json::json!([["42"]])
    );
}

#[test]
fn secret_catch_all_applies_only_to_the_secret_subcommand() {
    let ws = sqlite_ws("");
    let out = dbento()
        .current_dir(ws.path())
        .args(["query", "secret", "select 1", "--bogus"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--bogus"));
    let out = dbento()
        .current_dir(ws.path())
        .args(["--bogus", "secret"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--bogus"));
}

#[test]
fn secret_clap_errors_stay_generic_even_after_a_workspace_flag() {
    let ws = sqlite_ws("");
    for args in [
        vec!["secret", "set", "--bogus=hunter2", "kc"],
        vec!["--workspace", ".", "secret", "set", "--bogus=hunter2", "kc"],
        vec!["--workspace=.", "secret", "set", "--bogus=hunter2", "kc"],
    ] {
        let out = dbento()
            .current_dir(ws.path())
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(3), "{args:?}");
        let all = [out.stdout, out.stderr].concat();
        let all = String::from_utf8_lossy(&all);
        assert!(
            !all.contains("hunter2") && all.contains("invalid arguments"),
            "{args:?} {all}"
        );
    }
}

#[test]
fn secret_set_and_delete_never_echo_an_unknown_name() {
    let ws = common::write_tree(&[("dbento.toml", PG)]);
    for action in ["set", "delete"] {
        let out = dbento()
            .current_dir(ws.path())
            .args(["secret", action, "hunter2"])
            .write_stdin("pw\n")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(3), "{action}");
        let all = [out.stdout, out.stderr].concat();
        assert!(
            !String::from_utf8_lossy(&all).contains("hunter2"),
            "{action}"
        );
    }
}

#[test]
fn secret_set_names_the_env_variable_or_sqlite_in_its_refusal() {
    let ws = common::write_tree(&[("dbento.toml", PG)]);
    let refusal = |conn: &str| {
        let out = dbento()
            .current_dir(ws.path())
            .args(["secret", "set", conn])
            .write_stdin("pw\n")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    assert!(refusal("envpg").contains("$DBENTO_SECRET_TEST_UNSET"));
    assert!(
        refusal("lite").contains("has no secret: a sqlite connection opens without one"),
        "{}",
        refusal("lite")
    );
}

#[test]
fn secret_set_and_delete_refuse_a_child_before_reading_input() {
    let ws = common::write_tree(&[(
        "dbento.toml",
        "dbento = 1\n[connections.kc]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\ndatabases = [\"app\"]\n",
    )]);
    for action in ["set", "delete"] {
        let out = dbento()
            .current_dir(ws.path())
            .env("DBENTO_TEST_NO_KEYCHAIN", "1")
            .args(["secret", action, "kc/app"])
            .write_stdin("pw\n")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(3), "{action}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("no such connection in dbento.toml"),
            "{action}: {err}"
        );
    }
}

const TUNNELLED: &str = "dbento = 1\n[connections.tun]\nengine = \"postgres\"\nhost = \"db\"\ndatabase = \"d\"\nuser = \"u\"\n[connections.tun.ssh]\nhost = \"bastion\"\n[connections.kc]\nengine = \"postgres\"\nhost = \"h\"\ndatabase = \"d\"\nuser = \"u\"\n";

/// Runs `dbento` in a workspace holding `toml`, with no keychain and an empty SSH home.
fn run_in(toml: &str, args: &[&str]) -> std::process::Output {
    let ws = common::write_tree(&[("dbento.toml", toml)]);
    let home = tempfile::tempdir().unwrap();
    dbento()
        .current_dir(ws.path())
        .env("DBENTO_TEST_NO_KEYCHAIN", "1")
        .env("DBENTO_TEST_SSH_HOME", home.path())
        .args(args)
        .write_stdin("pw\n")
        .output()
        .unwrap()
}

#[test]
fn the_ssh_flags_reach_the_keychain_before_or_after_the_name() {
    for args in [
        &["secret", "set", "--ssh-password", "tun"][..],
        &["secret", "set", "tun", "--ssh-password"],
        &["secret", "set", "--ssh-key-passphrase", "tun"],
        &["secret", "set", "tun", "--ssh-key-passphrase"],
        &["secret", "delete", "--ssh-password", "tun"],
        &["secret", "delete", "--ssh-key-passphrase", "tun"],
    ] {
        let out = run_in(TUNNELLED, args);
        assert_eq!(out.status.code(), Some(3), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("disabled by DBENTO_TEST_NO_KEYCHAIN"),
            "{args:?}: {err}"
        );
    }
}

#[test]
fn an_ssh_flag_on_a_connection_without_ssh_is_refused() {
    for action in ["set", "delete"] {
        let out = run_in(TUNNELLED, &["secret", action, "kc", "--ssh-password"]);
        assert_eq!(out.status.code(), Some(3), "{action}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("has no ssh table"), "{action}: {err}");
    }
}

#[test]
fn two_secret_flags_at_once_are_refused() {
    for args in [
        &["secret", "set", "--tls-key", "--ssh-password", "tun"][..],
        &[
            "secret",
            "set",
            "--ssh-password",
            "--ssh-key-passphrase",
            "tun",
        ],
        &["secret", "set", "--ssh-password", "tun", "--tls-key"],
    ] {
        let out = run_in(TUNNELLED, args);
        assert!(!out.status.success(), "{args:?}");
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains("disabled by DBENTO_TEST_NO_KEYCHAIN"),
            "{args:?} reached the keychain"
        );
    }
}

#[test]
fn an_absolute_identity_file_outside_the_home_warns() {
    let toml = TUNNELLED.replace(
        "host = \"bastion\"\n",
        "host = \"bastion\"\nidentity_file = \"/opt/keys/id\"\n",
    );
    let out = run_in(&toml, &["connections"]);
    assert_eq!(out.status.code(), Some(0));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("warning: ") && err.contains("/opt/keys/id"),
        "{err}"
    );
}

#[test]
fn a_refused_env_name_is_never_sent_or_printed() {
    let toml = "dbento = 1\n[connections.envpg]\nengine = \"postgres\"\nhost = \"127.0.0.1\"\nport = 9\ndatabase = \"d\"\nuser = \"u\"\nsecret = { env = \"PGPASSWORD\" }\n";
    let ws = common::write_tree(&[("dbento.toml", toml)]);
    let out = dbento()
        .current_dir(ws.path())
        .env("PGPASSWORD", "leak-value-x")
        .args(["query", "envpg", "select 1"])
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("DBENTO_SECRET_ENVPG"), "{err}");
    assert!(
        !err.contains("PGPASSWORD") && !err.contains("leak-value-x"),
        "{err}"
    );
    let set = dbento()
        .current_dir(ws.path())
        .args(["secret", "set", "envpg"])
        .write_stdin("pw\n")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&set.stderr);
    assert_eq!(set.status.code(), Some(3), "{err}");
    assert!(!err.contains("PGPASSWORD"), "{err}");
}
