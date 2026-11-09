use crate::common::{dbento, write_tree};

const TWO: &str = "dbento = 1\n[connections.prod]\nengine = \"postgres\"\nhost = \"db.example.com\"\ndatabase = \"app\"\nuser = \"app\"\nread_only = true\ncolor = \"red\"\n[connections.lite]\nengine = \"sqlite\"\npath = \"data/app.db\"\n";

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn connections_prints_a_table_found_from_a_subdirectory() {
    let ws = write_tree(&[("dbento.toml", TWO), ("queries/lite/a.sql", "select 1")]);
    let out = dbento()
        .current_dir(ws.path().join("queries/lite"))
        .arg("connections")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "\
name | engine   | target                      | read_only | color | folder
-----+----------+-----------------------------+-----------+-------+-------
lite | sqlite   | data/app.db                 | false     |       |
prod | postgres | app@db.example.com:5432/app | true      | red   |
"
    );
}

#[test]
fn connections_show_the_inherited_colour_and_the_folder() {
    let toml = "dbento = 1\n[folders.\"Prod\"]\ncolor = \"red\"\n\
                [connections.eu]\nengine = \"sqlite\"\npath = \"eu.db\"\nfolder = \"Prod/EU\"\n\
                [connections.lite]\nengine = \"sqlite\"\npath = \"lite.db\"\n";
    let ws = write_tree(&[("dbento.toml", toml)]);
    let out = dbento()
        .arg("--workspace")
        .arg(ws.path())
        .args(["connections", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "{\"dbento\":1,\"connections\":[\
{\"name\":\"eu\",\"engine\":\"sqlite\",\"target\":\"eu.db\",\"read_only\":false,\"color\":\"red\",\"folder\":\"Prod/EU\"},\
{\"name\":\"lite\",\"engine\":\"sqlite\",\"target\":\"lite.db\",\"read_only\":false,\"color\":null,\"folder\":null}]}\n"
    );
}

#[test]
fn connections_prints_json_with_workspace_flag() {
    let ws = write_tree(&[("dbento.toml", TWO)]);
    let out = dbento()
        .arg("--workspace")
        .arg(ws.path())
        .args(["connections", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "{\"dbento\":1,\"connections\":[\
{\"name\":\"lite\",\"engine\":\"sqlite\",\"target\":\"data/app.db\",\"read_only\":false,\"color\":null,\"folder\":null},\
{\"name\":\"prod\",\"engine\":\"postgres\",\"target\":\"app@db.example.com:5432/app\",\"read_only\":true,\"color\":\"red\",\"folder\":null}]}\n"
    );
}

#[test]
fn a_missing_workspace_is_exit_3_and_names_the_search() {
    let empty = tempfile::tempdir().unwrap();
    let out = dbento()
        .current_dir(empty.path())
        .arg("connections")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(out.stdout.is_empty());
    assert!(
        stderr(&out).contains("no dbento.toml in") && stderr(&out).contains("or any parent"),
        "{}",
        stderr(&out)
    );
    let out = dbento()
        .arg("--workspace")
        .arg(empty.path())
        .arg("connections")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(
        stderr(&out).contains("no dbento.toml in"),
        "{}",
        stderr(&out)
    );
    assert!(
        !stderr(&out).contains("or any parent"),
        "--workspace does not walk up: {}",
        stderr(&out)
    );
}

#[test]
fn an_invalid_workspace_is_exit_3() {
    let ws = write_tree(&[("dbento.toml", "dbento = 1\npassword = \"x\"\n")]);
    let out = dbento()
        .current_dir(ws.path())
        .arg("connections")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(
        stderr(&out).contains("dbento secret set"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn init_creates_a_workspace_once() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("new");
    let out = dbento()
        .arg("--workspace")
        .arg(&target)
        .arg("init")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(target.join("queries").is_dir());
    assert!(
        std::fs::read_to_string(target.join("dbento.toml"))
            .unwrap()
            .contains("dbento = 1")
    );
    let out = dbento().current_dir(&target).arg("init").output().unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(stderr(&out).contains("already exists"), "{}", stderr(&out));
}

#[test]
fn usage_errors_are_exit_3_and_help_is_exit_0() {
    assert_eq!(
        dbento().arg("frobnicate").output().unwrap().status.code(),
        Some(3)
    );
    assert_eq!(
        dbento()
            .args(["connections", "--format", "xml"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(3)
    );
    assert_eq!(
        dbento().arg("--help").output().unwrap().status.code(),
        Some(0)
    );
    let out = dbento().arg("--version").output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains(dbento_version()));
}

fn dbento_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
