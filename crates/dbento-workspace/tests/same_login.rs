//! Which changes keep the login a saved secret belongs to, and which keys a change touches.
use dbento_workspace::{ConnectionConfig, Workspace};
use std::path::{Path, PathBuf};

fn ws(text: &str) -> Workspace {
    Workspace::from_text(PathBuf::from("/w"), Path::new("/w/dbento.toml"), text)
        .unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn conn(body: &str) -> ConnectionConfig {
    ws(&format!("dbento = 1\n[connections.c]\n{body}"))
        .declared("c")
        .unwrap()
        .clone()
}

fn with(base: &str, extra: &str) -> String {
    format!("{base}{extra}")
}

const SQL: &str = "engine = \"sqlite\"\npath = \"a.db\"\n";
const PG: &str = "engine = \"postgres\"\nhost = \"db.internal\"\ndatabase = \"d\"\nuser = \"u\"\n";
const PGL: &str = "engine = \"postgres\"\nhost = \"localhost\"\ndatabase = \"d\"\nuser = \"u\"\n";
const MS: &str = "engine = \"mssql\"\nhost = \"db.internal\"\nuser = \"u\"\n";
const MY: &str = "host = \"db.internal\"\nuser = \"u\"\n";
const MG: &str =
    "engine = \"mongodb\"\nuri = \"mongodb://app@db.internal:27017/shop?authSource=admin\"\n";
const SR: &str = "engine = \"surrealdb\"\nhost = \"db.internal\"\nnamespace = \"n\"\ndatabase = \"d\"\nuser = \"u\"\nauth_level = \"root\"\n";
const DY: &str = "engine = \"dynamodb\"\nregion = \"eu-west-1\"\nendpoint = \"http://127.0.0.1:5000\"\naccess_key_id = \"AKIDTEST\"\nsecret = \"keychain\"\nconsistent_read = true\n";

// (the one key differing_keys reports, whether that key identifies the login, a, b)
fn cases() -> Vec<(&'static str, bool, String, String)> {
    vec![
        (
            "engine",
            true,
            SQL.into(),
            "engine = \"duckdb\"\npath = \"a.db\"\n".into(),
        ),
        (
            "engine",
            false,
            format!("engine = \"mysql\"\n{MY}"),
            format!("engine = \"mariadb\"\n{MY}"),
        ),
        (
            "read_only",
            false,
            PG.into(),
            with(PG, "read_only = true\n"),
        ),
        (
            "statement_timeout",
            false,
            PG.into(),
            with(PG, "statement_timeout = \"5s\"\n"),
        ),
        (
            "path",
            false,
            SQL.into(),
            "engine = \"sqlite\"\npath = \"b.db\"\n".into(),
        ),
        (
            "host",
            true,
            PG.into(),
            PG.replace("db.internal", "db2.internal"),
        ),
        (
            "database",
            false,
            PG.into(),
            PG.replace("database = \"d\"", "database = \"e\""),
        ),
        (
            "database",
            false,
            SR.into(),
            SR.replace("database = \"d\"", "database = \"e\""),
        ),
        (
            "user",
            true,
            PG.into(),
            PG.replace("user = \"u\"", "user = \"v\""),
        ),
        ("port", true, PG.into(), with(PG, "port = 5433\n")),
        ("secret", false, PG.into(), with(PG, "secret = \"none\"\n")),
        ("tls", false, PGL.into(), with(PGL, "tls = \"verify\"\n")),
        (
            "tls_ca",
            false,
            with(PG, "tls_ca = \"ca.pem\"\n"),
            with(PG, "tls_ca = \"ca2.pem\"\n"),
        ),
        (
            "tls_cert",
            false,
            with(PG, "tls_cert = \"c.crt\"\ntls_key = \"c.key\"\n"),
            with(PG, "tls_cert = \"d.crt\"\ntls_key = \"c.key\"\n"),
        ),
        (
            "tls_key",
            false,
            with(PG, "tls_cert = \"c.crt\"\ntls_key = \"c.key\"\n"),
            with(PG, "tls_cert = \"c.crt\"\ntls_key = \"d.key\"\n"),
        ),
        (
            "settings",
            false,
            with(PG, "settings = { search_path = \"app\" }\n"),
            with(PG, "settings = { search_path = \"other\" }\n"),
        ),
        (
            "databases",
            false,
            PG.into(),
            with(PG, "databases = [\"d1\"]\n"),
        ),
        (
            "read_only_user",
            false,
            with(MS, "read_only_user = \"r\"\n"),
            with(MS, "read_only_user = \"s\"\n"),
        ),
        ("uri", true, MG.into(), MG.replace("27017", "27018")),
        (
            "namespace",
            true,
            SR.into(),
            SR.replace("namespace = \"n\"", "namespace = \"m\""),
        ),
        (
            "auth_level",
            true,
            SR.into(),
            SR.replace("\"root\"", "\"namespace\""),
        ),
        (
            "region",
            true,
            DY.into(),
            DY.replace("eu-west-1", "us-east-1"),
        ),
        ("endpoint", true, DY.into(), DY.replace("5000", "5001")),
        (
            "access_key_id",
            true,
            DY.into(),
            DY.replace("AKIDTEST", "AKIDOTHER"),
        ),
        (
            "consistent_read",
            false,
            DY.into(),
            DY.replace("consistent_read = true", "consistent_read = false"),
        ),
    ]
}

#[test]
fn only_a_login_key_makes_another_login() {
    for (key, login, a, b) in cases() {
        let (a, b) = (conn(&a), conn(&b));
        assert_eq!(a.same_login(&b), !login, "{key}");
        assert_eq!(b.same_login(&a), !login, "{key} reversed");
    }
}

#[test]
fn differing_keys_names_the_one_changed_key() {
    for (key, _, a, b) in cases() {
        assert_eq!(conn(&a).differing_keys(&conn(&b)), vec![key], "{key}");
    }
}

#[test]
fn differing_keys_lists_several_in_format_order() {
    let a = conn(PG);
    let b = conn(&with(
        &PG.replace("database = \"d\"", "database = \"e\""),
        "port = 5433\nread_only = true\nsettings = { search_path = \"app\" }\n",
    ));
    assert_eq!(
        a.differing_keys(&b),
        vec!["read_only", "database", "port", "settings"]
    );
}

#[test]
fn name_colour_and_folder_are_neither_login_nor_listed() {
    let w = ws(&format!(
        "dbento = 1\n[connections.a]\n{PG}\n[connections.b]\n{PG}color = \"red\"\nfolder = \"x\"\n"
    ));
    let (a, b) = (w.declared("a").unwrap(), w.declared("b").unwrap());
    assert!(a.same_login(b));
    assert!(a.differing_keys(b).is_empty());
}

#[test]
fn same_session_implies_same_login_in_every_valid_fixture() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/valid");
    let mut pairs = 0;
    for entry in std::fs::read_dir(&root).unwrap() {
        let case = entry.unwrap().path();
        let text = std::fs::read_to_string(case.join("dbento.toml")).unwrap();
        let w = Workspace::from_text(
            PathBuf::from("/fixture"),
            Path::new("/fixture/dbento.toml"),
            &text,
        )
        .unwrap_or_else(|e| panic!("{}: {e}", case.display()));
        for a in w.connections() {
            for b in w.connections() {
                if a.same_session(b) {
                    assert!(
                        a.same_login(b),
                        "{}: {} and {}",
                        case.display(),
                        a.name(),
                        b.name()
                    );
                    pairs += 1;
                }
            }
        }
    }
    assert!(pairs > 0, "no fixture connection was checked");
}
