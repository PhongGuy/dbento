//! The resolved dependency graph: no aws-lc, OpenSSL or native-tls, one rustls, the pinned client
//! crates, no SurrealDB crate, and tiberius and mongodb from their patched copies in vendor/.
use std::collections::BTreeSet;
use std::process::Command;

/// Every `name vX.Y.Z [(path)]` line `cargo tree` resolves for the workspace, on every target,
/// over normal, build and dev edges. Not `--offline`: a cold CI cache lacks the
/// platform-only crates `--target all` resolves, so cargo may fetch them here.
fn packages() -> BTreeSet<String> {
    let out = Command::new(env!("CARGO"))
        .args([
            "tree",
            // Plain text whatever CARGO_TERM_COLOR says, so the duplicate marker parses.
            "--color",
            "never",
            "--workspace",
            "--locked",
            "--target",
            "all",
            "-e",
            "normal,build,dev",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ])
        .current_dir(xtask::workspace_root())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| l.trim_end_matches(" (*)").to_owned())
        .filter(|l| !l.is_empty())
        .collect()
}

fn named<'a>(packages: &'a BTreeSet<String>, name: &str) -> Vec<&'a str> {
    packages
        .iter()
        .map(String::as_str)
        .filter(|p| p.split(' ').next() == Some(name))
        .collect()
}

#[test]
fn no_forbidden_tls_stack_and_one_rustls() {
    let packages = packages();
    for name in [
        "aws-lc-rs",
        "aws-lc-sys",
        "openssl",
        "openssl-sys",
        "native-tls",
    ] {
        assert!(
            named(&packages, name).is_empty(),
            "{name}: {:?}",
            named(&packages, name)
        );
    }
    let rustls: BTreeSet<&str> = named(&packages, "rustls")
        .into_iter()
        .map(|p| p.split(' ').nth(1).unwrap())
        .collect();
    assert_eq!(rustls.len(), 1, "{rustls:?}");
}

#[test]
fn tiberius_resolves_to_the_vendored_copy() {
    let packages = packages();
    let tiberius = named(&packages, "tiberius");
    assert_eq!(tiberius.len(), 1, "{tiberius:?}");
    assert!(
        tiberius[0].starts_with("tiberius v0.13.0 (") && tiberius[0].ends_with("vendor/tiberius)"),
        "{}",
        tiberius[0]
    );
}

#[test]
fn the_vendored_copy_keeps_its_licences_and_lists_its_patches() {
    let dir = xtask::workspace_root().join("vendor/tiberius");
    for file in ["LICENSE-MIT.txt", "LICENSE-APACHE.txt"] {
        assert!(dir.join(file).is_file(), "{file}");
    }
    let patches = std::fs::read_to_string(dir.join("DBENTO-PATCHES")).unwrap();
    assert!(
        patches.contains("P1") && patches.contains("#456"),
        "{patches}"
    );
    assert!(
        patches.contains("P3") && patches.contains("simple_query_items"),
        "{patches}"
    );
    assert!(
        patches.contains("P5") && patches.contains("#461") && patches.contains("exact-money"),
        "{patches}"
    );
    assert!(
        patches.contains("P6") && patches.contains("UTF-8"),
        "{patches}"
    );
    assert!(
        patches.contains("P4") && patches.contains("type_info"),
        "{patches}"
    );
}

#[test]
fn mongodb_resolves_once_to_the_vendored_copy_and_bson_at_its_pin() {
    let packages = packages();
    let mongodb = named(&packages, "mongodb");
    assert_eq!(mongodb.len(), 1, "{mongodb:?}");
    assert!(
        mongodb[0].starts_with("mongodb v3.9.1 (") && mongodb[0].ends_with("vendor/mongodb)"),
        "{}",
        mongodb[0]
    );
    assert_eq!(named(&packages, "bson"), ["bson v3.1.0"]);
}

#[test]
fn the_vendored_mongodb_keeps_its_licence_and_lists_its_patch() {
    let dir = xtask::workspace_root().join("vendor/mongodb");
    assert!(dir.join("LICENSE").is_file());
    let patches = std::fs::read_to_string(dir.join("DBENTO-PATCHES")).unwrap();
    assert!(
        patches.contains("P1")
            && patches.contains("MAX_REPLY_DEPTH")
            && patches.contains("MAX_BATCH_DEPTH"),
        "{patches}"
    );
}

#[test]
fn the_surrealdb_client_crates_resolve_at_their_pins_and_no_bsl_crate_does() {
    let packages = packages();
    assert_eq!(
        named(&packages, "tokio-tungstenite"),
        ["tokio-tungstenite v0.30.0"]
    );
    assert_eq!(named(&packages, "cbor4ii"), ["cbor4ii v1.2.4"]);
    assert_eq!(named(&packages, "half"), ["half v2.7.1"]);
    let bsl: Vec<&String> = packages
        .iter()
        .filter(|p| p.starts_with("surreal"))
        .collect();
    assert!(bsl.is_empty(), "{bsl:?}");
}

#[test]
fn the_redis_crate_resolves_once_at_its_pin() {
    assert_eq!(named(&packages(), "redis"), ["redis v1.8.1"]);
}

// The pilot is a separate workspace that the root patch does not reach, so it must
// repeat every vendored crate's patch or it silently builds the unpatched release.
#[test]
fn the_pilot_workspace_patches_every_crate_the_root_does() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let patches = |manifest: &str| -> BTreeSet<String> {
        let text = std::fs::read_to_string(root.join(manifest)).expect("read manifest");
        let doc: toml::Table = text.parse().expect("parse manifest");
        doc["patch"]["crates-io"]
            .as_table()
            .expect("a crates-io patch table")
            .keys()
            .cloned()
            .collect()
    };
    let root_patches = patches("Cargo.toml");
    let pilot_patches = patches("tools/dbento-pilot/Cargo.toml");
    let missing: Vec<_> = root_patches.difference(&pilot_patches).collect();
    assert!(missing.is_empty(), "the pilot does not patch {missing:?}");
}
