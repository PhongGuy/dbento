//! The session binding: one secret-free encoding of exactly what same_session compares.
use dbento_workspace::{ConnectionConfig, Workspace};
use std::path::{Path, PathBuf};

fn ws(text: &str) -> Workspace {
    Workspace::from_text(PathBuf::from("/w"), Path::new("/w/dbento.toml"), text)
        .unwrap_or_else(|e| panic!("{text}: {e}"))
}

/// Connection `c` written with `body`, in a workspace at /w.
fn conn(body: &str) -> ConnectionConfig {
    ws(&format!("dbento = 1\n[connections.c]\n{body}"))
        .declared("c")
        .unwrap()
        .clone()
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
const S3: &str = "engine = \"s3\"\nregion = \"auto\"\nendpoint = \"http://127.0.0.1:5000\"\naccess_key_id = \"AKIDTEST\"\nsecret = \"keychain\"\n";

fn pair_differs(label: &str, a: &str, b: &str) {
    let (a, b) = (conn(a), conn(b));
    assert_ne!(a.session_binding(), b.session_binding(), "{label}");
    assert!(!a.same_session(&b), "{label}");
}

fn pair_same(label: &str, a: &ConnectionConfig, b: &ConnectionConfig) {
    assert_eq!(a.session_binding(), b.session_binding(), "{label}");
    assert!(a.same_session(b), "{label}");
}

#[test]
fn the_sqlite_binding_is_pinned() {
    assert_eq!(
        conn(SQL).session_binding(),
        r#"{"b":1,"engine":"sqlite","read_only":false,"statement_timeout_ns":null,"secret_source":"none","file_path":["/","w","a.db"],"host":null,"port":null,"database":null,"user":null,"tls":null,"tls_ca":null,"tls_cert":null,"tls_key":null,"settings":{},"read_only_user":null,"uri":null,"namespace":null,"auth_level":null,"region":null,"endpoint":null,"access_key_id":null,"consistent_read":false}"#
    );
}

#[test]
fn every_bound_field_changes_the_binding_and_the_session() {
    let with = |base: &str, extra: &str| format!("{base}{extra}");
    let cases: Vec<(&str, String, String)> = vec![
        (
            "engine",
            SQL.into(),
            "engine = \"duckdb\"\npath = \"a.db\"\n".into(),
        ),
        ("read_only", SQL.into(), with(SQL, "read_only = true\n")),
        (
            "statement_timeout",
            SQL.into(),
            with(SQL, "statement_timeout = \"5s\"\n"),
        ),
        ("secret kind", PG.into(), with(PG, "secret = \"none\"\n")),
        (
            "secret env name",
            with(PG, "secret = { env = \"DBENTO_SECRET_PGPW\" }\n"),
            with(PG, "secret = { env = \"DBENTO_SECRET_PGPW2\" }\n"),
        ),
        (
            "file_path",
            SQL.into(),
            "engine = \"sqlite\"\npath = \"b.db\"\n".into(),
        ),
        ("host", PG.into(), PG.replace("db.internal", "db2.internal")),
        ("port", PG.into(), with(PG, "port = 5433\n")),
        (
            "database",
            PG.into(),
            PG.replace("database = \"d\"", "database = \"e\""),
        ),
        (
            "user",
            PG.into(),
            PG.replace("user = \"u\"", "user = \"v\""),
        ),
        ("tls", PGL.into(), with(PGL, "tls = \"verify\"\n")),
        (
            "tls_ca",
            with(PG, "tls_ca = \"ca.pem\"\n"),
            with(PG, "tls_ca = \"ca2.pem\"\n"),
        ),
        (
            "tls_cert",
            with(PG, "tls_cert = \"c.crt\"\ntls_key = \"c.key\"\n"),
            with(PG, "tls_cert = \"d.crt\"\ntls_key = \"c.key\"\n"),
        ),
        (
            "tls_key",
            with(PG, "tls_cert = \"c.crt\"\ntls_key = \"c.key\"\n"),
            with(PG, "tls_cert = \"c.crt\"\ntls_key = \"d.key\"\n"),
        ),
        (
            "settings",
            with(PG, "settings = { search_path = \"app\" }\n"),
            with(PG, "settings = { search_path = \"other\" }\n"),
        ),
        (
            "read_only_user",
            with(MS, "read_only_user = \"r\"\n"),
            with(MS, "read_only_user = \"s\"\n"),
        ),
        ("uri", MG.into(), MG.replace("27017", "27018")),
        (
            "namespace",
            SR.into(),
            SR.replace("namespace = \"n\"", "namespace = \"m\""),
        ),
        (
            "auth_level",
            SR.into(),
            SR.replace("\"root\"", "\"namespace\""),
        ),
        ("region", DY.into(), DY.replace("eu-west-1", "us-east-1")),
        ("endpoint", DY.into(), DY.replace("5000", "5001")),
        (
            "access_key_id",
            DY.into(),
            DY.replace("AKIDTEST", "AKIDOTHER"),
        ),
        (
            "consistent_read",
            DY.into(),
            DY.replace("consistent_read = true", "consistent_read = false"),
        ),
        (
            "virtual_hosted",
            S3.into(),
            with(S3, "virtual_hosted = true\n"),
        ),
    ];
    for (label, a, b) in &cases {
        pair_differs(label, a, b);
    }
}

#[test]
fn labels_and_spellings_leave_the_binding_alone() {
    let cases = [
        ("color", SQL.to_owned(), format!("{SQL}color = \"red\"\n")),
        ("folder", SQL.to_owned(), format!("{SQL}folder = \"x\"\n")),
        (
            "effective_color",
            format!("{SQL}folder = \"x\"\n"),
            format!("{SQL}folder = \"x\"\n[folders.x]\ncolor = \"red\"\n"),
        ),
        (
            "mariadb",
            format!("engine = \"mysql\"\n{MY}"),
            format!("engine = \"mariadb\"\n{MY}"),
        ),
        (
            "path absolute",
            SQL.to_owned(),
            "engine = \"sqlite\"\npath = \"/w/a.db\"\n".to_owned(),
        ),
        (
            "path dotted",
            SQL.to_owned(),
            "engine = \"sqlite\"\npath = \"./a.db\"\n".to_owned(),
        ),
        (
            "databases",
            PG.to_owned(),
            format!("{PG}databases = [\"d1\"]\n"),
        ),
        (
            "buckets",
            S3.to_owned(),
            format!("{S3}buckets = [\"logs\"]\n"),
        ),
    ];
    for (label, a, b) in &cases {
        pair_same(label, &conn(a), &conn(b));
    }
}

#[test]
fn the_name_is_not_bound() {
    let w = ws(&format!(
        "dbento = 1\n[connections.a]\n{SQL}\n[connections.b]\n{SQL}"
    ));
    pair_same("name", w.declared("a").unwrap(), w.declared("b").unwrap());
}

#[test]
fn a_derived_child_binds_like_the_declared_connection_it_equals() {
    let w = ws(&format!(
        "dbento = 1\n[connections.c]\n{PG}databases = [\"d1\"]\n\n[connections.x]\n{}",
        PG.replace("database = \"d\"", "database = \"d1\"")
    ));
    let child = w.resolve("c/d1").unwrap().into_owned();
    pair_same("parent", &child, w.declared("x").unwrap());
    pair_differs(
        "child database",
        PG,
        &PG.replace("database = \"d\"", "database = \"d1\""),
    );
}

#[test]
fn virtual_hosted_enters_the_binding_only_when_true() {
    let plain: serde_json::Value = serde_json::from_str(&conn(S3).session_binding()).unwrap();
    assert!(plain.get("virtual_hosted").is_none(), "{plain}");
    let hosted = conn(&format!("{S3}virtual_hosted = true\n"));
    let v: serde_json::Value = serde_json::from_str(&hosted.session_binding()).unwrap();
    assert_eq!(v["virtual_hosted"], serde_json::Value::Bool(true));
}

#[test]
fn the_binding_holds_only_the_bound_keys_and_the_secret_source_by_name() {
    let c = conn(&format!(
        "{PG}secret = {{ env = \"DBENTO_SECRET_PGPW\" }}\n"
    ));
    let v: serde_json::Value = serde_json::from_str(&c.session_binding()).unwrap();
    let mut got: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    let mut want = vec![
        "b",
        "engine",
        "read_only",
        "statement_timeout_ns",
        "secret_source",
        "file_path",
        "host",
        "port",
        "database",
        "user",
        "tls",
        "tls_ca",
        "tls_cert",
        "tls_key",
        "settings",
        "read_only_user",
        "uri",
        "namespace",
        "auth_level",
        "region",
        "endpoint",
        "access_key_id",
        "consistent_read",
    ];
    got.sort_unstable();
    want.sort_unstable();
    assert_eq!(got, want);
    assert_eq!(
        v["secret_source"],
        serde_json::json!({"env": "DBENTO_SECRET_PGPW"})
    );
    assert_eq!(v["statement_timeout_ns"], serde_json::Value::Null);
    let timed = conn(&format!("{SQL}statement_timeout = \"5s\"\n"));
    let t: serde_json::Value = serde_json::from_str(&timed.session_binding()).unwrap();
    assert_eq!(t["statement_timeout_ns"], 5_000_000_000u64);
}

#[cfg(unix)]
#[test]
fn paths_differing_only_in_an_invalid_byte_bind_apart() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    // The text must be UTF-8, so the invalid byte rides in on the workspace folder.
    let at = |byte: u8| {
        let dir = PathBuf::from(OsStr::from_bytes(&[b'/', b'w', byte]));
        let text = format!("dbento = 1\n[connections.c]\n{SQL}");
        Workspace::from_text(dir.clone(), &dir.join("dbento.toml"), &text)
            .unwrap_or_else(|e| panic!("{text}: {e}"))
            .declared("c")
            .unwrap()
            .clone()
    };
    let (a, b) = (at(0xfe), at(0xff));
    assert_ne!(
        a.session_binding(),
        b.session_binding(),
        "invalid byte lost"
    );
    assert!(!a.same_session(&b), "invalid byte lost");
}
