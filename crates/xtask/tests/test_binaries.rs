//! A crate with `autotests = false` runs only the test files it names, so every file in its
//! `tests/` must be a module of `tests/main.rs` or a `[[test]]` binary of its own.
use std::path::Path;

/// The `tests/*.rs` files of the crate at `dir` that neither `tests/main.rs` declares nor a
/// `[[test]]` entry names. Empty for a crate that still discovers its tests itself.
fn undeclared(dir: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(dir.join("Cargo.toml")).expect("read manifest");
    let doc: toml::Table = text.parse().expect("parse manifest");
    if doc["package"]
        .get("autotests")
        .and_then(toml::Value::as_bool)
        != Some(false)
    {
        return Vec::new();
    }
    let binaries: Vec<String> = doc
        .get("test")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|t| t.get("path")?.as_str().map(str::to_owned))
        .collect();
    let tests = dir.join("tests");
    let main = std::fs::read_to_string(tests.join("main.rs")).unwrap_or_default();
    let modules: Vec<&str> = main
        .lines()
        .filter_map(|l| l.trim().strip_prefix("mod ")?.strip_suffix(';'))
        .collect();
    let mut missing: Vec<String> = std::fs::read_dir(&tests)
        .expect("read tests")
        .map(|e| {
            e.expect("a dir entry")
                .file_name()
                .into_string()
                .expect("a UTF-8 name")
        })
        .filter(|name| name.ends_with(".rs") && name != "main.rs")
        .filter(|name| {
            !modules.contains(&name.trim_end_matches(".rs"))
                && !binaries.contains(&format!("tests/{name}"))
        })
        .map(|name| format!("{}/tests/{name}", dir.display()))
        .collect();
    missing.sort();
    missing
}

/// The workspace member directories, from the root manifest.
fn members() -> Vec<std::path::PathBuf> {
    let root = xtask::workspace_root();
    let text = std::fs::read_to_string(root.join("Cargo.toml")).expect("read manifest");
    let doc: toml::Table = text.parse().expect("parse manifest");
    doc["workspace"]["members"]
        .as_array()
        .expect("a members list")
        .iter()
        .map(|m| root.join(m.as_str().expect("a member path")))
        .collect()
}

#[test]
fn every_test_file_of_a_one_binary_crate_is_declared() {
    let missing: Vec<String> = members().iter().flat_map(|dir| undeclared(dir)).collect();
    assert!(missing.is_empty(), "never compiled: {missing:?}");
}

#[test]
fn a_file_left_out_of_main_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let tests = dir.path().join("tests");
    std::fs::create_dir(&tests).unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"x\"\nautotests = false\n\n\
         [[test]]\nname = \"integration\"\npath = \"tests/main.rs\"\n\n\
         [[test]]\nname = \"alone\"\npath = \"tests/alone.rs\"\n",
    )
    .unwrap();
    std::fs::write(tests.join("main.rs"), "mod declared;\nmod support;\n").unwrap();
    for file in ["declared.rs", "alone.rs", "forgotten.rs"] {
        std::fs::write(tests.join(file), "").unwrap();
    }
    assert_eq!(
        undeclared(dir.path()),
        [format!("{}/tests/forgotten.rs", dir.path().display())]
    );
}

#[test]
fn a_crate_that_discovers_its_tests_is_not_checked() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("tests")).unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    std::fs::write(dir.path().join("tests/loose.rs"), "").unwrap();
    assert!(undeclared(dir.path()).is_empty());
}
