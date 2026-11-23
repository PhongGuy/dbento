//! The optional `[connections.<name>.ssh]` table: parsing, load-time refusals and binding.
use dbento_workspace::{ConnectionConfig, OpenError, SshAuth, Workspace};
use std::path::{Path, PathBuf};

fn open(text: &str) -> Result<Workspace, OpenError> {
    let text = format!("dbento = 1\n{text}");
    Workspace::from_text(PathBuf::from("/w"), Path::new("/w/dbento.toml"), &text)
}

fn conn(text: &str) -> ConnectionConfig {
    open(text)
        .unwrap_or_else(|e| panic!("{text}: {e}"))
        .declared("c")
        .unwrap()
        .clone()
}

fn refusal(text: &str) -> String {
    match open(text) {
        Ok(_) => panic!("accepted: {text}"),
        Err(e) => e.to_string(),
    }
}

const PG: &str = "[connections.c]\nengine = \"postgres\"\nhost = \"db.internal\"\ndatabase = \"d\"\nuser = \"u\"\n";
const SSH: &str = "[connections.c.ssh]\nhost = \"bastion\"\n";
const FULL: &str = "[connections.c.ssh]\nhost = \"jump\"\nport = 2222\nuser = \"ops\"\nauth = \"key\"\nidentity_file = \"k\"\n";

#[test]
fn every_ssh_key_is_read() {
    let c = conn(&format!(
        "{PG}[connections.c.ssh]\nhost = \"jump\"\nport = 2222\nuser = \"ops\"\nauth = \"key\"\nidentity_file = \"~/.ssh/id_jump\"\nkeepalive = false\n"
    ));
    let ssh = c.ssh().expect("an ssh table");
    assert_eq!(ssh.host(), "jump");
    assert_eq!(ssh.port(), Some(2222));
    assert_eq!(ssh.user(), Some("ops"));
    assert_eq!(ssh.auth(), Some(SshAuth::Key));
    assert_eq!(ssh.identity_file(), Some("~/.ssh/id_jump"));
    assert!(!ssh.keepalive());
}

#[test]
fn unset_ssh_keys_are_left_to_the_ssh_config() {
    let c = conn(&format!("{PG}{SSH}"));
    let ssh = c.ssh().unwrap();
    assert_eq!(
        (ssh.port(), ssh.user(), ssh.auth(), ssh.identity_file()),
        (None, None, None, None)
    );
    assert!(ssh.keepalive(), "keepalive defaults to on");
    assert!(conn(PG).ssh().is_none());
}

#[test]
fn the_auth_words_parse_and_others_are_refused() {
    for (word, auth) in [
        ("agent", SshAuth::Agent),
        ("key", SshAuth::Key),
        ("password", SshAuth::Password),
    ] {
        let c = conn(&format!("{PG}{SSH}auth = \"{word}\"\n"));
        assert_eq!(c.ssh().unwrap().auth(), Some(auth), "{word}");
        assert_eq!(auth.name(), word);
    }
    let e = refusal(&format!("{PG}{SSH}auth = \"pubkey\"\n"));
    assert!(
        e.contains("auth `pubkey` must be \"agent\", \"key\" or \"password\""),
        "{e}"
    );
}

#[test]
fn the_tunnelled_engines_take_an_ssh_table() {
    for body in [
        PG,
        "[connections.c]\nengine = \"mysql\"\nhost = \"db\"\nuser = \"u\"\n",
        "[connections.c]\nengine = \"mariadb\"\nhost = \"db\"\nuser = \"u\"\n",
        "[connections.c]\nengine = \"mssql\"\nhost = \"db\"\nuser = \"u\"\n",
        "[connections.c]\nengine = \"surrealdb\"\nhost = \"db\"\nnamespace = \"n\"\ndatabase = \"d\"\n",
        "[connections.c]\nengine = \"redis\"\nhost = \"cache.internal\"\n",
        "[connections.c]\nengine = \"redis\"\nhost = \"cache\"\ntls = \"off\"\n",
    ] {
        assert!(conn(&format!("{body}{SSH}")).ssh().is_some(), "{body}");
    }
}

#[test]
fn mongodb_dynamodb_and_file_engines_refuse_an_ssh_table() {
    for (engine, body) in [
        (
            "mongodb",
            "[connections.c]\nengine = \"mongodb\"\nuri = \"mongodb://app@db:27017/shop\"\n",
        ),
        (
            "dynamodb",
            "[connections.c]\nengine = \"dynamodb\"\nregion = \"eu-west-1\"\n",
        ),
        (
            "sqlite",
            "[connections.c]\nengine = \"sqlite\"\npath = \"a.db\"\n",
        ),
        (
            "duckdb",
            "[connections.c]\nengine = \"duckdb\"\npath = \"a.duckdb\"\n",
        ),
    ] {
        let e = refusal(&format!("{body}{SSH}"));
        assert!(
            e.contains(&format!("`ssh` is not valid for {engine}")),
            "{e}"
        );
    }
}

#[test]
fn a_postgres_socket_cannot_be_tunnelled() {
    let e = refusal(&format!(
        "[connections.c]\nengine = \"postgres\"\nhost = \"/var/run/postgresql\"\ndatabase = \"d\"\nuser = \"u\"\n{SSH}"
    ));
    assert!(
        e.contains("a postgres socket cannot be reached over SSH"),
        "{e}"
    );
}

#[test]
fn identity_file_goes_only_with_key_auth_or_none() {
    for auth in ["agent", "password"] {
        let e = refusal(&format!(
            "{PG}{SSH}auth = \"{auth}\"\nidentity_file = \"k\"\n"
        ));
        assert!(
            e.contains("`identity_file` needs auth = \"key\" or no auth"),
            "{auth}: {e}"
        );
    }
    for auth in ["auth = \"key\"\n", ""] {
        let c = conn(&format!("{PG}{SSH}{auth}identity_file = \"k\"\n"));
        assert_eq!(c.ssh().unwrap().identity_file(), Some("k"));
    }
}

#[test]
fn an_empty_or_missing_ssh_host_is_refused() {
    for host in ["", "  "] {
        let e = refusal(&format!("{PG}[connections.c.ssh]\nhost = \"{host}\"\n"));
        assert!(e.contains("`ssh.host` must not be empty"), "{e}");
    }
    let e = refusal(&format!("{PG}[connections.c.ssh]\nport = 22\n"));
    assert!(e.contains("host"), "a missing host is named: {e}");
}

#[test]
fn ssh_port_zero_is_refused() {
    let e = refusal(&format!("{PG}{SSH}port = 0\n"));
    assert!(e.contains("`ssh.port` must be between 1 and 65535"), "{e}");
}

#[test]
fn unknown_keys_and_passwords_in_the_ssh_table_are_refused() {
    let e = refusal(&format!("{PG}{SSH}hostname = \"x\"\n"));
    assert!(e.contains("unknown field `hostname`"), "{e}");
    assert!(matches!(
        open(&format!("{PG}{SSH}password = \"hunter2\"\n")),
        Err(OpenError::Password { .. })
    ));
}

#[test]
fn a_derived_child_keeps_its_parents_tunnel() {
    let ws = open(&format!("{PG}databases = [\"app\"]\n{SSH}")).unwrap();
    assert_eq!(
        ws.connection("c/app").unwrap().ssh().unwrap().host(),
        "bastion"
    );
}

#[test]
fn a_connection_without_ssh_binds_as_before() {
    // The SQLite value is copied from `the_sqlite_binding_is_pinned` in tests/session_binding.rs.
    assert_eq!(
        conn("[connections.c]\nengine = \"sqlite\"\npath = \"a.db\"\n").session_binding(),
        r#"{"b":1,"engine":"sqlite","read_only":false,"statement_timeout_ns":null,"secret_source":"none","file_path":["/","w","a.db"],"host":null,"port":null,"database":null,"user":null,"tls":null,"tls_ca":null,"tls_cert":null,"tls_key":null,"settings":{},"read_only_user":null,"uri":null,"namespace":null,"auth_level":null,"region":null,"endpoint":null,"access_key_id":null,"consistent_read":false}"#
    );
    assert_eq!(
        conn(PG).session_binding(),
        r#"{"b":1,"engine":"postgres","read_only":false,"statement_timeout_ns":null,"secret_source":"keychain","file_path":null,"host":"db.internal","port":5432,"database":"d","user":"u","tls":"verify","tls_ca":null,"tls_cert":null,"tls_key":null,"settings":{},"read_only_user":null,"uri":null,"namespace":null,"auth_level":null,"region":null,"endpoint":null,"access_key_id":null,"consistent_read":false}"#
    );
}

#[test]
fn the_ssh_table_is_appended_to_the_binding() {
    let without = conn(PG).session_binding();
    let with = conn(&format!(
        "{PG}[connections.c.ssh]\nhost = \"jump\"\nport = 2222\nuser = \"ops\"\nauth = \"key\"\nidentity_file = \"~/.ssh/id_jump\"\nkeepalive = false\n"
    ))
    .session_binding();
    let head = without.strip_suffix('}').unwrap();
    assert_eq!(
        with,
        format!(
            "{head},\"ssh\":{{\"host\":\"jump\",\"port\":2222,\"user\":\"ops\",\"auth\":\"key\",\"identity_file\":\"~/.ssh/id_jump\",\"keepalive\":false}}}}"
        )
    );
}

#[test]
fn every_ssh_key_changes_the_session_and_is_listed_as_ssh() {
    let base = conn(&format!("{PG}{FULL}"));
    let cases = [
        ("host", FULL.replace("\"jump\"", "\"jump2\""), false),
        ("port", FULL.replace("2222", "2223"), false),
        ("user", FULL.replace("\"ops\"", "\"dev\""), true),
        ("auth", FULL.replace("auth = \"key\"\n", ""), true),
        ("identity_file", FULL.replace("\"k\"", "\"k2\""), true),
        ("keepalive", format!("{FULL}keepalive = false\n"), true),
    ];
    for (label, table, login) in cases {
        let other = conn(&format!("{PG}{table}"));
        assert!(!base.same_session(&other), "{label}");
        assert_ne!(base.session_binding(), other.session_binding(), "{label}");
        assert_eq!(base.differing_keys(&other), vec!["ssh"], "{label}");
        assert_eq!(base.same_login(&other), login, "{label}");
    }
}

#[test]
fn adding_or_removing_the_tunnel_is_another_login() {
    let (plain, tunnelled) = (conn(PG), conn(&format!("{PG}{SSH}")));
    assert!(!plain.same_session(&tunnelled));
    assert!(!plain.same_login(&tunnelled));
    assert!(!tunnelled.same_login(&plain));
    assert_eq!(plain.differing_keys(&tunnelled), vec!["ssh"]);
    assert!(tunnelled.same_session(&conn(&format!("{PG}{SSH}"))));
}
