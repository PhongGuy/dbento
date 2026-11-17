//! The update banner and the Settings toggle in the running app, against a loopback feed
//! signed with a throwaway key. Never clicks Release notes or Download page, which would open
//! a real browser.
mod common;

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::TcpListener;
use std::path::Path;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use common::{Pilot, find_node, launch_args, sqlite_workspace};
use dbento_core::update::{Product, target_id};
use serde_json::{Value, json};

/// Serves `files` (path, body) over plain HTTP on a loopback port for the rest of the test,
/// one request per connection.
fn serve(files: Vec<(&'static str, Vec<u8>)>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            if reader.read_line(&mut request).is_err() {
                continue;
            }
            // Read through the blank line so the client sees a whole exchange.
            loop {
                let mut header = String::new();
                if !matches!(reader.read_line(&mut header), Ok(n) if n > 0) || header == "\r\n" {
                    break;
                }
            }
            let at = request.split_whitespace().nth(1).unwrap_or("");
            let reply = match files.iter().find(|(p, _)| *p == at) {
                Some((_, body)) => {
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    [head.into_bytes(), body.clone()].concat()
                }
                None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_vec(),
            };
            let _ = stream.write_all(&reply);
        }
    });
    port
}

/// A 99.0.0 manifest for this host's app, its signature and the key line, served on loopback.
fn feed() -> (u16, String) {
    let key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let probe = TcpListener::bind("127.0.0.1:0").unwrap();
    let artifact_port = probe.local_addr().unwrap().port();
    drop(probe);
    let manifest = format!(
        r#"{{"version":"99.0.0","targets":{{"{}":{{"url":"http://127.0.0.1:{artifact_port}/artifact","format":"appimage","size":1,"sha256":"{}"}}}}}}"#,
        target_id(Product::App).unwrap(),
        "0".repeat(64)
    );
    let trusted = Some("timestamp:0\tfile:latest.json");
    let sig = minisign::sign(
        Some(&key.pk),
        &key.sk,
        std::io::Cursor::new(manifest.as_bytes()),
        trusted,
        None,
    )
    .unwrap();
    let port = serve(vec![
        ("/latest.json", manifest.into_bytes()),
        (
            "/latest.json.sig",
            STANDARD.encode(sig.to_string()).into_bytes(),
        ),
    ]);
    (port, STANDARD.encode(key.pk.to_box().unwrap().to_string()))
}

async fn until(pilot: &Pilot, what: &str, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let tree = pilot.ok("tree", json!({})).await;
        if done(&tree) {
            return tree;
        }
        assert!(Instant::now() < deadline, "{what} never happened: {tree}");
        pilot.ok("wait", json!({ "ms": 50 })).await;
    }
}
async fn click(pilot: &Pilot, id: &str) {
    pilot.ok("click", json!({ "target": { "id": [id] } })).await;
}
fn name<'a>(tree: &'a Value, id: &str) -> Option<&'a str> {
    find_node(tree, id).and_then(|n| n["name"].as_str())
}
/// Polls `file` until its JSON satisfies `ok`.
fn wait_file(file: &Path, what: &str, ok: impl Fn(&Value) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let saved = std::fs::read_to_string(file)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok());
        if saved.as_ref().is_some_and(&ok) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{what} never reached {}: {saved:?}",
            file.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_banner_skip_and_the_settings_toggle() {
    let (port, key_line) = feed();
    let (ws, state) = (sqlite_workspace(), tempfile::tempdir().unwrap());
    let pilot = Pilot::start().await;
    let mut args = launch_args("headless", ws.path());
    args["env"] = json!({
        "DBENTO_STATE_DIR": state.path(),
        "DBENTO_UPDATE_MANIFEST_URL": format!("http://127.0.0.1:{port}/latest.json"),
        "DBENTO_TEST_UPDATE_KEYS": key_line,
    });
    pilot.ok("launch", args).await;

    let tree = until(&pilot, "the banner", |t| {
        find_node(t, "update-banner").is_some()
    })
    .await;
    assert_eq!(
        name(&tree, "update-banner-text"),
        Some("Dbento 99.0.0 is available")
    );
    for id in [
        "update-notes",
        "update-download-page",
        "update-later",
        "update-skip",
    ] {
        assert!(find_node(&tree, id).is_some(), "{id} missing: {tree}");
    }
    assert!(
        find_node(&tree, "update-install").is_none(),
        "a source build offered to install"
    );
    if let Some(dir) = std::env::var_os("DBENTO_SHOTS_DIR").map(std::path::PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        pilot
            .ok(
                "screenshot",
                json!({ "save_to": dir.join("update-banner.png") }),
            )
            .await;
    }

    click(&pilot, "update-skip").await;
    until(&pilot, "the banner to go", |t| {
        find_node(t, "update-banner").is_none()
    })
    .await;
    wait_file(&state.path().join("update-state.json"), "the skip", |s| {
        s["skipped_version"] == json!("99.0.0")
    });

    pilot.ok("key", json!({ "keys": "ctrl-," })).await;
    until(&pilot, "the dialog", |t| {
        find_node(t, "settings-dialog").is_some()
    })
    .await;
    click(&pilot, "settings-check-updates").await;
    wait_file(&state.path().join("settings.json"), "the toggle", |s| {
        s["check_for_updates"] == json!(false)
    });
    click(&pilot, "settings-check-now").await;
    until(&pilot, "the manual report", |t| {
        name(t, "settings-update-status") == Some("Dbento 99.0.0 is available.")
    })
    .await;
    pilot.assert_bus_untouched();
}
