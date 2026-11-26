//! The secret pin: the fields that tie a saved password to its server, normalised.
use dbento_workspace::{ConnectionConfig, Workspace};
use std::path::{Path, PathBuf};

fn open(text: &str) -> Workspace {
    let text = format!("dbento = 1\n{text}");
    Workspace::from_text(PathBuf::from("/w"), Path::new("/w/dbento.toml"), &text)
        .unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn conn(body: &str) -> ConnectionConfig {
    open(&format!("[connections.c]\n{body}"))
        .declared("c")
        .unwrap()
        .clone()
}

fn pin(body: &str) -> String {
    conn(body).secret_pin().as_str().to_owned()
}

fn pg(host: &str) -> String {
    format!("engine = \"postgres\"\nhost = \"{host}\"\ndatabase = \"d\"\nuser = \"ada\"\n")
}

fn server(engine: &str) -> String {
    format!("engine = \"{engine}\"\nhost = \"db.example\"\nuser = \"ada\"\n")
}

const SR: &str = "engine = \"surrealdb\"\nhost = \"db.example\"\nnamespace = \"n\"\ndatabase = \"d\"\nuser = \"ada\"\nauth_level = \"root\"\n";
const MG: &str =
    "engine = \"mongodb\"\nuri = \"mongodb://app@db.example:27017/shop?authSource=admin\"\n";
const DY: &str = "engine = \"dynamodb\"\nregion = \"eu-west-1\"\nendpoint = \"http://127.0.0.1:5000\"\naccess_key_id = \"AKIDTEST\"\nsecret = \"keychain\"\n";
const S3: &str = "engine = \"s3\"\nregion = \"auto\"\nendpoint = \"http://127.0.0.1:5000\"\naccess_key_id = \"AKIDTEST\"\nsecret = \"keychain\"\n";

#[test]
fn each_engine_pins_its_target_with_defaults_filled() {
    let cases: Vec<(String, &str)> = vec![
        (
            pg("db.example"),
            r#"{"v":1,"engine":"postgres","user":"ada","host":"db.example","port":5432}"#,
        ),
        (
            server("mysql"),
            r#"{"v":1,"engine":"mysql","user":"ada","host":"db.example","port":3306}"#,
        ),
        (
            server("mariadb"),
            r#"{"v":1,"engine":"mysql","user":"ada","host":"db.example","port":3306}"#,
        ),
        (
            server("mssql"),
            r#"{"v":1,"engine":"mssql","user":"ada","host":"db.example","port":1433}"#,
        ),
        (
            server("redis"),
            r#"{"v":1,"engine":"redis","user":"ada","host":"db.example","port":6379}"#,
        ),
        (
            SR.to_owned(),
            r#"{"v":1,"engine":"surrealdb","user":"ada","host":"db.example","port":8000}"#,
        ),
        (
            MG.to_owned(),
            r#"{"v":1,"engine":"mongodb","uri":"mongodb://app@db.example:27017/shop?authSource=admin"}"#,
        ),
        (
            DY.to_owned(),
            r#"{"v":1,"engine":"dynamodb","access_key_id":"AKIDTEST","region":"eu-west-1","endpoint":"http://127.0.0.1:5000"}"#,
        ),
        (
            S3.to_owned(),
            r#"{"v":1,"engine":"s3","access_key_id":"AKIDTEST","region":"auto","endpoint":"http://127.0.0.1:5000"}"#,
        ),
    ];
    for (body, want) in cases {
        assert_eq!(pin(&body), want, "{body}");
    }
}

#[test]
fn written_defaults_and_an_endpoint_slash_are_the_same_pin() {
    let pg = pg("db.example");
    assert_eq!(
        pin(&pg),
        pin(&format!("{pg}port = 5432\n")),
        "postgres port"
    );
    let my = server("mysql");
    assert_eq!(pin(&my), pin(&format!("{my}port = 3306\n")), "mysql port");
    let slash = DY.replace("5000\"", "5000/\"");
    assert_eq!(pin(DY), pin(&slash), "a trailing endpoint slash");
    let no_region = pin(&DY.replace("region = \"eu-west-1\"\n", ""));
    assert!(no_region.contains(r#""region":null"#), "{no_region}");
}

#[test]
fn file_engines_pin_their_resolved_file() {
    let lite = pin("engine = \"sqlite\"\npath = \"a.db\"\n");
    assert!(
        lite.starts_with(r#"{"v":1,"engine":"sqlite","file_path":""#),
        "{lite}"
    );
    assert!(lite.contains("a.db"), "{lite}");
    assert_ne!(
        lite,
        pin("engine = \"sqlite\"\npath = \"b.db\"\n"),
        "another file"
    );
}

#[test]
fn host_spellings_that_reach_the_same_socket_share_a_pin() {
    let groups: [&[&str]; 3] = [
        &["db.example", "DB.Example", "db.example."],
        &["::1", "[::1]", "0:0:0:0:0:0:0:1"],
        &["127.0.0.1", "127.0.0.1."],
    ];
    for group in groups {
        let first = pin(&pg(group[0]));
        for host in &group[1..] {
            assert_eq!(pin(&pg(host)), first, "{host} and {}", group[0]);
        }
    }
    assert!(
        pin(&pg("[::1]")).contains(r#""host":"::1""#),
        "brackets are dropped"
    );
}

#[test]
fn spellings_that_may_differ_stay_apart() {
    assert!(
        pin(&pg("127.1")).contains(r#""host":"127.1""#),
        "127.1 is verbatim"
    );
    assert_ne!(
        pin(&pg("127.1")),
        pin(&pg("127.0.0.1")),
        "127.1 is not merged"
    );
    let accented = pin(&pg("Bücher.example"));
    assert!(
        accented.contains(r#""host":"Bücher.example""#),
        "{accented}"
    );
    assert_ne!(
        accented,
        pin(&pg("bücher.example")),
        "non-ASCII is not lowercased"
    );
    let socket = pin(&pg("/var/run/postgresql"));
    assert!(
        socket.contains(r#""host":"/var/run/postgresql""#),
        "{socket}"
    );
    let quoted = pin(&pg("db\\\"x"));
    assert!(
        quoted.contains(r#""host":"db\"x""#),
        "JSON-escaped: {quoted}"
    );
}

#[test]
fn every_target_field_repoints_the_pin() {
    let pgx = pg("db.example");
    let cases: Vec<(&str, String, String)> = vec![
        ("postgres host", pgx.clone(), pg("db2.example")),
        ("postgres port", pgx.clone(), format!("{pgx}port = 5433\n")),
        (
            "postgres user",
            pgx.clone(),
            pgx.replace("\"ada\"", "\"bob\""),
        ),
        (
            "mysql host",
            server("mysql"),
            server("mysql").replace("db.example", "db2.example"),
        ),
        (
            "mssql port",
            server("mssql"),
            format!("{}port = 1434\n", server("mssql")),
        ),
        (
            "redis user",
            server("redis"),
            server("redis").replace("\"ada\"", "\"bob\""),
        ),
        ("engine", server("mysql"), server("mssql")),
        ("mongodb uri", MG.to_owned(), MG.replace("27017", "27018")),
        (
            "dynamodb key",
            DY.to_owned(),
            DY.replace("AKIDTEST", "AKIDOTHER"),
        ),
        (
            "dynamodb region",
            DY.to_owned(),
            DY.replace("eu-west-1", "us-east-1"),
        ),
        (
            "dynamodb endpoint",
            DY.to_owned(),
            DY.replace("5000", "5001"),
        ),
        ("s3 endpoint", S3.to_owned(), S3.replace("5000", "5001")),
        (
            "surrealdb host",
            SR.to_owned(),
            SR.replace("db.example", "db2.example"),
        ),
        (
            "surrealdb level",
            SR.to_owned(),
            SR.replace("\"root\"", "\"namespace\""),
        ),
    ];
    for (label, a, b) in cases {
        assert_ne!(pin(&a), pin(&b), "{label}");
    }
    for (label, b) in [
        (
            "database",
            pgx.replace("database = \"d\"", "database = \"e\""),
        ),
        ("read_only", format!("{pgx}read_only = true\n")),
        ("color", format!("{pgx}color = \"red\"\n")),
        (
            "statement_timeout",
            format!("{pgx}statement_timeout = \"5s\"\n"),
        ),
    ] {
        assert_eq!(pin(&pgx), pin(&b), "{label} is not part of the pin");
    }
}

#[test]
fn surrealdb_pins_what_its_level_signs_in_to() {
    let at = |level: &str| SR.replace("\"root\"", &format!("\"{level}\""));
    let ns = |s: String| s.replace("namespace = \"n\"", "namespace = \"m\"");
    let db = |s: String| s.replace("database = \"d\"", "database = \"e\"");
    assert_eq!(
        pin(&at("namespace")),
        r#"{"v":1,"engine":"surrealdb","user":"ada","host":"db.example","port":8000,"namespace":"n"}"#
    );
    assert_eq!(
        pin(&at("database")),
        r#"{"v":1,"engine":"surrealdb","user":"ada","host":"db.example","port":8000,"namespace":"n","database":"d"}"#
    );
    assert_eq!(
        pin(&at("root")),
        pin(&db(ns(at("root")))),
        "root ignores both"
    );
    assert_ne!(
        pin(&at("namespace")),
        pin(&ns(at("namespace"))),
        "namespace level"
    );
    assert_eq!(
        pin(&at("namespace")),
        pin(&db(at("namespace"))),
        "namespace level, database"
    );
    assert_ne!(
        pin(&at("database")),
        pin(&db(at("database"))),
        "database level"
    );
}

#[test]
fn the_ssh_hop_is_part_of_the_pin() {
    let base = pg("db.internal");
    let via = |ssh: &str| pin(&format!("{base}[connections.c.ssh]\n{ssh}"));
    let hop = via("host = \"bastion\"\nport = 2222\n");
    assert_eq!(
        hop,
        r#"{"v":1,"engine":"postgres","user":"ada","host":"db.internal","port":5432,"via":{"host":"bastion","port":2222}}"#
    );
    assert!(via("host = \"bastion\"\n").ends_with(r#""via":{"host":"bastion","port":null}}"#));
    assert_ne!(pin(&base), via("host = \"bastion\"\n"), "adding the hop");
    assert_ne!(
        hop,
        via("host = \"bastion2\"\nport = 2222\n"),
        "another bastion"
    );
    assert_ne!(
        hop,
        via("host = \"bastion\"\nport = 2223\n"),
        "another hop port"
    );
    assert_ne!(
        via("host = \"bastion\"\n"),
        via("host = \"bastion\"\nport = 22\n"),
        "port as written"
    );
    assert_eq!(
        hop,
        via("host = \"Bastion.\"\nport = 2222\n"),
        "the hop host normalises"
    );
    assert_eq!(
        hop,
        via("host = \"bastion\"\nport = 2222\nuser = \"ops\"\n"),
        "ssh user is not pinned"
    );
}

#[test]
fn a_derived_child_shares_its_parents_pin() {
    let w = open(&format!(
        "[connections.c]\n{}databases = [\"app\"]\n",
        pg("db.example")
    ));
    let parent = w.declared("c").unwrap().secret_pin();
    assert_eq!(w.connection("c/app").unwrap().secret_pin(), parent);
}
