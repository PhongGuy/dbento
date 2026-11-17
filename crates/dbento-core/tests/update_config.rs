//! The update feed: compiled constants, the build config and its URL rules, and the trusted key.
#![cfg(feature = "update")]

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use dbento_core::update::{
    CheckError, DOWNLOAD_PAGE, MANIFEST_URL, Product, Timeouts, build_config, build_config_with,
    release_notes_url, target_id,
};
use std::time::Duration;

const PAGE: &str = "https://example.com/download";

#[test]
fn the_feed_points_at_the_public_repository() {
    assert_eq!(
        MANIFEST_URL,
        "https://github.com/PhongGuy/dbento/releases/latest/download/latest.json"
    );
    assert_eq!(
        DOWNLOAD_PAGE,
        "https://github.com/PhongGuy/dbento/releases/latest"
    );
    assert_eq!(
        release_notes_url("0.2.0"),
        "https://github.com/PhongGuy/dbento/releases/tag/v0.2.0"
    );
}

#[test]
fn target_ids_name_product_os_and_arch() {
    let id = target_id(Product::App).expect("development hosts are release targets");
    let parts: Vec<&str> = id.splitn(3, '-').collect();
    assert_eq!(parts[0], "app");
    assert!(["linux", "macos", "windows"].contains(&parts[1]), "{id}");
    assert_eq!(parts[2], std::env::consts::ARCH);
    assert!(target_id(Product::Cli).unwrap().starts_with("cli-"));
}

#[test]
fn any_missing_input_disables_updates() {
    let url = Some("https://e.com/l.json");
    let try_ = |u: Option<&str>, p: Option<&str>, k: &str| {
        build_config_with(Product::App, u, p, k, "0.1.0")
    };
    assert_eq!(
        try_(None, Some(PAGE), "k").unwrap_err(),
        CheckError::Disabled
    );
    assert_eq!(
        try_(Some(""), Some(PAGE), "k").unwrap_err(),
        CheckError::Disabled
    );
    assert_eq!(try_(url, None, "k").unwrap_err(), CheckError::Disabled);
    assert_eq!(try_(url, Some(""), "k").unwrap_err(), CheckError::Disabled);
    assert_eq!(try_(url, Some(PAGE), "").unwrap_err(), CheckError::Disabled);
    assert_eq!(
        try_(url, Some(PAGE), "# comment only\n\n").unwrap_err(),
        CheckError::Disabled
    );
}

#[test]
fn a_config_keeps_its_inputs_and_default_timeouts() {
    let c = build_config_with(
        Product::App,
        Some("https://e.com/l.json"),
        Some(PAGE),
        "# k\nkey-one\n  key-two  \n",
        "0.3.0",
    )
    .unwrap();
    assert_eq!(c.manifest_url(), "https://e.com/l.json");
    assert_eq!(c.download_page(), PAGE);
    assert_eq!(c.keys(), ["key-one", "key-two"]);
    assert_eq!(c.current_version(), "0.3.0");
    assert_eq!(Some(c.target_id().to_owned()), target_id(Product::App));
    let t = c.timeouts();
    assert_eq!(
        (t.connect, t.manifest, t.artifact),
        (
            Duration::from_secs(10),
            Duration::from_secs(30),
            Duration::from_secs(15 * 60)
        )
    );
    let mut short = Timeouts::default();
    short.manifest = Duration::from_millis(200);
    assert_eq!(
        c.with_timeouts(short).timeouts().manifest,
        Duration::from_millis(200)
    );
}

#[test]
fn a_plain_http_manifest_url_is_refused_unless_loopback() {
    let r = build_config_with(
        Product::Cli,
        Some("http://example.com/l.json"),
        Some(PAGE),
        "k",
        "0.1.0",
    );
    assert!(matches!(r, Err(CheckError::ManifestInvalid(_))), "{r:?}");
    for local in [
        "http://127.0.0.1:8080/l.json",
        "http://localhost/l.json",
        "http://[::1]:9/l.json",
    ] {
        let r = build_config_with(Product::Cli, Some(local), Some(PAGE), "k", "0.1.0");
        assert!(r.is_ok(), "{local}: {r:?}");
    }
}

#[test]
fn the_download_page_must_be_https() {
    let url = Some("https://e.com/l.json");
    for page in [
        "http://example.com/download",
        "ftp://example.com/",
        "not a url",
    ] {
        let r = build_config_with(Product::App, url, Some(page), "k", "0.1.0");
        assert!(
            matches!(r, Err(CheckError::ManifestInvalid(_))),
            "{page}: {r:?}"
        );
    }
    // A loopback page serves local runs only, so only debug builds accept it.
    let r = build_config_with(
        Product::App,
        url,
        Some("http://127.0.0.1:8000/"),
        "k",
        "0.1.0",
    );
    if cfg!(debug_assertions) {
        assert!(r.is_ok(), "{r:?}");
    } else {
        assert!(matches!(r, Err(CheckError::ManifestInvalid(_))), "{r:?}");
    }
}

#[test]
fn a_debug_build_without_the_override_is_disabled() {
    if cfg!(debug_assertions) && std::env::var_os("DBENTO_UPDATE_MANIFEST_URL").is_none() {
        assert_eq!(
            build_config(Product::Cli).unwrap_err(),
            CheckError::Disabled
        );
    }
}

#[test]
fn keys_txt_holds_the_release_key() {
    let text = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../assets/update/keys.txt"
    ));
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    assert_eq!(lines.len(), 1, "{text}");
    let decoded = String::from_utf8(STANDARD.decode(lines[0]).unwrap()).unwrap();
    assert!(
        decoded.starts_with("untrusted comment: minisign public key: 40786E97EA264BC5\n"),
        "{decoded}"
    );
    minisign_verify::PublicKey::decode(&decoded).unwrap();
}
