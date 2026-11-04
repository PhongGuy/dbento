//! `dbento import datagrip` on small synthetic projects; consoles are always off.
mod common;
use common::{dbento, write_tree};
use std::path::Path;
use std::process::Output;
use tempfile::TempDir;

const SHOP: &str = r#"<project version="4"><component name="DataSourceManagerImpl" format="xml" multifile-model="true">
<data-source source="LOCAL" name="Shop" uuid="aaaaaaaa-0000-4000-8000-000000000001"><driver-ref>postgresql</driver-ref>
<jdbc-url>jdbc:postgresql://db.shop.example/shop?user=clerk&amp;password=hunter2-cli</jdbc-url></data-source>
<data-source source="LOCAL" name="Nobody" uuid="aaaaaaaa-0000-4000-8000-000000000002"><driver-ref>postgresql</driver-ref>
<jdbc-url>jdbc:postgresql://db.shop.example/stock</jdbc-url></data-source></component></project>"#;
const MALFORMED: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../dbento-core/tests/fixtures/datagrip/malformed"
);

fn project() -> TempDir {
    write_tree(&[(".idea/dataSources.xml", SHOP)])
}

fn import(ws: &Path, project: &Path, extra: &[&str]) -> Output {
    dbento()
        .arg("--workspace")
        .arg(ws)
        .args(["import", "datagrip"])
        .arg(project)
        .arg("--no-consoles")
        .args(extra)
        .output()
        .unwrap()
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap()
}

#[test]
fn the_json_report_lists_actions_skips_warnings_and_next_steps() {
    let (p, ws) = (project(), TempDir::new().unwrap());
    let out = import(ws.path(), p.path(), &["--format", "json"]);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let doc = json(&out);
    assert_eq!(
        (doc["dbento"].as_u64(), doc["dry_run"].as_bool()),
        (Some(1), Some(false))
    );
    assert_eq!(
        doc["connections"],
        serde_json::json!([{"name": "shop", "from": "Shop", "action": "add"}])
    );
    assert_eq!(doc["queries"], serde_json::json!([]));
    assert_eq!(
        doc["skipped"],
        serde_json::json!([{"kind": "data_source", "from": "Nobody", "reason": "no_user"}])
    );
    let reasons: Vec<&str> = doc["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["reason"].as_str().unwrap())
        .collect();
    assert_eq!(reasons, ["password_in_url", "tls_stricter"]);
    assert_eq!(
        doc["next_steps"],
        serde_json::json!(["dbento secret set shop"])
    );
    let toml = std::fs::read_to_string(ws.path().join("dbento.toml")).unwrap();
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    );
    for text in [&toml, &stdout, &stderr] {
        assert!(!text.contains("hunter2"), "{text}");
    }
}

#[test]
fn a_dry_run_reports_and_writes_nothing() {
    let (p, ws) = (project(), TempDir::new().unwrap());
    let out = import(ws.path(), p.path(), &["--dry-run", "--format", "json"]);
    assert_eq!(code(&out), 0);
    let doc = json(&out);
    assert_eq!(
        (
            doc["dry_run"].as_bool(),
            doc["connections"][0]["action"].as_str()
        ),
        (Some(true), Some("add"))
    );
    assert_eq!(std::fs::read_dir(ws.path()).unwrap().count(), 0);
}

#[test]
fn the_table_report_ends_with_the_secret_step() {
    let (p, ws) = (project(), TempDir::new().unwrap());
    let out = import(ws.path(), p.path(), &[]);
    assert_eq!(code(&out), 0);
    let text = String::from_utf8(out.stdout).unwrap();
    for needle in ["shop", "add", "no_user", "password_in_url"] {
        assert!(text.contains(needle), "{needle}: {text}");
    }
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[lines.len() - 2..],
        ["next steps:", "dbento secret set shop"],
        "{text}"
    );
}

#[test]
fn without_a_workspace_flag_the_nearest_workspace_or_the_working_directory_is_used() {
    let p = project();
    let run_in = |dir: &Path| {
        dbento()
            .current_dir(dir)
            .args(["import", "datagrip"])
            .arg(p.path())
            .arg("--no-consoles")
            .output()
            .unwrap()
    };
    // /dev/shm has no workspace above it, unlike a temp dir inside someone's checkout.
    let clean = Path::new("/dev/shm");
    let fresh = if clean.is_dir() {
        TempDir::new_in(clean).unwrap()
    } else {
        TempDir::new().unwrap()
    };
    let above: Vec<_> = fresh
        .path()
        .ancestors()
        .skip(1)
        .filter(|a| a.join("dbento.toml").exists())
        .collect();
    assert!(
        above.is_empty(),
        "a dbento.toml above the test folder would capture the import: {above:?}"
    );
    assert_eq!(code(&run_in(fresh.path())), 0);
    assert!(
        std::fs::read_to_string(fresh.path().join("dbento.toml"))
            .unwrap()
            .contains("[connections.shop]")
    );
    let outer = write_tree(&[("dbento.toml", "dbento = 1\n"), ("sub/keep.txt", "")]);
    assert_eq!(code(&run_in(&outer.path().join("sub"))), 0);
    assert!(
        std::fs::read_to_string(outer.path().join("dbento.toml"))
            .unwrap()
            .contains("[connections.shop]")
    );
    assert!(!outer.path().join("sub/dbento.toml").exists());
}

#[test]
fn a_conflict_is_left_alone_under_skip_and_renamed_under_rename() {
    let p = project();
    let existing = "dbento = 1\n[connections.shop]\nengine = \"postgres\"\nhost = \"other.example\"\ndatabase = \"shop\"\nuser = \"clerk\"\n";
    let ws = write_tree(&[("dbento.toml", existing)]);
    let out = import(ws.path(), p.path(), &["--format", "json"]);
    assert_eq!(code(&out), 0);
    assert_eq!(json(&out)["connections"][0]["action"], "conflict");
    assert_eq!(
        std::fs::read_to_string(ws.path().join("dbento.toml")).unwrap(),
        existing
    );
    let out = import(
        ws.path(),
        p.path(),
        &["--on-conflict", "rename", "--format", "json"],
    );
    assert_eq!(
        json(&out)["connections"][0],
        serde_json::json!({"name": "shop-2", "from": "Shop", "action": "add"})
    );
}

#[test]
fn default_user_fills_a_missing_user() {
    let (p, ws) = (project(), TempDir::new().unwrap());
    let out = import(
        ws.path(),
        p.path(),
        &["--default-user", "bob", "--format", "json"],
    );
    assert_eq!(json(&out)["connections"][1]["name"], "nobody");
    let opened = dbento_core::Workspace::open(ws.path()).unwrap();
    assert_eq!(opened.connection("nobody").unwrap().user(), Some("bob"));
}

#[test]
fn failures_before_any_write_exit_3_and_write_nothing() {
    let ws = TempDir::new().unwrap();
    let empty = TempDir::new().unwrap();
    for folder in [Path::new(MALFORMED), empty.path()] {
        assert_eq!(
            code(&import(ws.path(), folder, &[])),
            3,
            "{}",
            folder.display()
        );
    }
    assert_eq!(std::fs::read_dir(ws.path()).unwrap().count(), 0);
    let inline = write_tree(&[("dbento.toml", "dbento = 1\nconnections = {}\n")]);
    assert_eq!(code(&import(inline.path(), project().path(), &[])), 3);
    assert_eq!(
        std::fs::read_to_string(inline.path().join("dbento.toml")).unwrap(),
        "dbento = 1\nconnections = {}\n"
    );
    assert_eq!(
        code(&import(
            ws.path(),
            project().path(),
            &["--on-conflict", "bogus"]
        )),
        3
    );
}

#[test]
fn a_relative_project_folder_resolves_a_project_dir_sqlite_path_to_the_real_file() {
    let lite = r#"<project version="4"><component name="DataSourceManagerImpl"><data-source source="LOCAL" name="Lite" uuid="cccccccc-0000-4000-8000-000000000001"><driver-ref>sqlite.xerial</driver-ref><jdbc-url>jdbc:sqlite:$PROJECT_DIR$/app.db</jdbc-url></data-source></component></project>"#;
    let base = write_tree(&[("proj/.idea/dataSources.xml", lite), ("proj/app.db", "")]);
    let ws = TempDir::new().unwrap();
    let out = dbento()
        .current_dir(base.path())
        .arg("--workspace")
        .arg(ws.path())
        .args([
            "import",
            "datagrip",
            "proj",
            "--no-consoles",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let doc = json(&out);
    // The workspace lies elsewhere, so only machine_specific_path may remain.
    assert_eq!(
        doc["warnings"],
        serde_json::json!([{"from": "Lite", "reason": "machine_specific_path"}]),
        "{doc}"
    );
    let toml = std::fs::read_to_string(ws.path().join("dbento.toml")).unwrap();
    let path = toml
        .lines()
        .find_map(|l| l.strip_prefix("path = \""))
        .and_then(|l| l.strip_suffix('"'))
        .unwrap_or_else(|| panic!("no path in {toml}"));
    assert_eq!(
        ws.path().join(path).canonicalize().unwrap(),
        base.path().join("proj/app.db").canonicalize().unwrap()
    );
}
