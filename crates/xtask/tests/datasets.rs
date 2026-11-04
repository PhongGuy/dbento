use xtask::datasets::*;

#[test]
fn datasets_parse_in_order_without_duplicates() {
    assert_eq!(parse_datasets("").unwrap(), vec![]);
    assert_eq!(
        parse_datasets("stress, pagila,stress").unwrap(),
        vec![Choice::Stress, Choice::Download(&PAGILA)]
    );
    assert_eq!(
        parse_datasets("airlines").unwrap(),
        vec![Choice::Download(&AIRLINES_SMALL)]
    );
    assert_eq!(
        parse_datasets("airlines-big").unwrap(),
        vec![Choice::Download(&AIRLINES_BIG)]
    );
}

#[test]
fn imdb_unknown_names_and_two_airlines_sizes_are_refused() {
    let imdb = parse_datasets("pagila,imdb").unwrap_err();
    assert!(format!("{imdb:#}").contains("never downloads"), "{imdb:#}");
    assert!(parse_datasets("chinook").is_err());
    assert!(parse_datasets("pagila,,stress").is_err());
    let two = parse_datasets("airlines,airlines-medium").unwrap_err();
    assert!(format!("{two:#}").contains("one airlines"), "{two:#}");
}

#[test]
fn every_pin_is_https_with_a_sha256_and_a_license() {
    for d in [&PAGILA, &AIRLINES_SMALL, &AIRLINES_MEDIUM, &AIRLINES_BIG] {
        assert!(!d.license.is_empty(), "{}", d.name);
        for f in d.downloads {
            assert!(f.url.starts_with("https://"), "{}", f.url);
            assert!(f.url.ends_with(f.file), "{}", f.url);
            assert_eq!(f.sha256.len(), 64);
            assert!(
                f.sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            );
        }
    }
    assert_eq!(AIRLINES_SMALL.downloads[0].file, "demo-small-en.zip");
}

#[test]
fn curl_sends_the_research_agent_and_no_address() {
    let args = curl_args(
        "https://example.org/a.zip",
        std::path::Path::new("/tmp/a.zip.part"),
    );
    let at = args.iter().position(|a| a == "-A").unwrap();
    assert_eq!(args[at + 1], USER_AGENT);
    assert!(!USER_AGENT.contains('@'));
    for flag in ["-f", "-L", "--proto", "=https", "-o", "/tmp/a.zip.part"] {
        assert!(
            args.iter().any(|a| a == flag),
            "{flag} missing from {args:?}"
        );
    }
    assert_eq!(args.last().unwrap(), "https://example.org/a.zip");
}

#[test]
fn a_cached_file_that_matches_is_used_without_the_network() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("x.sql"), b"select 1;\n").unwrap();
    let sha256: &'static str = Box::leak(xtask::db::sha256_hex(b"select 1;\n").into_boxed_str());
    let d = Download {
        url: "https://invalid.invalid/x.sql",
        file: "x.sql",
        sha256,
    };
    assert_eq!(fetch(&d, dir.path()).unwrap(), dir.path().join("x.sql"));
}

#[test]
fn a_mismatched_file_fails_verification_and_a_bad_download_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.sql");
    std::fs::write(&path, b"tampered").unwrap();
    let err = verify_file(&path, &"0".repeat(64)).unwrap_err();
    assert!(
        format!("{err:#}").contains(&xtask::db::sha256_hex(b"tampered")),
        "{err:#}"
    );
    // A refused connection fails at once; curl's --retry backs off on an unresolvable host.
    let d = Download {
        url: "https://127.0.0.1:1/x.sql",
        file: "x.sql",
        sha256: "0000000000000000000000000000000000000000000000000000000000000000",
    };
    assert!(fetch(&d, dir.path()).is_err());
    assert!(!dir.path().join("x.sql.part").exists());
    assert!(
        !path.exists(),
        "a cached file that no longer matches is removed before refetching"
    );
}

#[test]
fn the_source_note_records_license_urls_and_hashes() {
    let note = source_note(&PAGILA);
    assert!(note.contains(PAGILA.license));
    for f in PAGILA.downloads {
        assert!(note.contains(f.url) && note.contains(f.sha256));
    }
}

#[test]
fn a_planted_part_symlink_is_removed_without_touching_its_target() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("victim");
    std::fs::write(&target, b"keep").unwrap();
    let part = dir.path().join("x.zip.part");
    std::os::unix::fs::symlink(&target, &part).unwrap();
    clear_part(&part).unwrap();
    assert!(part.symlink_metadata().is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"keep");
    // Nothing there is fine too.
    clear_part(&part).unwrap();
}

/// A stand-in container runtime: `-c` (create database) succeeds, the psql load fails.
fn fake_runtime(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-podman");
    std::fs::write(
        &path,
        "#!/bin/sh\ncase \" $* \" in *\" -c \"*) exit 0;; esac\ncat >/dev/null\nexit 3\n",
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn a_failed_airlines_load_names_psql_not_the_unzip_it_starved() {
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("demo.zip");
    std::fs::write(&zip, b"not a zip").unwrap();
    let rt = fake_runtime(dir.path());
    let err =
        xtask::db::load_download(rt.to_str().unwrap(), "c", &AIRLINES_SMALL, &[zip]).unwrap_err();
    assert!(format!("{err:#}").contains("loading airlines"), "{err:#}");
}
