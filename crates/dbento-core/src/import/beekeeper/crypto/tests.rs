//! simple-encryptor decryption, the `.key` unwrap, and the test-only encrypt helper.
// `super::*` brings in `Hmac`, `KeyInit`, `Mac`, `Digest`, `Sha256` and the base64 `Engine`.
use super::*;
use crate::import::beekeeper::BeekeeperError;
use tempfile::TempDir;

/// The column key the fixture `.key` wraps.
pub(crate) const FIXTURE_KEY: &str =
    "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";
const FIXTURE_DOT_KEY: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/beekeeper/.key"
));
// `"fake-pw-reports-é"` under FIXTURE_KEY with IV [2; 16], made with the openssl recipe.
const REPORTS_PW: &str = "6e58e25413ec33f23fab65bf19a0cdae0919a899b0040463130caebaa6f9eca102020202020202020202020202020202/ELOx60NvLtbs5NNL9G96WMGHEsSGXAjMXmajK8t02o=";

/// `hex(HMAC-SHA256(SHA-256(key), body)) + body`, for bodies the helper cannot make.
fn sign(key: &str, body: &str) -> String {
    let k = Sha256::digest(key.as_bytes());
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&k).unwrap();
    mac.update(body.as_bytes());
    format!("{}{body}", hex::encode(mac.finalize().into_bytes()))
}

fn key_file(contents: &[u8]) -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(".key");
    std::fs::write(&path, contents).unwrap();
    (dir, path)
}

#[test]
fn the_fixture_key_unwraps_and_a_column_decrypts_to_its_json_string() {
    let key = unwrap_key(FIXTURE_DOT_KEY.trim()).expect("unwraps");
    assert_eq!(key.as_str(), FIXTURE_KEY);
    let pw = decrypt_string(&key, REPORTS_PW).expect("decrypts");
    assert_eq!(pw.expose(), "fake-pw-reports-é");
}

#[test]
fn the_helper_reproduces_the_openssl_ciphertext() {
    let plain = "\"fake-pw-reports-é\"".as_bytes();
    assert_eq!(encrypt(FIXTURE_KEY, plain, [2; 16]), REPORTS_PW);
    let wrap = format!(r#"{{"encryptionKey":"{FIXTURE_KEY}"}}"#);
    assert_eq!(
        encrypt(WRAP_KEY, wrap.as_bytes(), [0; 16]),
        FIXTURE_DOT_KEY.trim()
    );
}

#[test]
fn a_changed_hmac_iv_or_body_or_a_wrong_key_is_refused() {
    let flip = |i: usize| {
        let mut b = REPORTS_PW.as_bytes().to_vec();
        b[i] = if b[i] == b'0' { b'1' } else { b'0' };
        String::from_utf8(b).unwrap()
    };
    for bad in [flip(0), flip(64), flip(REPORTS_PW.len() - 3)] {
        assert!(decrypt(FIXTURE_KEY, &bad).is_none(), "{bad}");
    }
    assert!(decrypt("another key", REPORTS_PW).is_none());
}

#[test]
fn malformed_text_is_refused_without_panicking() {
    let iv = "00".repeat(16);
    let short_body = format!(
        "{iv}{}",
        base64::engine::general_purpose::STANDARD.encode([1u8; 5])
    );
    let cases = vec![
        String::new(),
        "short".to_owned(),
        "z".repeat(64),
        format!("{}é", &REPORTS_PW[..100]),
        sign(FIXTURE_KEY, &short_body),
        sign(FIXTURE_KEY, &format!("{iv}not base64!")),
        sign(
            FIXTURE_KEY,
            &format!("{}{}", "zz".repeat(16), "AAAAAAAAAAAAAAAAAAAAAA=="),
        ),
        sign(FIXTURE_KEY, "zz"),
    ];
    for bad in &cases {
        assert!(decrypt(FIXTURE_KEY, bad).is_none(), "{bad:?}");
    }
}

#[test]
fn a_plaintext_that_is_not_a_json_string_is_no_secret() {
    for plain in [
        &b"42"[..],
        b"null",
        b"{\"a\":1}",
        b"not json",
        b"\"unterminated",
    ] {
        let c = encrypt(FIXTURE_KEY, plain, [3; 16]);
        assert!(decrypt(FIXTURE_KEY, &c).is_some());
        assert!(decrypt_string(FIXTURE_KEY, &c).is_none(), "{plain:?}");
    }
}

#[test]
fn read_key_trims_the_file_and_a_missing_file_is_none() {
    let (_d, path) = key_file(FIXTURE_DOT_KEY.as_bytes());
    let key = read_key(&path).unwrap();
    assert_eq!(key.as_deref().map(String::as_str), Some(FIXTURE_KEY));
    assert!(read_key(&path.with_file_name("absent")).unwrap().is_none());
}

#[test]
fn a_key_file_that_is_too_large_not_ciphertext_or_not_the_json_shape_is_refused() {
    let inner = format!(r#"{{"encryptionKey":"{FIXTURE_KEY}"}}"#);
    let cases = [
        b"garbage-key-content".to_vec(),
        encrypt(WRAP_KEY, br#"{"other":"x"}"#, [0; 16]).into_bytes(),
        encrypt(WRAP_KEY, br#"{"encryptionKey":7}"#, [0; 16]).into_bytes(),
        encrypt("not the constant", inner.as_bytes(), [0; 16]).into_bytes(),
        vec![b'a'; (MAX_KEY_BYTES + 1) as usize],
    ];
    for contents in cases {
        let (_d, path) = key_file(&contents);
        match read_key(&path) {
            Err(e @ BeekeeperError::Key { .. }) => {
                let text = format!("{e} {e:?}");
                assert!(text.contains(".key"), "{text}");
                assert!(
                    !text.contains("garbage") && !text.contains(FIXTURE_KEY),
                    "{text}"
                );
            }
            other => panic!("{:?}", other.map(|k| k.is_some())),
        }
    }
}

#[test]
fn the_fixture_ciphertext_is_what_the_helper_makes() {
    let modern = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/beekeeper/modern.sql"
    ));
    let old = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/beekeeper/old.sql"
    ));
    for (iv, pw) in [
        (0x01, "fake-pw-orders"),
        (0x02, "fake-pw-reports-é"),
        (0x04, "fake-pw-certs"),
        (0x05, "fake-pw-iam"),
        (0x09, "fake-pw-tunnel"),
        (0x99, "fake-ssh-pw"),
        (0x0d, "fake-pw-orders-2"),
    ] {
        let c = encrypt(FIXTURE_KEY, &serde_json::to_vec(pw).unwrap(), [iv; 16]);
        assert!(modern.contains(&c), "{pw}");
    }
    let good = encrypt(FIXTURE_KEY, br#""fake-pw-broken""#, [0x0b; 16]);
    assert!(good.starts_with('5') && modern.contains(&format!("6{}", &good[1..])));
    assert!(old.contains(&encrypt(FIXTURE_KEY, br#""fake-pw-legacy""#, [0x21; 16])));
}
