//! S3 through the CLI: `dbento query` lists, `dbento download` writes files safely.
#![cfg(feature = "s3")]
use crate::common::{dbento, write_tree};
use dbento_testkit::{S3_SECRET, S3_SECRET_VAR, S3Moto, big_byte, s3_moto};

fn workspace(s3: &S3Moto) -> tempfile::TempDir {
    let toml = format!(
        "dbento = 1\n{}\n[connections.lite]\nengine = \"sqlite\"\npath = \"x.db\"\n",
        s3.connection_toml("s3", "")
    );
    write_tree(&[("dbento.toml", toml.as_str())])
}

fn run(ws: &tempfile::TempDir, args: &[&str]) -> (i32, String, String) {
    let out = dbento()
        .current_dir(ws.path())
        .args(args)
        .env(S3_SECRET_VAR, S3_SECRET)
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[tokio::test]
async fn query_lists_a_prefix_as_json() {
    let s3 = s3_moto().await;
    let ws = workspace(&s3);
    let (code, out, err) = run(
        &ws,
        &[
            "query",
            "s3",
            &format!("ls 's3://{}/text/'", s3.bucket()),
            "--format",
            "json",
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("text/hello.txt"), "{out}");
}

#[tokio::test]
async fn download_writes_the_object_and_refuses_to_overwrite_without_force() {
    let s3 = s3_moto().await;
    let ws = workspace(&s3);
    let url = format!("s3://{}/big/three-mib.bin", s3.bucket());
    let (code, _, err) = run(&ws, &["download", "s3", &url, "-o", "big.bin"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("3145728 bytes"), "{err}");
    let got = std::fs::read(ws.path().join("big.bin")).unwrap();
    assert!(got.len() == 3 << 20 && got.iter().enumerate().all(|(i, b)| *b == big_byte(i)));
    std::fs::write(ws.path().join("big.bin"), b"mine").unwrap();
    let (code, _, err) = run(&ws, &["download", "s3", &url, "-o", "big.bin"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("--force"), "{err}");
    assert_eq!(std::fs::read(ws.path().join("big.bin")).unwrap(), b"mine");
    let (code, _, err) = run(&ws, &["download", "s3", &url, "-o", "big.bin", "--force"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        std::fs::metadata(ws.path().join("big.bin")).unwrap().len(),
        3 << 20
    );
}

#[tokio::test]
async fn download_refuses_above_max_size_and_leaves_no_file() {
    let s3 = s3_moto().await;
    let ws = workspace(&s3);
    let url = format!("s3://{}/big/three-mib.bin", s3.bucket());
    let (code, _, err) = run(
        &ws,
        &[
            "download",
            "s3",
            &url,
            "-o",
            "big.bin",
            "--max-size",
            "1MiB",
        ],
    );
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("--max-size"), "{err}");
    let left: Vec<_> = std::fs::read_dir(ws.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(
        !left.iter().any(|n| n.to_string_lossy().contains("big.bin")),
        "{left:?}"
    );
}

#[tokio::test]
async fn download_names_hostile_keys_escaped() {
    let s3 = s3_moto().await;
    let ws = workspace(&s3);
    let url = format!("s3://{}/hostile/bidi\u{202e}txt.exe", s3.bucket());
    let (code, _, err) = run(&ws, &["download", "s3", &url, "-o", "safe.bin"]);
    assert_eq!(code, 0, "{err}");
    assert!(!err.contains('\u{202e}'), "{err:?}");
    assert!(ws.path().join("safe.bin").exists());
}

#[test]
fn a_bad_url_or_a_non_s3_connection_is_a_usage_error() {
    let ws = write_tree(&[(
        "dbento.toml",
        "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"x.db\"\n",
    )]);
    let out = dbento()
        .current_dir(ws.path())
        .args(["download", "lite", "s3://b/k", "-o", "x"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let out = dbento()
        .current_dir(ws.path())
        .args(["download", "lite", "http://b/k", "-o", "x"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
}
