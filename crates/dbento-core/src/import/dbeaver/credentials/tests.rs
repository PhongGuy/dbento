//! Decrypting DBeaver's credentials file and reading only users and passwords from it.
use super::*;
use crate::Secret;
use crate::import::dbeaver::DBeaverError;
use std::path::PathBuf;

const IV: [u8; 16] = [7; 16];

fn write(dir: &tempfile::TempDir, bytes: &[u8]) -> PathBuf {
    let path = dir.path().join("credentials-config.json");
    std::fs::write(&path, bytes).unwrap();
    path
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn a_file_made_by_openssl_decrypts_with_the_public_key() {
    // IV 000102...0f, then: printf '%s' '<plain>' | openssl enc -aes-128-cbc \
    //   -K babb4a9f774ab853c96c2d653dfe544a -iv 000102030405060708090a0b0c0d0e0f
    let bytes = hex(concat!(
        "000102030405060708090a0b0c0d0e0f24c611e8218e0a36116e0bcbe9d8aa3a",
        "9ed994f4a039df3cff3d00ff1694d2e51e208864b161106dbfd5e361a496f62d",
        "1ba65291b07f59042b26616dc1cf2a8c"
    ));
    let plain = decrypt(&bytes).expect("decrypts");
    assert_eq!(
        &plain[..],
        br##"{"id-0":{"#connection":{"user":"u0","password":"fake-pw-0"}}}"##
    );
    assert_eq!(encrypt(&plain, bytes[..16].try_into().unwrap()), bytes);
}

#[test]
fn users_and_passwords_are_read_per_connection_id() {
    let dir = tempfile::TempDir::new().unwrap();
    let plain = br##"{"pg-1":{"#connection":{"user":"clerk","password":"fake-pw-\"q\u00e9","auth-token":"fake-token"},"network/ssh_tunnel":{"user":"jump","password":"fake-ssh-pw","keyValue":"fake-ssh-key"}},"pg-2":{"#connection":{"user":"viewer"}},"pg-3":{"network/ssh_tunnel":{"user":"jump"}},"profile:p":{"#connection":{"user":"pu","password":"fake-pw-p"}}}"##;
    let creds = read_credentials(&write(&dir, &encrypt(plain, IV)), true).unwrap();
    let pg1 = &creds["pg-1"];
    assert_eq!(
        (
            pg1.user.as_deref(),
            pg1.password.as_ref().map(Secret::expose)
        ),
        (Some("clerk"), Some("fake-pw-\"q\u{e9}"))
    );
    assert_eq!(
        (
            creds["pg-2"].user.as_deref(),
            creds["pg-2"].password.is_none()
        ),
        (Some("viewer"), true)
    );
    assert!(!creds.contains_key("pg-3"));
    let text = format!("{creds:?}");
    assert!(!text.contains("fake-") && text.contains("clerk"), "{text}");
}

#[test]
fn without_secrets_no_password_is_kept() {
    let dir = tempfile::TempDir::new().unwrap();
    let plain = br##"{"pg-1":{"#connection":{"user":"clerk","password":"fake-pw-1"}}}"##;
    let creds = read_credentials(&write(&dir, &encrypt(plain, IV)), false).unwrap();
    assert_eq!(
        (
            creds["pg-1"].user.as_deref(),
            creds["pg-1"].password.is_none()
        ),
        (Some("clerk"), true)
    );
}

#[test]
fn a_missing_file_is_empty_and_a_bad_one_is_a_credentials_error() {
    let dir = tempfile::TempDir::new().unwrap();
    assert!(
        read_credentials(&dir.path().join("absent.json"), true)
            .unwrap()
            .is_empty()
    );
    let mut bad_padding = encrypt(b"{}", IV);
    let last = bad_padding.len() - 1;
    bad_padding[last] ^= 0xff;
    let cases: Vec<Vec<u8>> = vec![
        vec![1; 10],
        vec![1; 16],
        vec![1; 36],
        vec![1; 48],
        bad_padding,
        encrypt(b"fake-pw-9 is not json", IV),
        encrypt(b"[1,2]", IV),
        encrypt(br#"{"pg-1":"fake-pw-9"}"#, IV),
        encrypt(&[0xff, 0xfe], IV),
    ];
    for bytes in cases {
        let path = write(&dir, &bytes);
        let err = read_credentials(&path, true).unwrap_err();
        assert!(
            matches!(&err, DBeaverError::Credentials { path: p } if p == &path),
            "{err:?}"
        );
        assert!(!err.to_string().contains("fake-pw"), "{err}");
    }
}

#[test]
fn a_file_over_one_mebibyte_is_refused_before_decrypting() {
    let dir = tempfile::TempDir::new().unwrap();
    let body = |filler: usize| {
        let pad = "a".repeat(filler);
        format!(r##"{{"pg-1":{{"#connection":{{"user":"u"}}}},"pad":{{"x":"{pad}"}}}}"##)
    };
    let small = write(&dir, &encrypt(body((1 << 20) - 200).as_bytes(), IV));
    assert!(std::fs::metadata(&small).unwrap().len() <= MAX_CREDENTIALS_BYTES);
    assert!(read_credentials(&small, true).unwrap().contains_key("pg-1"));
    let big = write(&dir, &encrypt(body(1 << 20).as_bytes(), IV));
    assert!(matches!(
        read_credentials(&big, true),
        Err(DBeaverError::Credentials { .. })
    ));
}

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/dbeaver/modern/.dbeaver"
);
const MAIN_PLAIN: &str = r##"{"postgres-jdbc-a1":{"#connection":{"user":"orders_app","password":"fake-pw-1"}},"postgres-jdbc-b2":{"#connection":{"user":"reporter","password":"fake-pw-2"}},"postgres-jdbc-f6":{"#connection":{"user":"tunnel_user","password":"fake-pw-6"},"network/ssh_tunnel":{"user":"jump","password":"fake-ssh-pw","keyValue":"fake-ssh-key"}},"postgres-jdbc-g7":{"#connection":{"user":"proxy_user"}},"profile:fixture-profile":{"#connection":{"user":"profile_user","password":"fake-pw-profile"}}}"##;
const EXTRA_PLAIN: &str = r##"{"postgres-jdbc-i9":{"#connection":{"user":"orders_ro","password":"fake-pw-9"}},"postgres-jdbc-a1":{"#connection":{"user":"shadow_user","password":"fake-pw-shadow"}}}"##;

#[test]
fn the_committed_fixture_files_decrypt_to_their_fake_plaintext() {
    let iv1: [u8; 16] = std::array::from_fn(|i| 15 - i as u8);
    let iv2: [u8; 16] = std::array::from_fn(|i| 0x11 * i as u8);
    for (file, plain, iv) in [
        ("credentials-config.json", MAIN_PLAIN, iv1),
        ("credentials-config-extra.json", EXTRA_PLAIN, iv2),
    ] {
        let bytes = std::fs::read(std::path::Path::new(FIXTURE).join(file)).unwrap();
        assert_eq!(&bytes[..16], &iv, "{file}");
        assert_eq!(&decrypt(&bytes).unwrap()[..], plain.as_bytes(), "{file}");
        assert_eq!(encrypt(plain.as_bytes(), iv), bytes, "{file}");
    }
}
